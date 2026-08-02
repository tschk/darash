use std::time::Duration;

use futures_util::StreamExt;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::{form_urlencoded, Url};

pub const DEFAULT_ENDPOINT: &str = "http://localhost:8080";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SafeSearch {
    #[default]
    Off,
    Moderate,
    Strict,
}

impl SafeSearch {
    fn value(self) -> &'static str {
        match self {
            Self::Off => "0",
            Self::Moderate => "1",
            Self::Strict => "2",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeRange {
    Day,
    Week,
    Month,
    Year,
}

impl TimeRange {
    fn value(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Speed,
    #[default]
    Balanced,
    Quality,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchSource {
    #[default]
    Web,
    Academic,
    Discussions,
}

impl SearchSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Academic => "academic",
            Self::Discussions => "discussions",
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

impl SearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Speed => "speed",
            Self::Balanced => "balanced",
            Self::Quality => "quality",
        }
    }

    fn result_limit(self) -> usize {
        match self {
            Self::Speed => 5,
            Self::Balanced => 10,
            Self::Quality => 20,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchRequest {
    query: String,
    mode: SearchMode,
    sources: Vec<SearchSource>,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            mode: SearchMode::default(),
            sources: vec![SearchSource::default()],
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn mode(&self) -> SearchMode {
        self.mode
    }

    pub fn sources(&self) -> &[SearchSource] {
        &self.sources
    }

    pub fn with_mode(mut self, mode: SearchMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_source(mut self, source: SearchSource) -> Self {
        self.sources = vec![source];
        self
    }

    pub fn with_sources<I>(mut self, sources: I) -> Self
    where
        I: IntoIterator<Item = SearchSource>,
    {
        self.sources = sources.into_iter().collect();
        self
    }

    fn search_query(&self) -> SearchQuery {
        let categories = self
            .sources
            .iter()
            .map(|source| source.category())
            .collect::<Vec<_>>()
            .join(",");
        SearchQuery::new(self.query.clone()).with_categories(categories)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuery {
    query: String,
    categories: Option<String>,
    engines: Vec<String>,
    language: Option<String>,
    page: Option<u32>,
    safe_search: Option<SafeSearch>,
    time_range: Option<TimeRange>,
}

impl SearchQuery {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            categories: None,
            engines: Vec::new(),
            language: None,
            page: None,
            safe_search: None,
            time_range: None,
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn with_categories(mut self, categories: impl Into<String>) -> Self {
        self.categories = Some(categories.into());
        self
    }

    pub fn with_engines<I, S>(mut self, engines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.engines = engines.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    pub fn with_page(mut self, page: u32) -> Self {
        self.page = Some(page);
        self
    }

    pub fn with_safe_search(mut self, safe_search: SafeSearch) -> Self {
        self.safe_search = Some(safe_search);
        self
    }

    pub fn with_time_range(mut self, time_range: TimeRange) -> Self {
        self.time_range = Some(time_range);
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut serializer = form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("q", &self.query);
        serializer.append_pair("format", "json");
        if let Some(categories) = &self.categories {
            serializer.append_pair("categories", categories);
        }
        if !self.engines.is_empty() {
            let engines = self.engines.join(",");
            serializer.append_pair("engines", &engines);
        }
        if let Some(language) = &self.language {
            serializer.append_pair("language", language);
        }
        if let Some(page) = self.page {
            serializer.append_pair("pageno", &page.to_string());
        }
        if let Some(safe_search) = self.safe_search {
            serializer.append_pair("safesearch", safe_search.value());
        }
        if let Some(time_range) = self.time_range {
            serializer.append_pair("time_range", time_range.value());
        }
        serializer.finish()
    }
}

#[derive(Clone, Debug)]
pub struct SearchConfig {
    endpoint: Url,
    timeout: Duration,
}

impl SearchConfig {
    pub fn new(endpoint: impl AsRef<str>) -> Result<Self, Error> {
        let endpoint = Url::parse(endpoint.as_ref())
            .map_err(|error| Error::InvalidEndpoint(error.to_string()))?;
        Self::from_url(endpoint)
    }

    pub fn from_url(endpoint: Url) -> Result<Self, Error> {
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(Error::InvalidEndpoint(
                "endpoint must use http or https".to_owned(),
            ));
        }
        if !endpoint.username().is_empty() || endpoint.password().is_some() {
            return Err(Error::InvalidEndpoint(
                "endpoint credentials are not supported".to_owned(),
            ));
        }
        Ok(Self {
            endpoint,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[derive(Clone)]
pub struct SearchClient {
    http: reqwest::Client,
    config: SearchConfig,
}

impl SearchClient {
    pub fn new(endpoint: impl AsRef<str>) -> Result<Self, Error> {
        Self::from_config(SearchConfig::new(endpoint)?)
    }

    pub fn local() -> Result<Self, Error> {
        Self::new(DEFAULT_ENDPOINT)
    }

    pub fn from_config(config: SearchConfig) -> Result<Self, Error> {
        if config.timeout.is_zero() {
            return Err(Error::InvalidTimeout);
        }
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(Error::ClientBuild)?;
        Ok(Self { http, config })
    }

    pub fn config(&self) -> &SearchConfig {
        &self.config
    }

    pub async fn search(&self, query: &SearchQuery) -> Result<SearchResponse, Error> {
        if query.query.trim().is_empty() {
            return Err(Error::EmptyQuery);
        }
        if query.page == Some(0) {
            return Err(Error::InvalidPage);
        }

        let mut url = self.search_url();
        url.set_query(Some(&query.to_query_string()));
        let response = self.http.get(url).send().await.map_err(Error::Request)?;
        let status = response.status();
        let body = read_response_body(response).await?;
        if !status.is_success() {
            return Err(Error::HttpStatus { status, body });
        }
        let mut response: SearchResponse = serde_json::from_str(&body).map_err(Error::Decode)?;
        if response.answer.is_none() {
            response.answer = response.answers.first().cloned();
        }
        if response.sources.is_empty() {
            response.sources = response.citations();
        }
        Ok(response)
    }

    pub async fn search_request(&self, request: &SearchRequest) -> Result<SearchResponse, Error> {
        let mut response = self.search(&request.search_query()).await?;
        limit_response(&mut response, request.mode);
        Ok(response)
    }

    fn search_url(&self) -> Url {
        let mut url = self.config.endpoint.clone();
        let base_path = url.path().trim_end_matches('/');
        let path = if base_path.is_empty() {
            "/search".to_owned()
        } else if base_path == "/search" || base_path.ends_with("/search") {
            base_path.to_owned()
        } else {
            format!("{base_path}/search")
        };
        url.set_path(&path);
        url
    }
}

fn limit_response(response: &mut SearchResponse, mode: SearchMode) {
    let limit = mode.result_limit();
    response.results.truncate(limit);
    response.sources.truncate(limit);
}

async fn read_response_body(response: reqwest::Response) -> Result<String, Error> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(Error::ResponseTooLarge);
    }

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(Error::Request)?;
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(Error::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|error| Error::InvalidResponseEncoding(error.to_string()))
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub number_of_results: u64,
    #[serde(default)]
    pub results: Vec<SearchResult>,
    #[serde(default)]
    pub answers: Vec<String>,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub sources: Vec<Citation>,
    #[serde(default)]
    pub corrections: Vec<String>,
    #[serde(default)]
    pub suggestions: Vec<String>,
}

impl SearchResponse {
    pub fn citations(&self) -> Vec<Citation> {
        self.results.iter().map(SearchResult::citation).collect()
    }

    pub fn cited_sources(&self) -> Vec<Citation> {
        if self.sources.is_empty() {
            self.citations()
        } else {
            self.sources.clone()
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SearchResult {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub engines: Vec<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(rename = "publishedDate", default)]
    pub published_date: Option<String>,
    #[serde(default)]
    pub score: Option<f64>,
}

impl SearchResult {
    pub fn citation(&self) -> Citation {
        Citation {
            title: self.title.clone(),
            url: self.url.clone(),
            snippet: self.content.clone(),
            source: self
                .engine
                .clone()
                .or_else(|| self.engines.first().cloned()),
            published_date: self.published_date.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Citation {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub source: Option<String>,
    pub published_date: Option<String>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("timeout must be greater than zero")]
    InvalidTimeout,
    #[error("query must not be empty")]
    EmptyQuery,
    #[error("page must be at least 1")]
    InvalidPage,
    #[error("failed to build HTTP client: {0}")]
    ClientBuild(#[source] reqwest::Error),
    #[error("search request failed: {0}")]
    Request(#[source] reqwest::Error),
    #[error("search service returned HTTP {status}: {body}")]
    HttpStatus { status: StatusCode, body: String },
    #[error("search response exceeded the {MAX_RESPONSE_BYTES}-byte limit")]
    ResponseTooLarge,
    #[error("search response was not valid UTF-8: {0}")]
    InvalidResponseEncoding(String),
    #[error("invalid search response: {0}")]
    Decode(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_carries_mode_and_sources_into_searxng_query() {
        let request = SearchRequest::new("rust async")
            .with_mode(SearchMode::Quality)
            .with_sources([SearchSource::Academic, SearchSource::Discussions]);

        assert_eq!(request.mode(), SearchMode::Quality);
        assert_eq!(
            request.sources(),
            [SearchSource::Academic, SearchSource::Discussions]
        );
        assert_eq!(
            request.search_query().to_query_string(),
            "q=rust+async&format=json&categories=science%2Csocial+media"
        );
    }

    #[test]
    fn mode_limits_results_without_replacing_backend_sources() {
        let source = Citation {
            title: "Backend source".to_owned(),
            url: "https://example.com".to_owned(),
            snippet: "source".to_owned(),
            source: None,
            published_date: None,
        };
        let response = |count| SearchResponse {
            query: "rust".to_owned(),
            number_of_results: count,
            results: (0..count)
                .map(|index| SearchResult {
                    title: index.to_string(),
                    url: format!("https://example.com/{index}"),
                    content: String::new(),
                    engine: None,
                    engines: Vec::new(),
                    category: None,
                    published_date: None,
                    score: None,
                })
                .collect(),
            answers: Vec::new(),
            answer: None,
            sources: vec![source.clone(); count as usize],
            corrections: Vec::new(),
            suggestions: Vec::new(),
        };

        for (mode, limit) in [
            (SearchMode::Speed, 5),
            (SearchMode::Balanced, 10),
            (SearchMode::Quality, 20),
        ] {
            let mut response = response(25);
            limit_response(&mut response, mode);
            assert_eq!(response.results.len(), limit);
            assert_eq!(response.sources.len(), limit);
            assert_eq!(response.sources[0], source);
        }
    }

    #[test]
    fn query_serializes_searxng_parameters() {
        let query = SearchQuery::new("rust async")
            .with_categories("general,news")
            .with_engines(["brave", "duckduckgo"])
            .with_language("en-US")
            .with_page(2)
            .with_safe_search(SafeSearch::Strict)
            .with_time_range(TimeRange::Week);

        assert_eq!(
            query.to_query_string(),
            "q=rust+async&format=json&categories=general%2Cnews&engines=brave%2Cduckduckgo&language=en-US&pageno=2&safesearch=2&time_range=week"
        );
    }

    #[test]
    fn endpoint_appends_search_path_once() {
        let root = SearchClient::new("http://localhost:8080").expect("valid endpoint");
        let existing = SearchClient::new("http://localhost:8080/search/").expect("valid endpoint");

        assert_eq!(root.search_url().as_str(), "http://localhost:8080/search");
        assert_eq!(
            existing.search_url().as_str(),
            "http://localhost:8080/search"
        );
    }

    #[test]
    fn endpoint_rejects_credentials_and_zero_timeout() {
        assert!(SearchConfig::new("http://user:pass@localhost:8080").is_err());
        let config = SearchConfig::new("http://localhost:8080")
            .expect("valid endpoint")
            .with_timeout(Duration::ZERO);
        assert!(SearchClient::from_config(config).is_err());
    }

    #[test]
    fn response_parses_and_projects_citations() {
        let response: SearchResponse = serde_json::from_str(
            r#"{
                "query": "rust async",
                "number_of_results": 1,
                "answers": ["Async Rust is Rust with futures."],
                "results": [{
                    "title": "Async Rust",
                    "url": "https://example.com/rust",
                    "content": "A guide to async Rust.",
                    "engine": "brave",
                    "engines": ["brave", "duckduckgo"],
                    "category": "general",
                    "publishedDate": "2026-01-02",
                    "score": 1.25
                }],
                "suggestions": ["rust futures"]
            }"#,
        )
        .expect("valid SearxNG response");

        assert_eq!(response.query, "rust async");
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.answers[0], "Async Rust is Rust with futures.");
        assert_eq!(response.suggestions, ["rust futures"]);
        assert_eq!(
            response.citations(),
            vec![Citation {
                title: "Async Rust".to_owned(),
                url: "https://example.com/rust".to_owned(),
                snippet: "A guide to async Rust.".to_owned(),
                source: Some("brave".to_owned()),
                published_date: Some("2026-01-02".to_owned()),
            }]
        );
    }
}
