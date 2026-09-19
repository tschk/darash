//! /api/fetch and /api/batch: in-worker fetch + darash-crate extraction.
//!
//! The HTTP half uses the Workers `Fetch` API; the HTML→markdown/text/
//! outline/select half is the `darash` crate compiled to wasm, so API output
//! matches the CLI exactly.

use serde_json::{json, Value};
use worker::*;

use darash::fetch;

pub(crate) struct FetchedPage {
    pub status: u16,
    pub final_url: String,
    pub content_type: String,
    pub body: String,
}

pub(crate) async fn handle_fetch(req: &Request) -> Result<Response> {
    let url = req.url()?;
    let target = url
        .query_pairs()
        .find(|(name, _)| name == "url")
        .map(|(_, value)| value.to_string())
        .unwrap_or_default();
    if target.trim().is_empty() {
        return Ok(Response::from_json(&json!({ "error": "url is required" }))?.with_status(400));
    }
    let has = |name: &str| url.query_pairs().any(|(key, _)| key == name);
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.to_string())
    };

    let started = crate::now_ms();
    let page = match fetch_page(&target, MAX_FETCH_BYTES).await {
        Ok(page) => page,
        Err(error) => {
            return Ok(
                Response::from_json(&json!({ "error": "fetch failed", "detail": error }))?
                    .with_status(502),
            )
        }
    };
    let ms = crate::now_ms().saturating_sub(started);

    let data = if has("text") {
        Value::String(fetch::to_text(&page.body))
    } else if has("outline") {
        let limit = param("limit").and_then(|l| l.parse::<usize>().ok());
        let entries: Vec<Value> = fetch::outline(&page.body)
            .into_iter()
            .take(limit.unwrap_or(usize::MAX))
            .map(|entry| {
                json!({
                    "selector": entry.selector,
                    "count": entry.count,
                    "sample": entry.sample,
                })
            })
            .collect();
        Value::Array(entries)
    } else if let Some(selector) = param("select") {
        match fetch::select_texts(&page.body, &selector) {
            Ok(items) => {
                let limit = param("limit").and_then(|l| l.parse::<usize>().ok());
                let items: Vec<String> = items
                    .into_iter()
                    .take(limit.unwrap_or(usize::MAX))
                    .collect();
                json!({ "selector": selector, "items": items, "count": items.len() })
            }
            Err(error) => {
                return Ok(
                    Response::from_json(&json!({ "error": error.to_string() }))?.with_status(400)
                )
            }
        }
    } else {
        // Default: readable markdown.
        let mut markdown = fetch::to_markdown(&page.body);
        if let Some(budget) = param("budget").and_then(|b| b.parse::<usize>().ok()) {
            let items = markdown.split("\n\n").map(str::to_string).collect();
            let budgeted = fetch::apply_budget(items, Some(budget));
            markdown = budgeted.items.join("\n\n");
        }
        Value::String(markdown)
    };

    Ok(Response::from_json(&json!({
        "data": data,
        "meta": {
            "status": page.status,
            "url": page.final_url,
            "contentType": page.content_type,
            "bytes": page.body.len(),
            "ms": ms,
        },
    }))?)
}

pub(crate) fn max_fetch_bytes() -> usize {
    MAX_FETCH_BYTES
}

pub(crate) async fn handle_batch(
    mut req: Request,
    tier: crate::accounts::Tier,
) -> Result<Response> {
    let body: Value = serde_json::from_str(&req.text().await?)?;
    let urls: Vec<String> = body["urls"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if urls.is_empty() {
        return Ok(
            Response::from_json(&json!({ "error": "urls array is required" }))?.with_status(400),
        );
    }
    let cap = tier.batch_cap();
    if urls.len() > cap {
        return Ok(Response::from_json(&json!({
            "error": format!("batch is capped at {cap} for the {} tier", tier.as_str()),
        }))?
        .with_status(413));
    }

    let mode = body["mode"].as_str().unwrap_or("md").to_string();
    let started = crate::now_ms();

    // Concurrent fetches (Workers allows many parallel subrequests).
    let tasks = urls.iter().map(|url| {
        let mode = mode.clone();
        async move {
            let mut entry = json!({ "url": url, "ok": false });
            match fetch_page(url, MAX_FETCH_BYTES).await {
                Ok(page) => {
                    entry["status"] = json!(page.status);
                    entry["ok"] = json!(page.status < 400);
                    entry["body"] = match mode.as_str() {
                        "text" => json!(fetch::to_text(&page.body)),
                        "html" => json!(page.body),
                        _ => json!(fetch::to_markdown(&page.body)),
                    };
                    entry["bytes"] = json!(page.body.len());
                }
                Err(error) => {
                    entry["error"] = json!(error);
                }
            }
            entry
        }
    });
    let results = futures_util::future::join_all(tasks).await;

    Ok(Response::from_json(&json!({
        "data": results,
        "meta": {
            "count": urls.len(),
            "mode": mode,
            "ms": crate::now_ms().saturating_sub(started),
        },
    }))?)
}

pub(crate) async fn fetch_page(
    url: &str,
    max_bytes: usize,
) -> std::result::Result<FetchedPage, String> {
    let request = Request::new(url, Method::Get).map_err(|e| e.to_string())?;
    let mut response = Fetch::Request(request)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status_code();
    let final_url = url.to_string();
    let content_type = response
        .headers()
        .get("content-type")
        .ok()
        .flatten()
        .unwrap_or_default();
    let body = response.text().await.map_err(|e| e.to_string())?;
    if body.len() > max_bytes {
        return Err(format!("body exceeded {max_bytes} bytes"));
    }
    Ok(FetchedPage {
        status,
        final_url,
        content_type,
        body,
    })
}

const MAX_FETCH_BYTES: usize = 2 * 1024 * 1024;
