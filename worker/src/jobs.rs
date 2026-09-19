//! Async jobs + pro research endpoint.
//!
//! Jobs are created instantly, processed in the background via `ctx.wait_until`,
//! and stored in the Accounts DO under `job:<id>`. Research (pro) runs a
//! search, fetches the top pages, and returns per-source markdown — a research
//! corpus without any LLM in the loop.

use serde_json::{json, Value};
use worker::*;

use crate::accounts::{self, Job, Tier};
use crate::{extract, search, Routed};

pub(crate) async fn create(
    mut req: Request,
    env: &Env,
    ctx: Context,
    tier: Tier,
) -> Result<Routed> {
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
        return accepted_routed(
            json!({ "error": "urls array is required" }),
            400,
            tier.as_str(),
        );
    }
    let cap = tier.batch_cap();
    if urls.len() > cap {
        return accepted_routed(
            json!({
                "error": format!("job is capped at {cap} urls for the {} tier", tier.as_str()),
            }),
            413,
            tier.as_str(),
        );
    }

    let mode = body["mode"].as_str().unwrap_or("md").to_string();
    let job = Job {
        id: crate::uuid_v4(),
        kind: "batch".to_string(),
        urls: urls.clone(),
        status: "pending".to_string(),
        created_at: crate::now_ms(),
        results: Value::Null,
    };
    accounts::put_job(env, &job).await?;

    let env_for_task = env.clone();
    let job_for_task = job.clone();
    ctx.wait_until(async move {
        let _ = process_batch(&env_for_task, job_for_task, mode).await;
    });

    Ok(Routed {
        response: Response::from_json(&json!({
            "data": { "jobId": job.id, "status": "pending", "count": urls.len() },
            "meta": { "poll": format!("/api/async/jobs/{}", job.id), "tier": tier.as_str() },
        }))?
        .with_status(202),
        tier: tier.as_str().to_string(),
    })
}

fn accepted_routed(body: Value, status: u16, tier: &str) -> Result<Routed> {
    Ok(Routed {
        response: Response::from_json(&body)?.with_status(status),
        tier: tier.to_string(),
    })
}

async fn process_batch(env: &Env, mut job: Job, mode: String) -> worker::Result<()> {
    job.status = "running".to_string();
    accounts::put_job(env, &job).await?;

    let tasks = job.urls.iter().map(|url| {
        let mode = mode.clone();
        async move {
            let mut entry = json!({ "url": url, "ok": false });
            match extract::fetch_page(url, extract::max_fetch_bytes()).await {
                Ok(page) => {
                    entry["status"] = json!(page.status);
                    entry["ok"] = json!(page.status < 400);
                    entry["body"] = match mode.as_str() {
                        "text" => json!(darash::fetch::to_text(&page.body)),
                        "html" => json!(page.body),
                        _ => json!(darash::fetch::to_markdown(&page.body)),
                    };
                    entry["bytes"] = json!(page.body.len());
                }
                Err(error) => entry["error"] = json!(error),
            }
            entry
        }
    });
    let results = futures_util::future::join_all(tasks).await;

    job.results = Value::Array(results);
    job.status = "complete".to_string();
    accounts::put_job(env, &job).await
}

pub(crate) async fn status(env: &Env, id: &str) -> Result<Option<Value>> {
    let job = accounts::get_job(env, id).await?;
    Ok(job.map(|job| {
        json!({
            "id": job.id,
            "status": job.status,
            "count": job.urls.len(),
            "createdAt": job.created_at,
            "results": job.results,
        })
    }))
}

/// Pro research: search → fetch top pages → markdown corpus per source.
pub(crate) async fn research(mut req: Request, env: &Env) -> Result<Response> {
    let body: Value = serde_json::from_str(&req.text().await?)?;
    let query = body["query"].as_str().unwrap_or_default().to_string();
    if query.trim().is_empty() {
        return Ok(Response::from_json(&json!({ "error": "query is required" }))?.with_status(400));
    }
    let limit = body["limit"]
        .as_u64()
        .unwrap_or(3)
        .min(crate::accounts::MAX_RESEARCH_PAGES as u64) as usize;

    let search_response = run_search(env, &query).await?;
    let urls: Vec<String> = search_response["data"]["results"]
        .as_array()
        .map(|results| {
            results
                .iter()
                .filter_map(|result| result["url"].as_str().map(str::to_string))
                .take(limit)
                .collect()
        })
        .unwrap_or_default();

    let started = crate::now_ms();
    let tasks = urls.iter().map(|url| async move {
        let mut entry = json!({ "url": url });
        match extract::fetch_page(url, extract::max_fetch_bytes()).await {
            Ok(page) => {
                entry["status"] = json!(page.status);
                entry["markdown"] = json!(darash::fetch::to_markdown(&page.body));
            }
            Err(error) => entry["error"] = json!(error),
        }
        entry
    });
    let sources = futures_util::future::join_all(tasks).await;

    Ok(Response::from_json(&json!({
        "data": { "query": query, "sources": sources },
        "meta": { "ms": crate::now_ms().saturating_sub(started), "pages": urls.len() },
    }))?)
}

/// Run the search pipeline without an incoming /api/search request.
async fn run_search(env: &Env, query: &str) -> Result<Value> {
    // Reuse the search handler by synthesizing a request against /api/search.
    let url = format!(
        "https://local/api/search?q={}&mode=balanced&limit={}",
        search::urlencode(query),
        6
    );
    let request = Request::new(&url, Method::Get)?;
    let mut response = search::handle(&request, env).await?;
    response.json().await
}
