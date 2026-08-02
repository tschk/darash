use crate::{Error, SearchResponse, SearchResult};
use serde::Deserialize;
use std::{
    fs,
    path::PathBuf,
    sync::{mpsc, OnceLock},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{net::TcpListener, runtime::Builder};
use url::Url;

static ENDPOINT: OnceLock<Result<Url, String>> = OnceLock::new();

pub(crate) fn endpoint() -> Result<Url, Error> {
    ENDPOINT
        .get_or_init(|| start().map_err(|error| error.to_string()))
        .clone()
        .map_err(Error::Embedded)
}

pub(crate) fn parse_response(query: &str, body: &str) -> Result<SearchResponse, serde_json::Error> {
    let response: WebsurfxResponse = serde_json::from_str(body)?;
    let results = response
        .results
        .into_iter()
        .map(|result| SearchResult {
            title: result.title,
            url: result.url,
            content: result.description,
            engine: result.engine.first().cloned(),
            engines: result.engine,
            category: None,
            published_date: None,
            score: Some(f64::from(result.relevance_score)),
        })
        .collect::<Vec<_>>();
    let sources = results
        .iter()
        .map(SearchResult::citation)
        .collect::<Vec<_>>();
    Ok(SearchResponse {
        query: query.to_owned(),
        number_of_results: results.len() as u64,
        results,
        answers: Vec::new(),
        answer: None,
        sources,
        corrections: Vec::new(),
        suggestions: Vec::new(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebsurfxResponse {
    #[serde(default)]
    results: Vec<WebsurfxResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebsurfxResult {
    title: String,
    url: String,
    description: String,
    #[serde(default)]
    engine: Vec<String>,
    relevance_score: f32,
}

fn start() -> Result<Url, Error> {
    let root = runtime_root()?;
    fs::create_dir_all(root.join("public")).map_err(|error| Error::Embedded(error.to_string()))?;
    fs::create_dir_all(root.join("websurfx"))
        .map_err(|error| Error::Embedded(error.to_string()))?;
    fs::write(root.join("websurfx/config.lua"), config())
        .map_err(|error| Error::Embedded(error.to_string()))?;

    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("darash-websurfx".to_owned())
        .spawn(move || {
            let result = run_server(root, sender.clone());
            if let Err(error) = result {
                let _ = sender.send(Err(error));
            }
        })
        .map_err(|error| Error::Embedded(error.to_string()))?;

    receiver
        .recv()
        .map_err(|error| Error::Embedded(error.to_string()))?
        .map_err(Error::Embedded)
}

fn run_server(root: PathBuf, sender: mpsc::SyncSender<Result<Url, String>>) -> Result<(), String> {
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let data_root = root.to_string_lossy().into_owned();
    let (server, address) = runtime.block_on(async {
        websurfx::set_data_root(data_root).map_err(|error| error.to_string())?;
        let config = websurfx::parser::Config::parse(true)
            .await
            .map_err(|error| error.to_string())?;
        let config = Box::leak(Box::new(config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let server = websurfx::run(listener, config)
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((server, address))
    })?;
    let endpoint = Url::parse(&format!("http://{address}")).map_err(|error| error.to_string())?;
    sender
        .send(Ok(endpoint))
        .map_err(|error| error.to_string())?;
    runtime.block_on(server).map_err(|error| error.to_string())
}

fn runtime_root() -> Result<PathBuf, Error> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Embedded(error.to_string()))?
        .as_nanos();
    Ok(std::env::temp_dir().join(format!(
        "darash-websurfx-{}-{timestamp}",
        std::process::id()
    )))
}

fn config() -> &'static str {
    "logging = false
debug = false
threads = 1
port = 0
binding_ip = \"127.0.0.1\"
production_use = false
request_timeout = 15
tcp_connection_keep_alive = 30
pool_idle_connection_timeout = 30
rate_limiter = { number_of_requests = 20, time_limit = 3 }
adaptive_window = true
operating_system_tls_certificates = true
number_of_https_connections = 4
client_connection_keep_alive = 30
safe_search = 0
colorscheme = \"catppuccin-mocha\"
theme = \"simple\"
animation = \"simple-frosted-glow\"
http_cache_expiry_time = 60
upstream_search_engines = { DuckDuckGo = true, Wikipedia = true }
proxy = nil
"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Citation;

    #[test]
    fn maps_websurfx_results_to_citations() {
        let response = parse_response(
            "rust",
            r#"{"results":[{"title":"Rust","url":"https://rust-lang.org","description":"A language","engine":["DuckDuckGo"],"relevanceScore":0.75}]}"#,
        )
        .expect("valid Websurfx response");

        assert_eq!(response.query, "rust");
        assert_eq!(response.number_of_results, 1);
        assert_eq!(response.results[0].content, "A language");
        assert_eq!(
            response.citations(),
            [Citation {
                title: "Rust".to_owned(),
                url: "https://rust-lang.org".to_owned(),
                snippet: "A language".to_owned(),
                source: Some("DuckDuckGo".to_owned()),
                published_date: None,
            }]
        );
    }
}
