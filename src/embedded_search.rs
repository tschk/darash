use crate::{Error, SearchQuery, SearchResponse, SearchResult};
use futures_util::future::join_all;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use std::collections::BTreeMap;
use url::{form_urlencoded, Url};

const DUCKDUCKGO_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const OPENALEX_ENDPOINT: &str = "https://api.openalex.org/works";
const HACKER_NEWS_ENDPOINT: &str = "https://hn.algolia.com/api/v1/search";
const RESULTS_PER_PROVIDER: usize = 10;
const USER_AGENT: &str = "darash-search/0.3";

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
}

pub(crate) async fn search(client: &Client, query: &SearchQuery) -> Result<SearchResponse, Error> {
    let providers = providers(query);
    let outcomes = join_all(
        providers
            .iter()
            .copied()
            .map(|provider| fetch_provider(client, provider, query)),
    )
    .await;

    let mut results = Vec::new();
    let mut failures = Vec::new();
    for outcome in outcomes {
        match outcome {
            Ok(provider_results) => results.extend(provider_results),
            Err(error) => failures.push(error),
        }
    }
    if results.is_empty() && !failures.is_empty() {
        return Err(Error::Local(failures.join("; ")));
    }

    let results = dedupe_and_rank(query.query(), results);
    let sources = results.iter().map(SearchResult::citation).collect();
    Ok(SearchResponse {
        query: query.query().to_owned(),
        number_of_results: results.len() as u64,
        results,
        answers: Vec::new(),
        answer: None,
        sources,
        corrections: Vec::new(),
        suggestions: Vec::new(),
    })
}

fn providers(query: &SearchQuery) -> Vec<Provider> {
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
}

async fn fetch_provider(
    client: &Client,
    provider: Provider,
    query: &SearchQuery,
) -> Result<Vec<SearchResult>, String> {
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
) -> Result<Vec<SearchResult>, String> {
    let params = {
        let mut serializer = form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("q", query.query());
        serializer.append_pair(
            "s",
            &((query.page.unwrap_or(1) - 1) * RESULTS_PER_PROVIDER as u32).to_string(),
        );
        serializer.append_pair("kp", query.safe_search.unwrap_or_default().value());
        if let Some(time_range) = query.time_range {
            serializer.append_pair("df", time_range.value());
        }
        serializer.finish()
    };
    let url = format!("{DUCKDUCKGO_ENDPOINT}?{params}");
    let body = get_body(client, &url).await?;
    parse_duckduckgo(&body, provider)
}

async fn fetch_openalex(
    client: &Client,
    provider: Provider,
    query: &SearchQuery,
) -> Result<Vec<SearchResult>, String> {
    let params = {
        let mut serializer = form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("search", query.query());
        serializer.append_pair("per-page", &RESULTS_PER_PROVIDER.to_string());
        serializer.append_pair("page", &query.page.unwrap_or(1).to_string());
        serializer.finish()
    };
    let url = format!("{OPENALEX_ENDPOINT}?{params}");
    let body = get_body(client, &url).await?;
    let response: OpenAlexResponse =
        serde_json::from_str(&body).map_err(|error| error.to_string())?;
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
) -> Result<Vec<SearchResult>, String> {
    let params = {
        let mut serializer = form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("query", query.query());
        serializer.append_pair("hitsPerPage", &RESULTS_PER_PROVIDER.to_string());
        serializer.append_pair("page", &(query.page.unwrap_or(1) - 1).to_string());
        serializer.finish()
    };
    let url = format!("{HACKER_NEWS_ENDPOINT}?{params}");
    let body = get_body(client, &url).await?;
    let response: HackerNewsResponse =
        serde_json::from_str(&body).map_err(|error| error.to_string())?;
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
        return Err(format!("HTTP {status}: {body}"));
    }
    Ok(body)
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
        title: text(title.split_whitespace()),
        url,
        content: text(snippet.split_whitespace()),
        engine: Some(provider.name().to_owned()),
        engines: vec![provider.name().to_owned()],
        category: Some(provider.category().to_owned()),
        published_date,
        score: None,
    }
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
    words
        .into_iter()
        .take(48)
        .map(|(_, word)| word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn dedupe_and_rank(query: &str, results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut merged: BTreeMap<String, SearchResult> = BTreeMap::new();
    for mut result in results {
        let Some(key) = safe_url(&result.url) else {
            continue;
        };
        result.url = key.clone();
        result.score = Some(relevance(query, &result));
        if let Some(existing) = merged.get_mut(&key) {
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
            existing.score = Some(
                existing
                    .score
                    .unwrap_or_default()
                    .max(result.score.unwrap_or_default()),
            );
        } else {
            merged.insert(key, result);
        }
    }
    let mut results = merged.into_values().collect::<Vec<_>>();
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

fn relevance(query: &str, result: &SearchResult) -> f64 {
    let terms = query
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let title = result.title.to_ascii_lowercase();
    let content = result.content.to_ascii_lowercase();
    let url = result.url.to_ascii_lowercase();
    let matched = terms
        .iter()
        .filter(|term| {
            title.contains(term.as_str())
                || content.contains(term.as_str())
                || url.contains(term.as_str())
        })
        .count();
    let title_matches = terms
        .iter()
        .filter(|term| title.contains(term.as_str()))
        .count();
    (matched as f64 / terms.len().max(1) as f64) + (title_matches as f64 * 0.25)
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
}
