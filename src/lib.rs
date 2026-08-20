use std::time::Duration;

use futures_util::StreamExt;
use reqwest::StatusCode;
use serde::{de::Deserializer, Deserialize, Serialize};
use thiserror::Error;
use url::{form_urlencoded, Url};

mod cache;
mod embedded_search;
mod websurfx;

pub use websurfx::{
    build_search_url as build_websurfx_search_url, WebsurfxEngineError, WebsurfxError,
    WebsurfxMappedResponse, WebsurfxMetadata, WebsurfxQuery, WebsurfxResponse, WebsurfxResult,
    WebsurfxSearchResponse, WebsurfxSearchResult,
};

pub const DEFAULT_ENDPOINT: &str = "http://localhost:8080";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SafeSearch {
    #[default]
    Off,
    Moderate,
    Strict,
    Level3,
    Level4,
}

impl SafeSearch {
    pub const fn level(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Moderate => 1,
            Self::Strict => 2,
            Self::Level3 => 3,
            Self::Level4 => 4,
        }
    }

    pub const fn from_level(level: u8) -> Option<Self> {
        match level {
            0 => Some(Self::Off),
            1 => Some(Self::Moderate),
            2 => Some(Self::Strict),
            3 => Some(Self::Level3),
            4 => Some(Self::Level4),
            _ => None,
        }
    }

    fn value(self) -> &'static str {
        match self {
            Self::Off => "0",
            Self::Moderate => "1",
            Self::Strict => "2",
            Self::Level3 => "3",
            Self::Level4 => "4",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeRange {
    Day,
    Week,
    Month,
    Year,
}

impl TimeRange {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    fn value(self) -> &'static str {
        self.as_str()
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

    pub const fn result_limit(self) -> usize {
        match self {
            Self::Speed => 5,
            Self::Balanced => 10,
            Self::Quality => 20,
        }
    }

    pub const fn provider_limit(self) -> usize {
        match self {
            Self::Speed => 1,
            Self::Balanced => 2,
            Self::Quality => 3,
        }
    }

    pub const fn provider_concurrency(self) -> usize {
        match self {
            Self::Speed => 1,
            Self::Balanced => 2,
            Self::Quality => 3,
        }
    }

    pub const fn provider_result_limit(self) -> usize {
        match self {
            Self::Speed => 5,
            Self::Balanced => 10,
            Self::Quality => 10,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchRequest {
    query: String,
    #[serde(default)]
    mode: SearchMode,
    #[serde(default = "default_sources")]
    sources: Vec<SearchSource>,
}

fn default_sources() -> Vec<SearchSource> {
    vec![SearchSource::default()]
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
        let mut selected = Vec::new();
        for source in sources {
            if !selected.contains(&source) {
                selected.push(source);
            }
        }
        self.sources = if selected.is_empty() {
            vec![SearchSource::default()]
        } else {
            selected
        };
        self
    }

    fn search_query(&self) -> SearchQuery {
        let categories = self
            .sources
            .iter()
            .map(|source| source.category())
            .collect::<Vec<_>>()
            .join(",");
        SearchQuery::new(self.query.clone())
            .with_categories(categories)
            .with_mode(self.mode)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuery {
    query: String,
    categories: Option<String>,
    mode: SearchMode,
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
            mode: SearchMode::default(),
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

    pub fn mode(&self) -> SearchMode {
        self.mode
    }

    pub fn with_mode(mut self, mode: SearchMode) -> Self {
        self.mode = mode;
        self
    }

    pub(crate) fn page_offset(&self, per_page: u32) -> Result<u32, Error> {
        match self.page {
            None => Ok(0),
            Some(0) => Err(Error::InvalidPage),
            Some(page) => page
                .checked_sub(1)
                .and_then(|page| page.checked_mul(per_page))
                .ok_or(Error::PageOverflow),
        }
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

    pub fn filters(&self) -> SearchFilters {
        SearchFilters {
            safe_search_level: self.safe_search.unwrap_or_default().level(),
            time_range: self.time_range,
            filtered: false,
            disallowed: false,
            no_providers_selected: false,
        }
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
    cache_capacity: usize,
    cache_ttl: Duration,
    blocklist: Vec<String>,
    allowlist: Vec<String>,
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
            cache_capacity: 64,
            cache_ttl: Duration::from_secs(60),
            blocklist: Vec::new(),
            allowlist: Vec::new(),
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_cache(mut self, capacity: usize, ttl: Duration) -> Self {
        self.cache_capacity = capacity;
        self.cache_ttl = ttl;
        self
    }

    pub fn with_blocklist<I, S>(mut self, terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.blocklist = terms.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_allowlist<I, S>(mut self, terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowlist = terms.into_iter().map(Into::into).collect();
        self
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn cache_settings(&self) -> (usize, Duration) {
        (self.cache_capacity, self.cache_ttl)
    }
}

#[derive(Clone)]
pub struct SearchClient {
    http: reqwest::Client,
    config: SearchConfig,
    embedded: bool,
    cache: cache::TtlCache<String, SearchResponse>,
}

impl SearchClient {
    pub fn new(endpoint: impl AsRef<str>) -> Result<Self, Error> {
        Self::from_config(SearchConfig::new(endpoint)?)
    }

    pub fn local() -> Result<Self, Error> {
        let mut client = Self::from_config(SearchConfig::new(DEFAULT_ENDPOINT)?)?;
        client.embedded = true;
        Ok(client)
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
        Ok(Self {
            http,
            cache: cache::TtlCache::new(config.cache_capacity, config.cache_ttl),
            config,
            embedded: false,
        })
    }

    pub fn config(&self) -> &SearchConfig {
        &self.config
    }

    pub fn clear_cache(&self) {
        self.cache.clear();
    }

    pub fn cached_entries(&self) -> usize {
        self.cache.len()
    }

    pub async fn search(&self, query: &SearchQuery) -> Result<SearchResponse, Error> {
        if query.query.trim().is_empty() {
            return Err(Error::EmptyQuery);
        }
        if query.page == Some(0) {
            return Err(Error::InvalidPage);
        }
        query.page_offset(10)?;

        let cache_key = format!(
            "{}:{}:{}",
            self.embedded,
            query.mode().as_str(),
            query.to_query_string()
        );
        if let Some(response) = self.cache.get(&cache_key) {
            return Ok(response);
        }

        if self.embedded {
            let outcome =
                embedded_search::search_with_outcome(&self.http, query, &self.config).await?;
            let mut response = outcome.response;
            for failure in outcome.failures {
                if !response
                    .provider_status
                    .iter()
                    .any(|status| status.provider == failure.provider)
                {
                    response
                        .provider_status
                        .push(ProviderStatus::failed(failure.provider, failure.error));
                }
            }
            self.cache.insert(cache_key, response.clone());
            return Ok(response);
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
        if response.number_of_results == 0 {
            response.number_of_results = response.results.len() as u64;
        }
        if response.answer.is_none() {
            response.answer = response.answers.first().cloned();
        }
        if response.sources.is_empty() {
            response.sources = response.citations();
        }
        response.filters = query.filters();
        self.cache.insert(cache_key, response.clone());
        Ok(response)
    }

    pub async fn search_request(&self, request: &SearchRequest) -> Result<SearchResponse, Error> {
        let mut response = self.search(&request.search_query()).await?;
        limit_response(&mut response, request.mode);
        Ok(response)
    }

    pub async fn search_websurfx(
        &self,
        query: &WebsurfxQuery,
    ) -> Result<WebsurfxMappedResponse, Error> {
        let url = websurfx::build_search_url(self.config.endpoint(), query)
            .map_err(|error| Error::InvalidEndpoint(error.to_string()))?;
        let response = self.http.get(url).send().await.map_err(Error::Request)?;
        let status = response.status();
        let body = read_response_body(response).await?;
        if !status.is_success() {
            return Err(Error::HttpStatus { status, body });
        }
        let response: WebsurfxSearchResponse =
            serde_json::from_str(&body).map_err(Error::Decode)?;
        Ok(response.into_search_response(query.query()))
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderStatusKind {
    Success,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderStatus {
    pub provider: String,
    pub status: ProviderStatusKind,
    #[serde(default)]
    pub result_count: u64,
    #[serde(default)]
    pub error: Option<String>,
}

impl ProviderStatus {
    pub fn success(provider: impl Into<String>, result_count: usize) -> Self {
        Self {
            provider: provider.into(),
            status: ProviderStatusKind::Success,
            result_count: result_count as u64,
            error: None,
        }
    }

    pub fn failed(provider: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            status: ProviderStatusKind::Failed,
            result_count: 0,
            error: Some(error.into()),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchFilters {
    #[serde(default)]
    pub safe_search_level: u8,
    #[serde(default)]
    pub time_range: Option<TimeRange>,
    #[serde(default)]
    pub filtered: bool,
    #[serde(default)]
    pub disallowed: bool,
    #[serde(default)]
    pub no_providers_selected: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub number_of_results: u64,
    #[serde(default)]
    pub results: Vec<SearchResult>,
    #[serde(default, deserialize_with = "deserialize_answers")]
    pub answers: Vec<String>,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub sources: Vec<Citation>,
    #[serde(default)]
    pub corrections: Vec<String>,
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(default)]
    pub provider_status: Vec<ProviderStatus>,
    #[serde(default)]
    pub filters: SearchFilters,
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
    #[serde(deserialize_with = "deserialize_string_or_empty")]
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

fn deserialize_string_or_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|value| value.unwrap_or_default())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawAnswer {
    Text(String),
    Object {
        answer: Option<String>,
        content: Option<String>,
        text: Option<String>,
    },
}

fn deserialize_answers<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let answers = Option::<Vec<RawAnswer>>::deserialize(deserializer)?.unwrap_or_default();
    Ok(answers
        .into_iter()
        .filter_map(|answer| match answer {
            RawAnswer::Text(text) => Some(text),
            RawAnswer::Object {
                answer,
                content,
                text,
            } => answer.or(content).or(text),
        })
        .collect())
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
    #[error("page is too large for the provider offset")]
    PageOverflow,
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
    #[error("embedded search failed: {0}")]
    Local(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

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
    fn empty_sources_fall_back_to_web_and_duplicates_are_removed() {
        let request = SearchRequest::new("rust")
            .with_sources([SearchSource::Academic, SearchSource::Academic]);
        assert_eq!(request.sources(), [SearchSource::Academic]);

        let request = SearchRequest::new("rust").with_sources([]);
        assert_eq!(request.sources(), [SearchSource::Web]);
        assert_eq!(
            request.search_query().to_query_string(),
            "q=rust&format=json&categories=general"
        );
    }

    #[test]
    fn request_deserializes_with_vane_defaults() {
        let request: SearchRequest =
            serde_json::from_str(r#"{"query":"rust"}"#).expect("query-only request");
        assert_eq!(request.mode(), SearchMode::Balanced);
        assert_eq!(request.sources(), [SearchSource::Web]);
    }

    #[test]
    fn safe_search_supports_levels_zero_through_four() {
        for level in 0..=4 {
            assert_eq!(
                SafeSearch::from_level(level)
                    .expect("supported level")
                    .level(),
                level
            );
        }
        assert_eq!(SafeSearch::from_level(5), None);
        assert_eq!(SafeSearch::Level4.value(), "4");
    }

    #[test]
    fn query_carries_mode_filters_and_checked_page_offsets() {
        let query = SearchQuery::new("rust")
            .with_mode(SearchMode::Quality)
            .with_safe_search(SafeSearch::Level3)
            .with_time_range(TimeRange::Week)
            .with_page(3);
        assert_eq!(query.mode(), SearchMode::Quality);
        assert_eq!(query.filters().safe_search_level, 3);
        assert_eq!(query.filters().time_range, Some(TimeRange::Week));
        assert_eq!(query.page_offset(10).expect("page offset"), 20);
        assert_eq!(query.mode().provider_result_limit(), 10);

        let oversized = SearchQuery::new("rust").with_page(u32::MAX);
        assert!(matches!(
            oversized.page_offset(10),
            Err(Error::PageOverflow)
        ));
    }

    #[test]
    fn search_modes_expose_retrieval_limits() {
        assert_eq!(SearchMode::Speed.provider_limit(), 1);
        assert_eq!(SearchMode::Speed.provider_concurrency(), 1);
        assert_eq!(SearchMode::Balanced.provider_limit(), 2);
        assert_eq!(SearchMode::Balanced.provider_concurrency(), 2);
        assert_eq!(SearchMode::Quality.provider_limit(), 3);
        assert_eq!(SearchMode::Quality.provider_concurrency(), 3);
        assert_eq!(SearchMode::Quality.provider_result_limit(), 10);
    }

    #[test]
    fn provider_status_is_additive_and_serde_compatible() {
        let status = ProviderStatus::failed("openalex", "rate limited");
        let response: SearchResponse =
            serde_json::from_str(r#"{"query":"rust"}"#).expect("legacy response remains readable");
        assert!(response.provider_status.is_empty());
        assert_eq!(status.status, ProviderStatusKind::Failed);
        assert_eq!(status.error.as_deref(), Some("rate limited"));
        let encoded = serde_json::to_value(status).expect("status serializes");
        assert_eq!(encoded["provider"], "openalex");
        assert_eq!(encoded["status"], "failed");
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
            provider_status: Vec::new(),
            filters: SearchFilters::default(),
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
    fn search_config_from_url_validates_scheme_and_credentials() {
        // Valid URLs
        let valid_http = Url::parse("http://localhost:8080").unwrap();
        assert!(SearchConfig::from_url(valid_http).is_ok());

        let valid_https = Url::parse("https://example.com/search").unwrap();
        assert!(SearchConfig::from_url(valid_https).is_ok());

        // Invalid scheme
        let invalid_scheme = Url::parse("ftp://localhost:8080").unwrap();
        let err = SearchConfig::from_url(invalid_scheme).unwrap_err();
        assert!(matches!(err, Error::InvalidEndpoint(msg) if msg == "endpoint must use http or https"));

        // URL with credentials
        let url_with_creds = Url::parse("http://user:pass@localhost:8080").unwrap();
        let err = SearchConfig::from_url(url_with_creds).unwrap_err();
        assert!(matches!(err, Error::InvalidEndpoint(msg) if msg == "endpoint credentials are not supported"));
    }

    #[test]
    fn local_client_selects_direct_backend() {
        let client = SearchClient::local().expect("local backend");
        assert!(client.embedded);
        assert_eq!(
            client.config.endpoint().as_str(),
            format!("{DEFAULT_ENDPOINT}/")
        );
    }

    #[test]
    fn local_search_future_is_send() {
        fn assert_send<T: Send>(_: T) {}

        let client = SearchClient::local().expect("local backend");
        let query = SearchQuery::new("rust");
        assert_send(client.search(&query));
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

    #[test]
    fn response_accepts_answer_objects_and_null_urls() {
        let response: SearchResponse = serde_json::from_str(
            r#"{
                "answers": [{"answer":"Async Rust uses futures."}],
                "results": [{"title":"Rust","url":null,"content":"A guide."}]
            }"#,
        )
        .expect("compatible SearxNG response");

        assert_eq!(response.answers, ["Async Rust uses futures."]);
        assert_eq!(response.results[0].url, "");
    }

    #[tokio::test]
    async fn search_fetches_json_and_reports_http_errors() {
        let body = r#"{"query":"rust","number_of_results":1,"results":[{"title":"Rust","url":"https://example.com/rust","content":"A Rust guide."}]}"#;
        let (endpoint, server) = spawn_http_server(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        let response = SearchClient::new(endpoint)
            .expect("valid endpoint")
            .search(&SearchQuery::new("rust"))
            .await
            .expect("successful response");
        let request = server.join().expect("server thread");

        assert!(request.starts_with("GET /search?q=rust&format=json HTTP/1.1"));
        assert_eq!(response.query, "rust");
        assert_eq!(response.cited_sources()[0].title, "Rust");

        let (endpoint, server) = spawn_http_server(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 12\r\nConnection: close\r\n\r\nupstream bad".to_owned(),
        );
        let error = SearchClient::new(endpoint)
            .expect("valid endpoint")
            .search(&SearchQuery::new("rust"))
            .await
            .expect_err("HTTP errors must be surfaced");
        server.join().expect("server thread");

        assert!(
            matches!(error, Error::HttpStatus { status, body } if status == StatusCode::BAD_GATEWAY && body == "upstream bad")
        );
    }

    fn spawn_http_server(response: String) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept test request");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let length = stream.read(&mut chunk).expect("read test request");
                if length == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..length]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(response.as_bytes())
                .expect("write test response");
            String::from_utf8(request).expect("HTTP request is UTF-8")
        });
        (format!("http://{address}"), handle)
    }
}
