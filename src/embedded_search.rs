use crate::{
    Error, ProviderStatus, SafeSearch, SearchConfig, SearchQuery, SearchResponse, SearchResult,
    TimeRange,
};
use futures_util::future::join_all;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use url::{form_urlencoded, Url};

const DUCKDUCKGO_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const OPENALEX_ENDPOINT: &str = "https://api.openalex.org/works";
const HACKER_NEWS_ENDPOINT: &str = "https://hn.algolia.com/api/v1/search";
const RESULTS_PER_PROVIDER: usize = 10;
const USER_AGENT: &str = "darash-search/0.3";
const MAX_PROVIDER_ERROR_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderFailure {
    pub(crate) provider: &'static str,
    pub(crate) error: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SearchOutcome {
    pub(crate) response: SearchResponse,
    pub(crate) failures: Vec<ProviderFailure>,
}

impl ProviderFailure {
    fn new(provider: Provider, error: impl Into<String>) -> Self {
        Self {
            provider: provider.name(),
            error: error.into(),
        }
    }
}

#[derive(Clone, Copy)]
enum Provider {
    Web,
    Academic,
    Discussions,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Web => "duckduckgo",
            Self::Academic => "openalex",
            Self::Discussions => "hacker-news",
        }
    }

    fn category(self) -> &'static str {
        match self {
            Self::Web => "general",
            Self::Academic => "science",
            Self::Discussions => "social media",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "duckduckgo" | "ddg" | "web" => Some(Self::Web),
            "openalex" | "academic" | "science" => Some(Self::Academic),
            "hacker-news" | "hackernews" | "hn" | "discussions" => Some(Self::Discussions),
            _ => None,
        }
    }
}

pub(crate) async fn search_with_outcome(
    client: &Client,
    query: &SearchQuery,
    config: &SearchConfig,
) -> Result<SearchOutcome, Error> {
    validate_page(query)?;
    let providers = providers(query);
    if query.safe_search == Some(SafeSearch::Level4) && matches_blocklist(query.query(), config) {
        let mut filters = query.filters();
        filters.disallowed = true;
        return Ok(SearchOutcome {
            response: SearchResponse {
                query: query.query().to_owned(),
                number_of_results: 0,
                results: Vec::new(),
                answers: Vec::new(),
                answer: None,
                sources: Vec::new(),
                corrections: Vec::new(),
                suggestions: Vec::new(),
                provider_status: Vec::new(),
                filters,
            },
            failures: Vec::new(),
        });
    }
    let outcomes =
        join_all(providers.iter().copied().map(|provider| async move {
            (provider, fetch_provider(client, provider, query).await)
        }))
        .await;

    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut provider_status = Vec::with_capacity(outcomes.len());
    for (provider, outcome) in outcomes {
        match outcome {
            Ok(provider_results) => {
                provider_status.push(ProviderStatus::success(
                    provider.name(),
                    provider_results.len(),
                ));
                results.extend(provider_results);
            }
            Err(error) => {
                provider_status.push(ProviderStatus::failed(error.provider, &error.error));
                failures.push(error);
            }
        }
    }
    if results.is_empty() && !failures.is_empty() {
        return Err(Error::Local(
            failures
                .iter()
                .map(|failure| format!("{}: {}", failure.provider, failure.error))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }

    let mut results = dedupe_and_rank(query.query(), results);
    let before_filter = results.len();
    if query.safe_search.unwrap_or_default().level() >= 3 {
        results.retain(|result| {
            !matches_blocklist_result(result, config) || matches_allowlist_result(result, config)
        });
    }
    let mut filters = query.filters();
    filters.filtered = results.len() != before_filter;
    filters.no_providers_selected = providers.is_empty();
    let sources = results.iter().map(SearchResult::citation).collect();
    Ok(SearchOutcome {
        response: SearchResponse {
            query: query.query().to_owned(),
            number_of_results: results.len() as u64,
            results,
            answers: Vec::new(),
            answer: None,
            sources,
            corrections: Vec::new(),
            suggestions: Vec::new(),
            provider_status,
            filters,
        },
        failures,
    })
}

fn providers(query: &SearchQuery) -> Vec<Provider> {
    if !query.engines.is_empty() {
        let selected: Vec<Provider> = query
            .engines
            .iter()
            .filter_map(|name| Provider::from_name(name))
            .fold(Vec::new(), |mut selected, provider| {
                if !selected
                    .iter()
                    .any(|current| current.name() == provider.name())
                {
                    selected.push(provider);
                }
                selected
            });
        if !selected.is_empty() {
            return selected
                .into_iter()
                .take(query.mode().provider_limit())
                .collect();
        }
    }
    let categories = query.categories.as_deref().unwrap_or("general");
    let mut selected: Vec<Provider> = Vec::new();
    for category in categories.split(',').map(str::trim) {
        let provider = match category {
            "science" => Provider::Academic,
            "social media" => Provider::Discussions,
            _ => Provider::Web,
        };
        if !selected
            .iter()
            .any(|current| current.name() == provider.name())
        {
            selected.push(provider);
        }
    }
    if selected.is_empty() {
        selected.push(Provider::Web);
    }
    selected
        .into_iter()
        .take(query.mode().provider_limit())
        .collect()
}

fn matches_blocklist(value: &str, config: &SearchConfig) -> bool {
    let value = value.to_ascii_lowercase();
    config
        .blocklist
        .iter()
        .filter(|term| !term.trim().is_empty())
        .any(|term| value.contains(&term.to_ascii_lowercase()))
}

fn matches_allowlist_result(result: &SearchResult, config: &SearchConfig) -> bool {
    let value = format!("{} {} {}", result.title, result.url, result.content);
    let value = value.to_ascii_lowercase();
    config
        .allowlist
        .iter()
        .filter(|term| !term.trim().is_empty())
        .any(|term| value.contains(&term.to_ascii_lowercase()))
}

fn matches_blocklist_result(result: &SearchResult, config: &SearchConfig) -> bool {
    let value = format!("{} {} {}", result.title, result.url, result.content);
    matches_blocklist(&value, config)
}

async fn fetch_provider(
    client: &Client,
    provider: Provider,
    query: &SearchQuery,
) -> Result<Vec<SearchResult>, ProviderFailure> {
    match provider {
        Provider::Web => fetch_web(client, provider, query).await,
        Provider::Academic => fetch_openalex(client, provider, query).await,
        Provider::Discussions => fetch_hacker_news(client, provider, query).await,
    }
}

async fn fetch_web(
    client: &Client,
    provider: Provider,
    query: &SearchQuery,
) -> Result<Vec<SearchResult>, ProviderFailure> {
    let params = duckduckgo_params(query).map_err(|error| ProviderFailure::new(provider, error))?;
    let url = format!("{DUCKDUCKGO_ENDPOINT}?{params}");
    let body = get_body(client, &url)
        .await
        .map_err(|error| ProviderFailure::new(provider, error))?;
    let mut results =
        parse_duckduckgo(&body, provider).map_err(|error| ProviderFailure::new(provider, error))?;
    results.truncate(query.mode().provider_result_limit());
    Ok(results)
}

async fn fetch_openalex(
    client: &Client,
    provider: Provider,
    query: &SearchQuery,
) -> Result<Vec<SearchResult>, ProviderFailure> {
    let params = openalex_params(query);
    let url = format!("{OPENALEX_ENDPOINT}?{params}");
    let body = get_body(client, &url)
        .await
        .map_err(|error| ProviderFailure::new(provider, error))?;
    let mut results =
        parse_openalex(&body, provider).map_err(|error| ProviderFailure::new(provider, error))?;
    results.truncate(query.mode().provider_result_limit());
    Ok(results)
}

fn parse_openalex(body: &str, provider: Provider) -> Result<Vec<SearchResult>, String> {
    let response: OpenAlexResponse =
        serde_json::from_str(body).map_err(|error| error.to_string())?;
    Ok(response
        .results
        .into_iter()
        .filter_map(|work| {
            let url = work
                .doi
                .or_else(|| {
                    work.primary_location
                        .as_ref()
                        .and_then(|location| location.landing_page_url.clone())
                })
                .or_else(|| {
                    work.primary_location
                        .as_ref()
                        .and_then(|location| location.pdf_url.clone())
                })?;
            let title = work.title.unwrap_or_else(|| "Untitled work".to_owned());
            let mut snippet = abstract_text(work.abstract_inverted_index);
            if let Some(year) = work.publication_year {
                if !snippet.is_empty() {
                    snippet.push_str(" — ");
                }
                snippet.push_str(&year.to_string());
            }
            Some(result(provider, title, url, snippet, work.publication_date))
        })
        .collect())
}

async fn fetch_hacker_news(
    client: &Client,
    provider: Provider,
    query: &SearchQuery,
) -> Result<Vec<SearchResult>, ProviderFailure> {
    let params =
        hacker_news_params(query).map_err(|error| ProviderFailure::new(provider, error))?;
    let url = format!("{HACKER_NEWS_ENDPOINT}?{params}");
    let body = get_body(client, &url)
        .await
        .map_err(|error| ProviderFailure::new(provider, error))?;
    let mut results = parse_hacker_news(&body, provider)
        .map_err(|error| ProviderFailure::new(provider, error))?;
    results.truncate(query.mode().provider_result_limit());
    Ok(results)
}

fn parse_hacker_news(body: &str, provider: Provider) -> Result<Vec<SearchResult>, String> {
    let response: HackerNewsResponse =
        serde_json::from_str(body).map_err(|error| error.to_string())?;
    Ok(response
        .hits
        .into_iter()
        .filter_map(|hit| {
            let url = hit.url.or(hit.story_url)?;
            let title = hit
                .title
                .or(hit.story_title)
                .unwrap_or_else(|| "Untitled discussion".to_owned());
            Some(result(
                provider,
                title,
                url,
                hit.story_text.or(hit.comment_text).unwrap_or_default(),
                hit.created_at,
            ))
        })
        .collect())
}

async fn get_body(client: &Client, url: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let body = super::read_response_body(response)
        .await
        .map_err(|error| error.to_string())?;
    if !status.is_success() {
        log::error!(
            "provider request failed with HTTP {status}: {}",
            bounded_error(&body)
        );
        return Err(format!("provider request failed: HTTP {status}"));
    }
    Ok(body)
}

fn bounded_error(value: &str) -> String {
    value.chars().take(MAX_PROVIDER_ERROR_BYTES).collect()
}

fn validate_page(query: &SearchQuery) -> Result<(), Error> {
    query.page_offset(RESULTS_PER_PROVIDER as u32).map(|_| ())
}

fn page_offset(query: &SearchQuery) -> Result<u32, String> {
    query
        .page_offset(RESULTS_PER_PROVIDER as u32)
        .map_err(|error| error.to_string())
}

fn duckduckgo_params(query: &SearchQuery) -> Result<String, String> {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("q", query.query());
    serializer.append_pair("s", &page_offset(query)?.to_string());
    serializer.append_pair(
        "kp",
        duckduckgo_safe_search(query.safe_search.unwrap_or_default()),
    );
    if let Some(time_range) = query.time_range {
        serializer.append_pair("df", duckduckgo_time_range(time_range));
    }
    Ok(serializer.finish())
}

fn openalex_params(query: &SearchQuery) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("search", query.query());
    serializer.append_pair(
        "per-page",
        &query.mode().provider_result_limit().to_string(),
    );
    serializer.append_pair("page", &query.page.unwrap_or(1).to_string());
    serializer.finish()
}

fn hacker_news_params(query: &SearchQuery) -> Result<String, String> {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("query", query.query());
    serializer.append_pair(
        "hitsPerPage",
        &query.mode().provider_result_limit().to_string(),
    );
    serializer.append_pair(
        "page",
        &(page_offset(query)? / RESULTS_PER_PROVIDER as u32).to_string(),
    );
    Ok(serializer.finish())
}

fn duckduckgo_safe_search(safe_search: SafeSearch) -> &'static str {
    match safe_search {
        SafeSearch::Off => "1",
        SafeSearch::Moderate => "-1",
        SafeSearch::Strict | SafeSearch::Level3 | SafeSearch::Level4 => "-2",
    }
}

fn duckduckgo_time_range(time_range: TimeRange) -> &'static str {
    match time_range {
        TimeRange::Day => "d",
        TimeRange::Week => "w",
        TimeRange::Month => "m",
        TimeRange::Year => "y",
    }
}

fn parse_duckduckgo(body: &str, provider: Provider) -> Result<Vec<SearchResult>, String> {
    let document = Html::parse_document(body);
    let result_selector = Selector::parse("div.result").map_err(|error| error.to_string())?;
    let title_selector = Selector::parse("a.result__a").map_err(|error| error.to_string())?;
    let snippet_selector = Selector::parse("a.result__snippet, div.result__snippet")
        .map_err(|error| error.to_string())?;
    Ok(document
        .select(&result_selector)
        .filter_map(|result_element| {
            let title_element = result_element.select(&title_selector).next()?;
            let title = text(title_element.text());
            let url = title_element
                .value()
                .attr("href")
                .and_then(resolve_duckduckgo_url)?;
            let snippet = result_element
                .select(&snippet_selector)
                .next()
                .map(|element| text(element.text()))
                .unwrap_or_default();
            Some(result(provider, title, url, snippet, None))
        })
        .collect())
}

fn resolve_duckduckgo_url(raw: &str) -> Option<String> {
    let raw = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.to_owned()
    };
    let parsed = Url::parse(&raw).ok()?;
    if let Some(redirect) = parsed
        .query_pairs()
        .find(|(key, _)| key == "uddg")
        .map(|(_, value)| value.into_owned())
    {
        return safe_url(&redirect);
    }
    safe_url(&raw)
}

fn safe_url(raw: &str) -> Option<String> {
    let mut url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_fragment(None);
    Some(url.to_string())
}

fn result(
    provider: Provider,
    title: String,
    url: String,
    snippet: String,
    published_date: Option<String>,
) -> SearchResult {
    SearchResult {
        title: clean_text(&title),
        url,
        content: clean_text(&snippet),
        engine: Some(provider.name().to_owned()),
        engines: vec![provider.name().to_owned()],
        category: Some(provider.category().to_owned()),
        published_date,
        score: None,
    }
}

fn clean_text(raw: &str) -> String {
    let parsed = Html::parse_fragment(raw);
    let extracted = parsed.root_element().text().collect::<Vec<_>>().join(" ");
    let mut clean = String::with_capacity(extracted.len());
    for character in extracted.chars() {
        if character.is_control() {
            if character.is_whitespace() {
                clean.push(' ');
            }
        } else {
            clean.push(character);
        }
    }
    clean.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn text<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts.collect::<Vec<_>>().join(" ")
}

fn abstract_text(index: Option<std::collections::HashMap<String, Vec<usize>>>) -> String {
    let mut words = index
        .unwrap_or_default()
        .into_iter()
        .flat_map(|(word, positions)| {
            positions
                .into_iter()
                .map(move |position| (position, word.clone()))
        })
        .collect::<Vec<_>>();
    words.sort_by_key(|(position, _)| *position);
    let mut iter = words.into_iter().take(48).map(|(_, word)| word);
    match iter.next() {
        Some(first) => iter.fold(first, |mut acc, word| {
            acc.push(' ');
            acc.push_str(&word);
            acc
        }),
        None => String::new(),
    }
}

fn dedupe_and_rank(query: &str, results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut max_len = query.len().min(400);
    while !query.is_char_boundary(max_len) {
        max_len -= 1;
    }
    let query = &query[..max_len];

    let mut merged: BTreeMap<String, SearchResult> = BTreeMap::new();
    for mut result in results {
        let Some(key) = safe_url(&result.url) else {
            continue;
        };
        match merged.entry(key) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let existing = entry.get_mut();
                for engine in result.engines {
                    if !existing.engines.contains(&engine) {
                        existing.engines.push(engine);
                    }
                }
                if existing.engine.is_none() {
                    existing.engine = result.engine;
                }
                if existing.content.len() < result.content.len() {
                    existing.content = result.content;
                }
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                result.url = entry.key().clone();
                entry.insert(result);
            }
        }
    }
    let mut results = merged.into_values().collect::<Vec<_>>();
    rank_results(query, &mut results);
    results.sort_by(|left, right| {
        right
            .score
            .unwrap_or_default()
            .total_cmp(&left.score.unwrap_or_default())
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.url.cmp(&right.url))
    });
    results
}

fn rank_results(query: &str, results: &mut [SearchResult]) {
    let query_terms = tokenize(query);
    if query_terms.is_empty() || results.is_empty() {
        return;
    }
    let documents = results
        .iter()
        .map(|result| {
            tokenize(&format!(
                "{} {} {}",
                result.title, result.content, result.url
            ))
            .into_iter()
            .collect::<HashSet<_>>()
        })
        .collect::<Vec<_>>();
    let document_count = documents.len() as f64;
    let query_terms = query_terms.into_iter().collect::<HashSet<_>>();
    let mut term_idfs = Vec::with_capacity(query_terms.len());
    for term in &query_terms {
        let document_frequency = documents
            .iter()
            .filter(|document| document.contains(term))
            .count() as f64;
        let inverse_document_frequency =
            ((document_count + 1.0) / (document_frequency + 1.0)).ln() + 1.0;
        term_idfs.push((term, inverse_document_frequency));
    }
    for (index, result) in results.iter_mut().enumerate() {
        let title = tokenize(&result.title);
        let content = tokenize(&result.content);
        let url = tokenize(&result.url);
        let mut score = 0.0;
        for (term, inverse_document_frequency) in &term_idfs {
            let title_frequency = title.iter().filter(|token| *token == *term).count() as f64;
            let content_frequency = content.iter().filter(|token| *token == *term).count() as f64;
            let url_frequency = url.iter().filter(|token| *token == *term).count() as f64;
            score += inverse_document_frequency
                * (2.0 * title_frequency / title.len().max(1) as f64
                    + content_frequency / content.len().max(1) as f64
                    + 0.5 * url_frequency / url.len().max(1) as f64);
        }
        if documents[index].is_empty() {
            score = 0.0;
        }
        result.score = Some(score);
    }
}

fn tokenize(value: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of",
        "on", "or", "that", "the", "this", "to", "was", "were", "with",
    ];
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|token| !STOP_WORDS.contains(&token.as_str()))
        .collect()
}

#[derive(Deserialize)]
struct OpenAlexResponse {
    #[serde(default)]
    results: Vec<OpenAlexWork>,
}

#[derive(Deserialize)]
struct OpenAlexWork {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    doi: Option<String>,
    #[serde(default)]
    publication_year: Option<u32>,
    #[serde(default)]
    publication_date: Option<String>,
    #[serde(default)]
    primary_location: Option<OpenAlexLocation>,
    #[serde(default)]
    abstract_inverted_index: Option<std::collections::HashMap<String, Vec<usize>>>,
}

#[derive(Deserialize)]
struct OpenAlexLocation {
    #[serde(default)]
    landing_page_url: Option<String>,
    #[serde(default)]
    pdf_url: Option<String>,
}

#[derive(Deserialize)]
struct HackerNewsResponse {
    #[serde(default)]
    hits: Vec<HackerNewsHit>,
}

#[derive(Deserialize)]
struct HackerNewsHit {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    story_title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    story_url: Option<String>,
    #[serde(default)]
    story_text: Option<String>,
    #[serde(default)]
    comment_text: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_redirected_and_rejects_unsafe_urls() {
        let url = resolve_duckduckgo_url(
            "https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fguide%23part",
        )
        .expect("redirect URL");
        assert_eq!(url, "https://example.com/guide");
        assert!(safe_url("javascript:alert(1)").is_none());
    }

    #[test]
    fn deduplicates_urls_and_merges_engines() {
        let first = result(
            Provider::Web,
            "Rust guide".to_owned(),
            "https://example.com/guide#top".to_owned(),
            "guide".to_owned(),
            None,
        );
        let second = result(
            Provider::Academic,
            "Rust guide".to_owned(),
            "https://example.com/guide".to_owned(),
            "longer guide".to_owned(),
            None,
        );
        let merged = dedupe_and_rank("rust", vec![first, second]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].engines, ["duckduckgo", "openalex"]);
        assert_eq!(merged[0].content, "longer guide");
    }

    #[test]
    fn parses_duckduckgo_fixture() {
        let results = parse_duckduckgo(
            r#"<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Frust">Rust</a><a class="result__snippet">A language</a></div>"#,
            Provider::Web,
        )
        .expect("valid fixture");
        assert_eq!(results[0].title, "Rust");
        assert_eq!(results[0].content, "A language");
        assert_eq!(results[0].url, "https://example.com/rust");
    }

    #[test]
    fn parses_openalex_fixture() {
        let results = parse_openalex(
            r#"{"results":[{"title":"Async Rust","doi":"https://doi.org/10.1234/rust","publication_year":2026,"abstract_inverted_index":{"Rust":[1],"async":[0]}}]}"#,
            Provider::Academic,
        )
        .expect("valid OpenAlex fixture");
        assert_eq!(results[0].title, "Async Rust");
        assert_eq!(results[0].url, "https://doi.org/10.1234/rust");
        assert!(results[0].content.contains("async Rust"));
    }

    #[test]
    fn parses_hacker_news_fixture_and_sanitizes_html() {
        let results = parse_hacker_news(
            r#"{"hits":[{"story_title":"Rust","story_url":"https://news.ycombinator.com/item?id=1","comment_text":"<p>Fast\u0007 <strong>async</strong></p>"}]}"#,
            Provider::Discussions,
        )
        .expect("valid Hacker News fixture");
        assert_eq!(results[0].title, "Rust");
        assert_eq!(results[0].content, "Fast async");
    }

    #[test]
    fn provider_parameters_use_native_values_and_checked_pages() {
        let query = SearchQuery::new("rust")
            .with_safe_search(SafeSearch::Moderate)
            .with_time_range(TimeRange::Week)
            .with_page(2);
        let params = duckduckgo_params(&query).expect("valid provider params");
        assert!(params.contains("kp=-1"));
        assert!(params.contains("df=w"));
        assert!(params.contains("s=10"));
        assert!(duckduckgo_params(&SearchQuery::new("rust").with_page(u32::MAX)).is_err());
    }

    #[test]
    fn provider_selection_honors_explicit_engines_and_mode() {
        let query = SearchQuery::new("rust")
            .with_engines(["openalex", "hacker-news", "duckduckgo"])
            .with_mode(crate::SearchMode::Speed);
        let selected = providers(&query);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name(), "openalex");
    }

    #[test]
    fn provider_selection_uses_aliases_and_falls_back_from_unknown_engines() {
        let aliased = SearchQuery::new("rust")
            .with_engines(["ddg", "HN", "academic"])
            .with_mode(crate::SearchMode::Quality);
        let selected = providers(&aliased);
        assert_eq!(
            selected
                .iter()
                .map(|provider| provider.name())
                .collect::<Vec<_>>(),
            ["duckduckgo", "hacker-news", "openalex"]
        );

        let unknown = SearchQuery::new("rust")
            .with_engines(["bing", "google"])
            .with_categories("science,social media");
        let selected = providers(&unknown);
        assert_eq!(
            selected
                .iter()
                .map(|provider| provider.name())
                .collect::<Vec<_>>(),
            ["openalex", "hacker-news"]
        );
    }

    #[test]
    fn dedupe_and_rank_truncates_long_queries_preventing_dos() {
        let long_query = "a".repeat(1000);
        let res = result(
            Provider::Web,
            "Rust".to_owned(),
            "https://example.com".to_owned(),
            "A language".to_owned(),
            None,
        );
        let start = std::time::Instant::now();
        let ranked = dedupe_and_rank(&long_query, vec![res]);
        let elapsed = start.elapsed();

        assert_eq!(ranked.len(), 1);
        // It shouldn't take more than a fraction of a second to process 400 bytes,
        // vs potentially hanging for a long time if it processed 1000 bytes.
        assert!(elapsed.as_millis() < 100);
    }

    #[test]
    fn ranking_uses_token_frequency_and_ignores_stop_words() {
        let results = dedupe_and_rank(
            "rust async",
            vec![
                result(
                    Provider::Web,
                    "Rust async guide".to_owned(),
                    "https://example.com/guide".to_owned(),
                    "async Rust futures".to_owned(),
                    None,
                ),
                result(
                    Provider::Web,
                    "The guide".to_owned(),
                    "https://example.com/other".to_owned(),
                    "A guide about programming".to_owned(),
                    None,
                ),
            ],
        );
        assert!(results[0].score.unwrap_or_default() > results[1].score.unwrap_or_default());
    }
}
