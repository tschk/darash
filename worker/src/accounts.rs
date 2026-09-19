//! Accounts Durable Object: API keys, anon rate-limit windows, async jobs.
//!
//! One global instance. Keys are stored as SHA-256 hashes; the raw key is
//! returned once at creation. Rate limits are enforced ONLY for anonymous
//! identities (`anon:<pseudonym>`); keyed users are tracked for usage display
//! but never limited.
//!
//! Durable Objects process one request at a time, so the get → check → put
//! sequences here are atomic.

use serde::{Deserialize, Serialize};
use serde_json::json;
use worker::*;

use crate::{stub_get_json, stub_post_json, uuid_v4};

pub(crate) const ANON_HOURLY: u64 = 30;
pub(crate) const ANON_DAILY: u64 = 100;
pub(crate) const MAX_RESEARCH_PAGES: usize = 5;

const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 86_400_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Free,
    Pro,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Free => "free",
            Tier::Pro => "pro",
        }
    }

    fn parse(value: &str) -> Option<Tier> {
        match value {
            "free" => Some(Tier::Free),
            "pro" => Some(Tier::Pro),
            _ => None,
        }
    }

    pub fn batch_cap(self) -> usize {
        match self {
            Tier::Free => 20,
            Tier::Pro => 45,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    // Aliases keep keys created by the previous TypeScript worker readable.
    #[serde(alias = "keyHash")]
    pub key_hash: String,
    pub tier: Tier,
    pub name: String,
    #[serde(alias = "createdAt")]
    pub created_at: u64,
    #[serde(alias = "revokedAt")]
    pub revoked_at: Option<u64>,
}

#[derive(Serialize)]
pub struct RateDecision {
    pub allowed: bool,
    pub hour_used: u64,
    pub day_used: u64,
    pub retry_after_secs: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub urls: Vec<String>,
    pub status: String,
    pub created_at: u64,
    pub results: serde_json::Value,
}

pub(crate) fn sha256_hex(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(value.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn hour_window(now: u64) -> (String, u64) {
    let hour = now / HOUR_MS;
    (format!("h{hour}"), (hour + 1) * HOUR_MS)
}

fn day_window(now: u64) -> (String, u64) {
    let day = now / DAY_MS;
    (format!("d{day}"), (day + 1) * DAY_MS)
}

// ---------------------------------------------------------------------------
// Worker-side helpers (call the DO over internal fetch)
// ---------------------------------------------------------------------------

pub(crate) async fn create_key(env: &Env, tier: Tier, name: String) -> Result<CreatedKey> {
    let response = stub_post_json(
        &crate::accounts_stub(env)?,
        "https://accounts/create",
        json!({ "tier": tier.as_str(), "name": name }),
    )
    .await?;
    Ok(CreatedKey {
        raw_key: response["key"].as_str().unwrap_or_default().to_string(),
        account: Account {
            key_hash: response["keyHash"].as_str().unwrap_or_default().to_string(),
            tier,
            name: response["name"].as_str().unwrap_or_default().to_string(),
            created_at: response["createdAt"].as_u64().unwrap_or_default(),
            revoked_at: None,
        },
    })
}

pub struct CreatedKey {
    pub raw_key: String,
    pub account: Account,
}

pub(crate) async fn lookup(env: &Env, raw_key: &str) -> Result<Option<Account>> {
    let response = stub_post_json(
        &crate::accounts_stub(env)?,
        "https://accounts/lookup",
        json!({ "key": raw_key }),
    )
    .await?;
    let found = response.get("account").is_some_and(|a| !a.is_null());
    if !found {
        return Ok(None);
    }
    let account = response["account"].clone();
    Ok(Some(Account {
        key_hash: account["keyHash"].as_str().unwrap_or_default().to_string(),
        tier: Tier::parse(account["tier"].as_str().unwrap_or("free")).unwrap_or(Tier::Free),
        name: account["name"].as_str().unwrap_or_default().to_string(),
        created_at: account["createdAt"].as_u64().unwrap_or_default(),
        revoked_at: None,
    }))
}

pub(crate) async fn revoke_key(env: &Env, raw_key: &str) -> Result<bool> {
    let response = stub_post_json(
        &crate::accounts_stub(env)?,
        "https://accounts/revoke",
        json!({ "key": raw_key }),
    )
    .await?;
    Ok(response["revoked"].as_bool().unwrap_or(false))
}

pub(crate) async fn list_keys(env: &Env) -> Result<serde_json::Value> {
    stub_get_json(&crate::accounts_stub(env)?, "https://accounts/list").await
}

pub(crate) async fn check_rate(env: &Env, identity: &str) -> Result<RateDecision> {
    let response = stub_post_json(
        &crate::accounts_stub(env)?,
        "https://accounts/check",
        json!({ "identity": identity }),
    )
    .await?;
    Ok(RateDecision {
        allowed: response["allowed"].as_bool().unwrap_or(false),
        hour_used: response["hourUsed"].as_u64().unwrap_or_default(),
        day_used: response["dayUsed"].as_u64().unwrap_or_default(),
        retry_after_secs: response["retryAfter"].as_u64().unwrap_or(1),
    })
}

pub(crate) async fn record(env: &Env, identity: &str) -> Result<()> {
    stub_post_json(
        &crate::accounts_stub(env)?,
        "https://accounts/record",
        json!({ "identity": identity }),
    )
    .await?;
    Ok(())
}

pub(crate) async fn snapshot(env: &Env, identity: &str, tier: &Tier) -> Result<serde_json::Value> {
    stub_post_json(
        &crate::accounts_stub(env)?,
        "https://accounts/snapshot",
        json!({ "identity": identity, "tier": tier.as_str() }),
    )
    .await
}

pub(crate) async fn put_job(env: &Env, job: &Job) -> Result<()> {
    stub_post_json(
        &crate::accounts_stub(env)?,
        "https://accounts/jobs/put",
        serde_json::to_value(job)?,
    )
    .await?;
    Ok(())
}

pub(crate) async fn get_job(env: &Env, id: &str) -> Result<Option<Job>> {
    let response = stub_get_json(
        &crate::accounts_stub(env)?,
        &format!("https://accounts/jobs/get?id={id}"),
    )
    .await?;
    if response.is_null() {
        return Ok(None);
    }
    serde_json::from_value(response)
        .map(Some)
        .map_err(into_error)
}

fn into_error(error: serde_json::Error) -> Error {
    Error::RustError(error.to_string())
}

// ---------------------------------------------------------------------------
// The Durable Object
// ---------------------------------------------------------------------------

#[durable_object]
pub struct Accounts {
    state: State,
}

impl DurableObject for Accounts {
    fn new(state: State, _env: Env) -> Self {
        Self { state }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path().to_string();
        let body = if req.method() == Method::Post {
            serde_json::from_str::<serde_json::Value>(&req.text().await?)
                .unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::Value::Null
        };
        let now = crate::now_ms();

        match (req.method(), path.as_str()) {
            (Method::Post, "/create") => {
                let tier =
                    Tier::parse(body["tier"].as_str().unwrap_or("free")).unwrap_or(Tier::Free);
                let raw = format!(
                    "dk_{}_{}{}",
                    if tier == Tier::Pro { "p" } else { "f" },
                    uuid_v4(),
                    uuid_v4()
                );
                let raw = raw.chars().take(48).collect::<String>();
                let key_hash = sha256_hex(&raw);
                let account = Account {
                    key_hash: key_hash.clone(),
                    tier,
                    name: body["name"].as_str().unwrap_or_default().to_string(),
                    created_at: now,
                    revoked_at: None,
                };
                self.state
                    .storage()
                    .put(&format!("key:{key_hash}"), &account)
                    .await?;
                Response::from_json(&json!({
                    "key": raw,
                    "tier": account.tier.as_str(),
                    "name": account.name,
                    "keyHash": key_hash,
                    "createdAt": account.created_at,
                }))
            }
            (Method::Post, "/lookup") => {
                let Some(key) = body["key"].as_str() else {
                    return Response::from_json(&json!({ "account": null }));
                };
                let key_hash = sha256_hex(key);
                let account = self
                    .state
                    .storage()
                    .get::<Account>(&format!("key:{key_hash}"))
                    .await?
                    .filter(|account| account.revoked_at.is_none());
                Response::from_json(&json!({ "account": account }))
            }
            (Method::Post, "/revoke") => {
                let Some(key) = body["key"].as_str() else {
                    return Response::from_json(&json!({ "revoked": false }));
                };
                let key_hash = sha256_hex(key);
                let storage_key = format!("key:{key_hash}");
                if let Some(mut account) = self.state.storage().get::<Account>(&storage_key).await?
                {
                    if account.revoked_at.is_none() {
                        account.revoked_at = Some(now);
                        self.state.storage().put(&storage_key, &account).await?;
                    }
                    Response::from_json(&json!({ "revoked": true }))
                } else {
                    Response::from_json(&json!({ "revoked": false }))
                }
            }
            (Method::Get, "/list") => {
                let entries = self
                    .state
                    .storage()
                    .list_with_options(ListOptions::new().prefix("key:"))
                    .await?;
                let today = day_window(now).0;
                let mut keys = Vec::new();
                for entry in entries.values() {
                    let value = entry.map_err(Error::from)?;
                    let account: Account = serde_wasm_bindgen::from_value(value)?;
                    let usage_key = format!("usage:{today}:{}", account.key_hash);
                    let usage_today = self
                        .state
                        .storage()
                        .get::<u64>(&usage_key)
                        .await?
                        .unwrap_or(0);
                    keys.push(json!({
                        "keyHash": format!("{}…", &account.key_hash[..12.min(account.key_hash.len())]),
                        "tier": account.tier.as_str(),
                        "name": account.name,
                        "createdAt": account.created_at,
                        "revoked": account.revoked_at.is_some(),
                        "usageToday": usage_today,
                    }));
                }
                Response::from_json(&json!({ "keys": keys }))
            }
            (Method::Post, "/check") => {
                let identity = body["identity"].as_str().unwrap_or_default().to_string();
                let (hour_key, hour_resets) = hour_window(now);
                let (day_key, day_resets) = day_window(now);
                let hour_used = self
                    .state
                    .storage()
                    .get::<u64>(&format!("usage:{hour_key}:{identity}"))
                    .await?
                    .unwrap_or(0);
                let day_used = self
                    .state
                    .storage()
                    .get::<u64>(&format!("usage:{day_key}:{identity}"))
                    .await?
                    .unwrap_or(0);
                // Keyed identities are never limited.
                let allowed = !identity.starts_with("anon:")
                    || (hour_used < ANON_HOURLY && day_used < ANON_DAILY);
                let retry_after_secs = if identity.starts_with("anon:") && hour_used >= ANON_HOURLY
                {
                    hour_resets.saturating_sub(now) / 1000 + 1
                } else {
                    day_resets.saturating_sub(now) / 1000 + 1
                };
                Response::from_json(&json!({
                    "allowed": allowed,
                    "hourUsed": hour_used,
                    "dayUsed": day_used,
                    "retryAfter": retry_after_secs,
                }))
            }
            (Method::Post, "/record") => {
                let identity = body["identity"].as_str().unwrap_or_default().to_string();
                let (hour_key, _) = hour_window(now);
                let (day_key, _) = day_window(now);
                for window in [hour_key, day_key] {
                    let key = format!("usage:{window}:{identity}");
                    let used = self.state.storage().get::<u64>(&key).await?.unwrap_or(0);
                    self.state.storage().put(&key, &(used + 1)).await?;
                }
                Response::from_json(&json!({ "ok": true }))
            }
            (Method::Post, "/snapshot") => {
                let identity = body["identity"].as_str().unwrap_or_default().to_string();
                let is_anon = identity.starts_with("anon:");
                let (hour_key, hour_resets) = hour_window(now);
                let (day_key, day_resets) = day_window(now);
                let hour = self
                    .state
                    .storage()
                    .get::<u64>(&format!("usage:{hour_key}:{identity}"))
                    .await?
                    .unwrap_or(0);
                let day = self
                    .state
                    .storage()
                    .get::<u64>(&format!("usage:{day_key}:{identity}"))
                    .await?
                    .unwrap_or(0);
                Response::from_json(&json!({
                    "hour": hour,
                    "hourLimit": if is_anon { ANON_HOURLY } else { 0 },
                    "resetHourAt": hour_resets,
                    "day": day,
                    "dayLimit": if is_anon { ANON_DAILY } else { 0 },
                    "resetDayAt": day_resets,
                }))
            }
            (Method::Post, "/jobs/put") => {
                let job: Job = serde_json::from_value(body).map_err(into_error)?;
                self.state
                    .storage()
                    .put(&format!("job:{}", job.id), &job)
                    .await?;
                Response::from_json(&json!({ "ok": true }))
            }
            (Method::Get, p) if p.starts_with("/jobs/get") => {
                let id = url
                    .query_pairs()
                    .find(|(name, _)| name == "id")
                    .map(|(_, value)| value.to_string())
                    .unwrap_or_default();
                let job = self
                    .state
                    .storage()
                    .get::<Job>(&format!("job:{id}"))
                    .await?;
                Response::from_json(&job)
            }
            _ => Response::from_json(&json!({ "error": "not found" })).map(|r| r.with_status(404)),
        }
    }
}
