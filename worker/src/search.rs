//! Provider-neutral search: SearXNG/websurfx instances first (JSON API),
//! keyless fallbacks (Bing RSS, Marginalia public API) when datacenter egress
//! is rate-limited.
//!
//! `SEARXNG_URLS` (comma-separated) overrides the instance list, so a
//! self-hosted searxng/websurfx instance can be plugged in without a redeploy
//! of code — just an env var change.

use futures_util::stream::{FuturesUnordered, StreamExt};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use worker::*;

use crate::{http_get, http_get_json};

/// One racer in the provider race: its display name plus its result.
type Race<'a> =
    Pin<Box<dyn Future<Output = (String, std::result::Result<Vec<Value>, String>)> + 'a>>;

pub(crate) const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36 darash-worker/0.1.0";

/// SearXNG answers its JSON API fast or not at all; a long timeout only adds
/// latency when an instance is rate-limiting datacenter egress.
const SEARXNG_TIMEOUT_MS: u64 = 2_500;
const FALLBACK_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_INSTANCES: [&str; 5] = [
    "https://searx.be",
    "https://priv.au",
    "https://paulgo.io",
    "https://search.bus-hit.me",
    "https://searx.tiekoetter.com",
];

pub(crate) async fn handle(req: &Request, env: &Env) -> Result<Response> {
    let url = req.url()?;
    let query = url
        .query_pairs()
        .find(|(name, _)| name == "q")
        .map(|(_, value)| value.to_string())
        .unwrap_or_default();
    if query.trim().is_empty() {
        return Ok(Response::from_json(&json!({ "error": "q is required" }))?.with_status(400));
    }
    if query.chars().count() > 512 {
        return Ok(
            Response::from_json(&json!({ "error": "q must be at most 512 characters" }))?
                .with_status(400),
        );
    }
    let mode = url
        .query_pairs()
        .find(|(name, _)| name == "mode")
        .map(|(_, value)| value.to_string())
        .unwrap_or_else(|| "balanced".to_string());
    let limit = url
        .query_pairs()
        .find(|(name, _)| name == "limit")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(8)
        .min(50);
    let wanted = match mode.as_str() {
        "speed" => 1,
        "quality" => 3,
        _ => 2,
    };

    let instances: Vec<String> = env
        .var("SEARXNG_URLS")
        .ok()
        .map(|value| {
            value
                .to_string()
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| DEFAULT_INSTANCES.iter().map(|s| s.to_string()).collect());

    let started = crate::now_ms();
    let mut used: Vec<String> = Vec::new();
    let mut lists: Vec<Vec<Value>> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    // Race every source at once — SearXNG instances and the bing/marginalia
    // fallbacks together — and stop as soon as `wanted` engines have
    // answered. Wall time is the Nth-fastest source, not the sum of
    // timeouts; a dead instance no longer delays the fallback that
    // replaces it, and losing races are dropped mid-flight.
    let mut races: FuturesUnordered<Race> = instances
        .iter()
        .cloned()
        .map(|base| {
            let query = query.as_str();
            Box::pin(async move {
                let outcome = query_searxng(&base, query).await;
                (base, outcome)
            }) as Race
        })
        .collect();
    races.push(Box::pin(async { ("bing".to_string(), query_bing_rss(&query).await) }) as Race);
    races.push(Box::pin(async {
        ("marginalia".to_string(), query_marginalia(&query).await)
    }) as Race);

    while let Some((base, outcome)) = races.next().await {
        match outcome {
            Ok(results) if !results.is_empty() => {
                used.push(base);
                lists.push(results);
            }
            Ok(_) => failures.push(format!("{base}: no results")),
            Err(error) => failures.push(format!("{base}: {error}")),
        }
        if lists.len() >= wanted {
            break;
        }
    }
    drop(races);

    if lists.is_empty() {
        return Ok(Response::from_json(&json!({
            "error": "all search instances failed",
            "detail": failures,
        }))?
        .with_status(502));
    }

    let results = merge(lists, limit);
    Ok(Response::from_json(&json!({
        "data": { "query": query, "results": results },
        "meta": { "ms": crate::now_ms().saturating_sub(started), "instances": used },
    }))?)
}

async fn query_searxng(base: &str, query: &str) -> std::result::Result<Vec<Value>, String> {
    let url = format!(
        "{}/search?q={}&format=json",
        base.trim_end_matches('/'),
        urlencode(query)
    );
    let body = http_get_json(&url, SEARXNG_TIMEOUT_MS)
        .await
        .map_err(|error| error.to_string())?;
    let Some(results) = body["results"].as_array() else {
        return Err("no results array".to_string());
    };
    Ok(results
        .iter()
        .filter_map(|item| {
            let url = item["url"].as_str()?;
            if url.is_empty() {
                return None;
            }
            Some(json!({
                "title": item["title"].as_str().unwrap_or_default(),
                "url": url,
                "content": item["content"].as_str().unwrap_or_default(),
                "engine": item["engine"].as_str().unwrap_or("searxng"),
                "score": 0,
            }))
        })
        .collect())
}

async fn query_bing_rss(query: &str) -> std::result::Result<Vec<Value>, String> {
    let url = format!(
        "https://www.bing.com/search?q={}&format=rss",
        urlencode(query)
    );
    let xml = http_get(
        &url,
        FALLBACK_TIMEOUT_MS,
        "text/html,text/xml,application/json",
    )
    .await
    .map_err(|error| error.to_string())?;
    let mut results = Vec::new();
    for item in xml.split("<item>").skip(1) {
        let Some(end) = item.find("</item>") else {
            continue;
        };
        let item = &item[..end];
        let tag = |name: &str| -> String {
            let open = format!("<{name}>");
            let close = format!("</{name}>");
            item.find(&open)
                .and_then(|start| {
                    item[start + open.len()..]
                        .find(&close)
                        .map(|len| item[start + open.len()..start + open.len() + len].to_string())
                })
                .map(|value| decode_xml(&value))
                .unwrap_or_default()
        };
        let title = tag("title");
        let link = tag("link");
        if title.is_empty() || !link.starts_with("http") {
            continue;
        }
        results.push(json!({
            "title": title,
            "url": link,
            "content": tag("description"),
            "engine": "bing",
            "score": 0,
        }));
    }
    Ok(results)
}

async fn query_marginalia(query: &str) -> std::result::Result<Vec<Value>, String> {
    let url = format!(
        "https://api.marginalia.nu/public/search/{}",
        urlencode(query)
    );
    let body = http_get_json(&url, FALLBACK_TIMEOUT_MS)
        .await
        .map_err(|error| error.to_string())?;
    let Some(results) = body["results"].as_array() else {
        return Err("no results array".to_string());
    };
    Ok(results
        .iter()
        .filter_map(|item| {
            let url = item["url"].as_str()?;
            let title = item["title"].as_str()?;
            if title.is_empty() || !url.starts_with("http") {
                return None;
            }
            Some(json!({
                "title": title,
                "url": url,
                "content": item["description"].as_str().unwrap_or_default(),
                "engine": "marginalia",
                "score": 0,
            }))
        })
        .collect())
}

/// Dedupe by normalized host+path, score by engine-count + position.
fn merge(lists: Vec<Vec<Value>>, limit: usize) -> Vec<Value> {
    let mut merged: Vec<(String, Value)> = Vec::new();
    for list in &lists {
        for (index, result) in list.iter().enumerate() {
            let Some(url) = result["url"].as_str() else {
                continue;
            };
            let key = normalize_url(url);
            let bonus = 10 + (list.len() - index);
            if let Some((_, existing)) = merged
                .iter_mut()
                .find(|(key_existing, _)| *key_existing == key)
            {
                existing["score"] = json!(existing["score"].as_u64().unwrap_or(0) + bonus as u64);
            } else {
                let mut entry = result.clone();
                entry["score"] = json!(bonus as u64);
                merged.push((key, entry));
            }
        }
    }
    merged.sort_by_key(|(_, entry)| -entry["score"].as_i64().unwrap_or(0));
    merged
        .into_iter()
        .map(|(_, entry)| entry)
        .take(limit)
        .collect()
}

fn normalize_url(raw: &str) -> String {
    if let Ok(url) = raw.parse::<url::Url>() {
        let host = url
            .host_str()
            .unwrap_or_default()
            .trim_start_matches("www.");
        let path = url.path().trim_end_matches('/');
        format!("{host}{path}")
    } else {
        raw.to_string()
    }
}

pub(crate) fn urlencode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn decode_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .trim()
        .to_string()
}
