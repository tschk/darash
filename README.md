# Darash

Darash is a provider-neutral async Rust search client with an in-process
[Websurfx](https://github.com/tschk/websurfx) backend by default. It can also
query a remote [SearxNG](https://docs.searxng.org/) endpoint, bounds response
sizes, and projects results into citations without requiring an API key.

## External SearxNG

The official SearxNG Docker Compose setup is the quickest local instance. It
requires Docker with Compose:

```sh
mkdir -p searxng/core-config
cd searxng
curl -fsSL \
  -O https://raw.githubusercontent.com/searxng/searxng/master/container/docker-compose.yml \
  -O https://raw.githubusercontent.com/searxng/searxng/master/container/.env.example
cp .env.example .env
docker compose up -d
```

SearxNG is then available at `http://localhost:8080`. Stop it with
`docker compose down`. See the [SearxNG container documentation](https://docs.searxng.org/admin/installation-docker.html)
for configuration and maintenance.

## Use the crate

Add Darash from GitHub:

```toml
[dependencies]
darash = { git = "https://github.com/tschk/darash" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Create a client, build a query, and search asynchronously:

```rust,no_run
use darash::{SearchClient, SearchQuery, SafeSearch, TimeRange};

#[tokio::main]
async fn main() -> Result<(), darash::Error> {
    let client = SearchClient::new("http://localhost:8080")?;
    let query = SearchQuery::new("rust async")
        .with_categories("general,news")
        .with_engines(["brave", "duckduckgo"])
        .with_language("en-US")
        .with_page(1)
        .with_safe_search(SafeSearch::Moderate)
        .with_time_range(TimeRange::Month);

    let response = client.search(&query).await?;
    for citation in response.citations() {
        println!("{} — {}", citation.title, citation.url);
    }
    Ok(())
}
```

`SearchResponse` also exposes the raw SearxNG query, result count, results,
answers, corrections, and suggestions. Its optional `answer` is a backend
answer when one is supplied; Darash does not call an AI provider. `sources` and
`cited_sources()` expose the cited `Citation` values for host-owned synthesis.
Each `SearchResult` includes its title, URL, snippet, engines, category,
publication date, and score.

The Vane-compatible request contract carries a search mode and source selection
without adding provider credentials:

```rust,no_run
use darash::{SearchClient, SearchMode, SearchRequest, SearchSource};

#[tokio::main]
async fn main() -> Result<(), darash::Error> {
    let request = SearchRequest::new("rust async")
        .with_mode(SearchMode::Quality)
        .with_sources([SearchSource::Web, SearchSource::Academic]);
    let response = SearchClient::local()?.search_request(&request).await?;
    for source in response.cited_sources() {
        println!("{}: {}", source.title, source.url);
    }
    Ok(())
}
```

`SearchClient::local()` starts Websurfx inside the current process on an
ephemeral loopback listener and adapts its JSON result model to Darash. The
host does not need a separate search service. `SearchMode` supports `Speed`, `Balanced` (the default), and `Quality`.
`SearchSource` supports `Web` (the default), `Academic`, and `Discussions`.
JSON requests may omit `mode` and `sources`; they default to `balanced` and
`web`.
The host can synthesize an answer from the returned sources with its own model.

## CLI

The native CLI starts in-process Websurfx by default. Pass `--url` only when
using another SearxNG-compatible endpoint:

```sh
cargo run -- search "rust async"
cargo run -- search "rust async" --mode quality --source academic --url http://localhost:9090
```

The CLI prints any backend answer and the cited sources. AI synthesis remains a
host responsibility; no MCP server is needed for this in-process tool.

Use `SearchConfig` when the endpoint needs a custom timeout:

```rust,no_run
use std::time::Duration;
use darash::{SearchClient, SearchConfig};

fn main() -> Result<(), darash::Error> {
    let config = SearchConfig::new("https://search.example.test")?
        .with_timeout(Duration::from_secs(5));
    let _client = SearchClient::from_config(config)?;
    Ok(())
}
```

## Limits and errors

- Requests time out after 15 seconds by default; configure this with
  `SearchConfig::with_timeout`.
- Response bodies are capped at 256 KiB, including streamed responses.
- Queries must contain non-whitespace text, and page numbers start at 1.
- Endpoints must use `http` or `https` and cannot contain embedded credentials.
- Redirects are disabled. Point the client at the final SearxNG endpoint.
- `Error` distinguishes invalid configuration, request failures, non-success
  HTTP responses, oversized or invalid responses, and JSON decode failures.

## Native quality checks

Run these commands from the crate root:

```sh
cargo fmt --all -- --check
cargo build --locked
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo package --locked
```

Darash is licensed under AGPL-3.0.
