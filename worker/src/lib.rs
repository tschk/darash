//! darash-site worker — Rust (workers-rs 0.8).
//!
//! Asset-first routing: static files (`../site`) are served by Cloudflare's
//! assets pipeline directly; this worker handles `/api/*` (and anything that
//! doesn't match an asset, which gets a JSON 404).
//!
//! Tiers:
//!   - anonymous: per-IP limits (30/h, 100/day) on search/fetch; batch, async
//!     jobs, and research need a key
//!   - free key: unmetered search/fetch + batch + async jobs
//!   - pro key: everything above + higher batch cap + /api/research
//!
//! Health, stats, and the live stream stay unmetered and keyless.

mod accounts;
mod counter;
mod extract;
mod jobs;
mod search;

use serde_json::json;
use worker::*;

use accounts::{Account, Tier};

pub use accounts::Accounts as AccountsDo;
pub use counter::Counter as CounterDo;

const COUNTER: &str = "COUNTER";
const ACCOUNTS: &str = "ACCOUNTS";
const ADMIN_SECRET: &str = "ADMIN_SECRET";
const CLIENT_SALT: &str = "darash-live-salt";

use accounts::{ANON_DAILY, ANON_HOURLY};

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let started = now_ms();
    let url = req.url()?;
    let method = req.method().to_string();
    let path = url.path().to_string();
    let client = client_pseudonym(&req).await;
    let query = url.query().map(str::to_string);

    let routed = match route(req, env.clone(), ctx, &url, &path).await {
        Ok(routed) => routed,
        Err(error) => Routed {
            response: Response::from_json(&json!({ "error": error.to_string() }))?.with_status(500),
            tier: "anon".to_string(),
        },
    };

    // Log every API request into the Counter DO.
    let ms = now_ms().saturating_sub(started);
    let mut event_path = path.clone();
    if let Some(query) = &query {
        event_path = format!("{path}{}", form_urlencoded_pairs(query));
    }
    let kind = if path.starts_with("/api/search") {
        "search"
    } else if path.starts_with("/api/fetch")
        || path.starts_with("/api/batch")
        || path.starts_with("/api/async")
        || path.starts_with("/api/research")
    {
        "fetch"
    } else {
        "other"
    };
    let event = json!({
        "id": uuid_v4(),
        "ts": now_ms(),
        "method": method,
        "path": event_path,
        "kind": kind,
        "status": routed.response.status_code(),
        "ms": ms,
        "client": client,
        "tier": routed.tier,
    });
    let counter = counter_stub(&env)?;
    let _ = stub_post_json(&counter, "https://counter/add", event).await;

    Ok(routed.response)
}

/// Outcome of routing: the response plus the resolved tier for the event log.
pub(crate) struct Routed {
    pub response: Response,
    pub tier: String,
}

fn ok_json(value: &serde_json::Value) -> Result<Response> {
    Response::from_json(value)
}

async fn route(req: Request, env: Env, ctx: Context, url: &Url, path: &str) -> Result<Routed> {
    let method = req.method();

    match (method, path) {
        (Method::Get, "/api/health") => Ok(Routed {
            response: ok_json(&json!({ "ok": true }))?,
            tier: "anon".into(),
        }),
        (Method::Get, "/api/stats") => {
            let request = Request::new("https://counter/state", Method::Get)?;
            let response = counter_stub(&env)?.fetch_with_request(request).await?;
            Ok(Routed {
                response,
                tier: "anon".into(),
            })
        }
        (Method::Get, "/api/live") => {
            // Stub fetch forwards the raw request, so build a fresh one aimed
            // at the DO's /stream route with the client's upgrade headers.
            let mut request = Request::new("https://counter/stream", Method::Get)?;
            {
                let headers = request.headers_mut()?;
                for name in [
                    "connection",
                    "upgrade",
                    "sec-websocket-key",
                    "sec-websocket-version",
                    "sec-websocket-protocol",
                    "sec-websocket-extensions",
                ] {
                    if let Ok(Some(value)) = req.headers().get(name) {
                        let _ = headers.set(name, &value);
                    }
                }
            }
            let response = counter_stub(&env)?.fetch_with_request(request).await?;
            Ok(Routed {
                response,
                tier: "anon".into(),
            })
        }
        (Method::Post, "/api/admin/keys") => admin_create(req, &env).await,
        (Method::Get, "/api/admin/keys") => admin_list(req, &env).await,
        (Method::Post, "/api/admin/keys/revoke") => admin_revoke(req, &env).await,
        (Method::Get, "/api/account") => account_snapshot(req, &env, url).await,
        (Method::Get, p) if p.starts_with("/api/async/jobs/") => {
            let id = p.trim_start_matches("/api/async/jobs/");
            let job = jobs::status(&env, id).await?;
            match job {
                Some(job) => Ok(Routed {
                    response: ok_json(&job)?,
                    tier: "anon".into(),
                }),
                None => Ok(Routed {
                    response: ok_json(&json!({ "error": "unknown job" }))?.with_status(404),
                    tier: "anon".into(),
                }),
            }
        }
        (method, path)
            if path.starts_with("/api/search")
                || path.starts_with("/api/fetch")
                || path.starts_with("/api/batch")
                || path.starts_with("/api/async/jobs")
                || path.starts_with("/api/research") =>
        {
            metered(method, req, env, ctx, url, path).await
        }
        // Non-API paths that fall through the assets pipeline (404s etc.).
        _ => Ok(Routed {
            response: ok_json(&json!({ "error": "not found" }))?.with_status(404),
            tier: "anon".into(),
        }),
    }
}

/// Metered API routes: resolve the key, enforce anon limits only, dispatch.
async fn metered(
    method: Method,
    req: Request,
    env: Env,
    ctx: Context,
    url: &Url,
    path: &str,
) -> Result<Routed> {
    let key = extract_key(&req, url);
    let account: Option<Account> = match &key {
        Some(raw) => match accounts::lookup(&env, raw).await? {
            Some(account) => Some(account),
            None => return invalid_key(),
        },
        None => None,
    };
    let tier = account.as_ref().map(|a| a.tier).unwrap_or(Tier::Free);
    let keyed = account.is_some();

    let identity = match &account {
        Some(account) => format!("key:{}", account.key_hash),
        None => format!("anon:{}", client_pseudonym(&req).await),
    };

    if account.is_none() {
        let decision = accounts::check_rate(&env, &identity).await?;
        if !decision.allowed {
            let headers = Headers::new();
            headers.set("retry-after", &decision.retry_after_secs.to_string())?;
            return Ok(Routed {
                response: ok_json(&json!({
                    "error": "rate limit exceeded",
                    "tier": "anon",
                    "limits": { "hour": ANON_HOURLY, "day": ANON_DAILY },
                    "retryAfter": decision.retry_after_secs,
                }))?
                .with_status(429)
                .with_headers(headers),
                tier: "anon".into(),
            });
        }
    }

    let response = match (method, path) {
        (Method::Get, "/api/search") => search::handle(&req, &env).await?,
        (Method::Get, "/api/fetch") => extract::handle_fetch(&req).await?,
        (Method::Post, "/api/batch") => {
            if !keyed {
                return keyed_required("batch");
            }
            extract::handle_batch(req, tier).await?
        }
        (Method::Post, "/api/async/jobs") => {
            if !keyed {
                return keyed_required("async jobs");
            }
            return jobs::create(req, &env, ctx, tier).await;
        }
        (Method::Post, "/api/research") => {
            if tier != Tier::Pro {
                return Ok(Routed {
                    response: ok_json(&json!({
                        "error": "research is a pro feature — needs a pro key",
                        "tier": tier.as_str(),
                    }))?
                    .with_status(402),
                    tier: tier.as_str().to_string(),
                });
            }
            jobs::research(req, &env).await?
        }
        _ => ok_json(&json!({ "error": "not found" }))?.with_status(404),
    };

    // Errors don't burn quota; successful calls record usage (informational
    // for keyed users — enforced only for anon).
    if response.status_code() < 400 {
        let _ = accounts::record(&env, &identity).await;
    }
    Ok(Routed {
        response,
        tier: tier.as_str().to_string(),
    })
}

// ---------------------------------------------------------------------------
// Admin + account routes
// ---------------------------------------------------------------------------

async fn account_snapshot(req: Request, env: &Env, url: &Url) -> Result<Routed> {
    let Some(raw) = extract_key(&req, url) else {
        return Ok(Routed {
            response: ok_json(&json!({
                "error": "api key required (Authorization: Bearer <key>)"
            }))?
            .with_status(401),
            tier: "anon".into(),
        });
    };
    let Some(account) = accounts::lookup(env, &raw).await? else {
        return invalid_key();
    };
    let identity = format!("key:{}", account.key_hash);
    let usage = accounts::snapshot(env, &identity, &account.tier).await?;
    Ok(Routed {
        response: ok_json(&json!({
            "tier": account.tier.as_str(),
            "name": account.name,
            "usage": usage,
        }))?,
        tier: account.tier.as_str().to_string(),
    })
}

async fn admin_authorized(req: &Request, env: &Env) -> Result<bool> {
    let Ok(secret) = env.secret(ADMIN_SECRET) else {
        return Ok(false);
    };
    let header = req.headers().get("authorization")?.unwrap_or_default();
    Ok(header == format!("Bearer {}", secret.to_string()))
}

async fn admin_create(mut req: Request, env: &Env) -> Result<Routed> {
    if !admin_authorized(&req, &env).await? {
        return unauthorized();
    }
    let body: serde_json::Value = serde_json::from_str(&req.text().await?)?;
    let tier = match body.get("tier").and_then(|t| t.as_str()) {
        Some("pro") => Tier::Pro,
        _ => Tier::Free,
    };
    let name = body
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or_default()
        .to_string();
    let created = accounts::create_key(&env, tier, name).await?;
    Ok(Routed {
        response: ok_json(&json!({
            "key": created.raw_key,
            "tier": created.account.tier.as_str(),
            "name": created.account.name,
            "keyHash": created.account.key_hash,
        }))?
        .with_status(201),
        tier: "anon".into(),
    })
}

async fn admin_list(req: Request, env: &Env) -> Result<Routed> {
    if !admin_authorized(&req, &env).await? {
        return unauthorized();
    }
    let keys = accounts::list_keys(&env).await?;
    Ok(Routed {
        response: ok_json(&json!({ "keys": keys }))?,
        tier: "anon".into(),
    })
}

async fn admin_revoke(mut req: Request, env: &Env) -> Result<Routed> {
    if !admin_authorized(&req, &env).await? {
        return unauthorized();
    }
    let body: serde_json::Value = serde_json::from_str(&req.text().await?)?;
    let Some(key) = body.get("key").and_then(|k| k.as_str()) else {
        return Ok(Routed {
            response: ok_json(&json!({ "error": "key is required" }))?.with_status(400),
            tier: "anon".into(),
        });
    };
    let revoked = accounts::revoke_key(&env, key).await?;
    Ok(Routed {
        response: ok_json(&json!({ "revoked": revoked }))?,
        tier: "anon".into(),
    })
}

fn unauthorized() -> Result<Routed> {
    Ok(Routed {
        response: ok_json(&json!({ "error": "unauthorized" }))?.with_status(401),
        tier: "anon".into(),
    })
}

fn invalid_key() -> Result<Routed> {
    Ok(Routed {
        response: ok_json(&json!({ "error": "invalid api key" }))?.with_status(401),
        tier: "anon".into(),
    })
}

fn keyed_required(feature: &str) -> Result<Routed> {
    Ok(Routed {
        response: ok_json(&json!({
            "error": format!("{feature} requires an api key (free tier and up)"),
        }))?
        .with_status(401),
        tier: "anon".into(),
    })
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn extract_key(req: &Request, url: &Url) -> Option<String> {
    if let Ok(Some(header)) = req.headers().get("authorization") {
        if let Some(raw) = header.strip_prefix("Bearer dk_") {
            return Some(format!("dk_{raw}"));
        }
    }
    let query = url.query_pairs().find(|(name, _)| name == "key");
    match query {
        Some((_, value)) if value.starts_with("dk_") => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn counter_stub(env: &Env) -> Result<Stub> {
    env.durable_object(COUNTER)?
        .id_from_name("global")?
        .get_stub()
}

pub(crate) fn accounts_stub(env: &Env) -> Result<Stub> {
    env.durable_object(ACCOUNTS)?
        .id_from_name("global")?
        .get_stub()
}

/// POST JSON to a DO stub and parse the JSON response.
pub(crate) async fn stub_post_json(
    stub: &Stub,
    url: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value> {
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(worker::wasm_bindgen::JsValue::from_str(
            &serde_json::to_string(&body)?,
        )));
    let request = Request::new_with_init(url, &init)?;
    let mut response = stub.fetch_with_request(request).await?;
    response.json().await
}

pub(crate) async fn stub_get_json(stub: &Stub, url: &str) -> Result<serde_json::Value> {
    let request = Request::new(url, Method::Get)?;
    let mut response = stub.fetch_with_request(request).await?;
    response.json().await
}

/// GET JSON from the internet with a timeout.
pub(crate) async fn http_get_json(url: &str, timeout_ms: u64) -> Result<serde_json::Value> {
    let text = http_get(url, timeout_ms, "application/json").await?;
    serde_json::from_str(&text)
        .map_err(|error| Error::RustError(format!("invalid JSON from {url}: {error}")))
}

/// GET text from the internet with a timeout and size cap.
pub(crate) async fn http_get(url: &str, timeout_ms: u64, accept: &str) -> Result<String> {
    let mut request = Request::new(url, Method::Get)?;
    request.headers_mut()?.set("accept", accept)?;
    request
        .headers_mut()?
        .set("user-agent", search::USER_AGENT)?;
    let fetch = async { Fetch::Request(request).send().await };
    let timeout = timeout_future(timeout_ms);
    match futures_util::future::select(Box::pin(fetch), Box::pin(timeout)).await {
        futures_util::future::Either::Left((result, _)) => {
            let mut response = result?;
            let status = response.status_code();
            if !(200..300).contains(&status) {
                return Err(Error::RustError(format!("HTTP {status} from {url}")));
            }
            let text = response.text().await?;
            if text.len() > 512 * 1024 {
                return Err(Error::RustError(format!(
                    "response from {url} exceeded 512 KiB"
                )));
            }
            Ok(text)
        }
        futures_util::future::Either::Right(_) => Err(Error::RustError(format!(
            "timeout after {timeout_ms}ms: {url}"
        ))),
    }
}

async fn timeout_future(ms: u64) {
    gloo_timers::future::TimeoutFuture::new(ms as u32).await;
}

pub(crate) fn now_ms() -> u64 {
    worker::js_sys::Date::now() as u64
}

pub(crate) fn uuid_v4() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 16] = rng.gen();
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}{}{}{}-{}{}-4{}{}-8{}{}-{}{}{}{}{}{}",
        hex[0],
        hex[1],
        hex[2],
        hex[3],
        hex[4],
        hex[5],
        hex[6],
        hex[7],
        hex[8],
        hex[9],
        hex[10],
        hex[11],
        hex[12],
        hex[13],
        hex[14],
        hex[15],
    )
}

pub(crate) async fn client_pseudonym(req: &Request) -> String {
    let ip = req
        .headers()
        .get("cf-connecting-ip")
        .ok()
        .flatten()
        .or_else(|| req.headers().get("x-forwarded-for").ok().flatten())
        .unwrap_or_else(|| "local".to_string());
    short_hash(&format!("{ip}{CLIENT_SALT}"))
}

pub(crate) fn short_hash(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(value.as_bytes());
    digest[..4].iter().map(|b| format!("{b:02x}")).collect()
}

fn form_urlencoded_pairs(query: &str) -> String {
    let names: Vec<String> = query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let name = part.split('=').next().unwrap_or(part);
            format!("{name}=…")
        })
        .collect();
    format!("?{}", names.join("&"))
}
