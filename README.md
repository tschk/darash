# Darash

Darash is an all-in-one research crate: provider-neutral async web search, page
fetching, and HTML extraction in one dependency, with no API keys. Search runs
on a small in-process multi-source backend by default and can also query a
remote [SearxNG](https://docs.searxng.org/) endpoint; fetch and extraction turn
any page into markdown, outlines, selector matches, rows, and tables — local,
deterministic, and shaped for a context window.

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
darash = "0.7.0"
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

## Fetch and extract

Search finds sources; `darash::fetch` reads them. Every fetch returns a
[`FetchReport`](https://docs.rs/darash) with the status, final URL, redirect
flag, elapsed time, content type, size, and body — never silent, even for
error statuses. Bodies are capped at 2 MiB.

```rust,no_run
use darash::fetch;

#[tokio::main]
async fn main() -> Result<(), darash::Error> {
    let report = fetch::fetch("https://example.com", &[]).await?;
    println!("{}", report.summary());

    // Agent-shaped output, all local and deterministic:
    let markdown = fetch::to_markdown(&report.body);   // page as markdown
    let outline = fetch::outline(&report.body);        // repeating structures
    let titles = fetch::select_texts(&report.body, "h2")?; // selector matches
    let rows = fetch::rows(&report.body, ".item", "title=h2, url=a@href")?;
    let tables = fetch::tables(&report.body);          // keyed table rows
    let tokens = fetch::estimate_tokens(&markdown);    // ~4 chars per token
    Ok(())
}
```

`apply_budget` cuts a list of items at item boundaries to fit an estimated
token budget, always keeping at least one item. `fetch::fetch(url, headers)` is
a thin wrapper over `fetch::fetch_with(url, FetchOptions { .. })`, which carries
a method, request body, basic auth, insecure-TLS opt-in, timeout, and body cap.
`fetch::read_source(path)` reads a local file through the same pipeline.
`fetch::locate(html, text)` finds the deepest elements that hold a piece of text
and returns a CSS selector path plus a short snippet. `fetch::paginate(total,
offset, limit)` computes the `PageMeta` state (`more`, `complete`, `past_end`)
the CLI uses. The `filter` module compiles the safe `--where` expression
language, and `disk_cache` holds the CLI's short-lived on-disk body cache.

## CLI

The `darash` binary ships with the crate (`cargo install darash`). Search
starts the in-process backend by default; pass `--url` only when using another
SearxNG-compatible endpoint:

```sh
darash search "rust async"
darash search "rust async" --mode quality --source academic --url http://localhost:9090
darash search "rust async" --json
```

`fetch` is the research half of the CLI. Without extraction flags it prints the
full report as JSON (including the body); `--body` prints only the body. With a
mode it prints extracted data on stdout and a one-line report on stderr:

```sh
darash fetch https://example.com                          # full report, JSON
darash fetch https://example.com --body                   # response body only
darash fetch https://example.com --md                     # page as markdown
darash fetch https://example.com --md --budget 800        # markdown within ~800 tokens
darash fetch https://example.com --outline                # repeating structures
darash fetch https://example.com --select "h2" --limit 10 # selector matches
darash fetch https://example.com --select "h2" --count    # just the match count
darash fetch https://example.com --select ".item" --row "title=h2, url=a@href"
darash fetch https://example.com --table --json           # keyed rows as JSON
darash fetch https://example.com --locate "Example Domain" # which selector holds it
darash fetch page.html --md                               # local files work too
```

Rows and single tables print as TSV by default (a header line once, then values;
tabs and newlines inside a value fold to spaces, and objects become JSON);
`--json` switches them to JSON rows, and multiple tables always print as JSON.
`--locate TEXT` reports each deepest element whose text or an attribute contains
`TEXT`, as a CSS selector path and an 80-character snippet.

Structured output (`--select`, `--row`, `--table`, `--locate`) is paginated:
`--limit` (default 50) caps a page and `--offset` starts it. In plain output the
CLI announces `N more result(s) hidden — continue with --offset …` or that the
offset is past the end; `--json-envelope` instead prints
`{"data": …, "meta": {state, total, offset, returned, next_offset}}`, where
`state` is `more`, `complete`, or `past_end`. `--where EXPR` filters rows and
tables with a safe expression language (no `eval`): comparisons (`== != ~ !~ >
>= < <=`), `&&`/`||`/`!`, numeric-string coercion, `/regex/[flags]`, backtick
literal column names, and dotted paths that prefer a literally named column.
When a filter matches nothing the CLI says so on stderr.

`fetch` also accepts the usual curl reflexes: `-X/--method`, `-d/--data`
(`@file`/`@-` strip CR/LF; `--data-raw` never reads `@` as a file;
`--data-binary` keeps bytes), `-u user:pass`, `-I/--head`, `-o FILE` (atomic
write), `-k/--insecure`, `-m/--max-time`, `--max-bytes`, and `-f/--fail` (HTTP
error exits 22 after printing the report). `-L`, `-i`, `-s`, and `-S` are
accepted no-ops. Fetched URL bodies are cached for about two minutes under the
user's cache directory and reused by parse modes; `--fresh` refetches and
re-caches, `--no-cache` never reads or writes, and credential-bearing URLs,
custom headers, non-`GET` methods, request bodies, and `Cache-Control: no-store`
bypass the cache. Cache hits announce their age on stderr.

Extraction output is capped at 50 items by default (`--limit`) and can be
further bounded with `--budget` (estimated tokens, cut at item boundaries);
omissions are announced on stderr, never silent. `--json` wraps any mode as
`{"data": …, "meta": {status, ok, url, ms, bytes, count, omitted}}`.

AI synthesis remains a host responsibility; no MCP server is needed for this
in-process tool.

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
