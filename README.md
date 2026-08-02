# Darash

Darash is a provider-neutral async Rust search client with a small in-process
multi-source backend by default. It can also query a remote
[SearxNG](https://docs.searxng.org/) endpoint, bounds response sizes, and
projects results into citations without requiring an API key.

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

Add Darash from crates.io:

```toml
[dependencies]
darash = "0.4.0"
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
Each `SearchResult` includes its title, URL, content, engines, category,
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

`SearchClient::local()` runs Darash's provider adapters directly in the current
process. The default backend queries DuckDuckGo, OpenAlex, and Hacker News as
needed; it does not start a separate search service. `SearchMode` supports
`Speed`, `Balanced` (the default), and `Quality`.
`SearchSource` supports `Web` (the default), `Academic`, and `Discussions`.
JSON requests may omit `mode` and `sources`; they default to `balanced` and
`web`.
The host can synthesize an answer from the returned sources with its own model.

The local providers are selected from the requested sources and run
concurrently:

- `web` queries DuckDuckGo HTML results.
- `academic` queries OpenAlex works.
- `discussions` queries Hacker News Algolia results.

Each selected provider contributes up to 5 results in `speed` mode and up to 10
in `balanced` or `quality` mode before URL deduplication and relevance ranking.
`SearchQuery::with_engines` selects the local provider names
`duckduckgo`/`ddg`, `openalex`, and `hacker-news`/`hn`; source categories remain
the convenient default selection. `SearchResponse::provider_status` preserves
success and failure information when one provider is unavailable.

`SearchMode` limits provider selection and retrieval as well as the returned
`results` and `sources`: speed selects one provider and returns at most 5
results, balanced selects up to two and returns at most 10, and quality selects
all requested providers and returns at most 20. `number_of_results` is not
changed by that cap.

`SafeSearch` supports levels 0 through 4. Remote requests use the SearxNG
values; local DuckDuckGo requests use DuckDuckGo's native values and date-range
codes. Configure local level-3/4 filtering with
`SearchConfig::with_blocklist` and `with_allowlist`. The response exposes the
applied level and `filtered`/`disallowed` flags in `SearchFilters`. Local
responses use a bounded in-memory TTL cache by default; configure it with
`with_cache` or call `clear_cache` on the client.

The local backend is a direct in-process adapter. It does not start an HTTP
listener, expose a Websurfx server, spawn a search subprocess, or read
Websurfx configuration or assets. The dependency-free Websurfx compatibility
types and URL builder are available for hosts that already run Websurfx; they
do not embed Websurfx or add its AGPL dependency tree. Use
`SearchClient::search_websurfx` when a configured endpoint is a Websurfx
server; it maps Websurfx's engine and error metadata into Darash's response
model.

## CLI

The native CLI starts the in-process Darash backend by default. Pass `--url`
only when using another SearxNG-compatible endpoint:

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

Darash is licensed under the ISC license.
