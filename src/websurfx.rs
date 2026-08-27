use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use url::{form_urlencoded, Url};

use crate::{Citation, ProviderStatus, SearchFilters, SearchResponse, SearchResult};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebsurfxSearchResponse {
    #[serde(default)]
    pub results: Vec<WebsurfxSearchResult>,
    #[serde(default)]
    pub engine_errors_info: Vec<WebsurfxEngineError>,
    #[serde(default)]
    pub disallowed: bool,
    #[serde(default)]
    pub filtered: bool,
    #[serde(default)]
    pub safe_search_level: u8,
    #[serde(default)]
    pub no_engines_selected: bool,
}

pub type WebsurfxResponse = WebsurfxSearchResponse;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebsurfxSearchResult {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub engine: Vec<String>,
    #[serde(default)]
    pub relevance_score: f32,
}

pub type WebsurfxResult = WebsurfxSearchResult;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WebsurfxEngineError {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub severity_color: String,
}

pub type WebsurfxError = WebsurfxEngineError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebsurfxMetadata {
    pub engine_errors_info: Vec<WebsurfxEngineError>,
    pub disallowed: bool,
    pub filtered: bool,
    pub safe_search_level: u8,
    pub no_engines_selected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WebsurfxMappedResponse {
    pub response: SearchResponse,
    pub metadata: WebsurfxMetadata,
}

impl WebsurfxSearchResponse {
    pub fn into_search_response(self, query: impl Into<String>) -> WebsurfxMappedResponse {
        let results = self
            .results
            .into_iter()
            .map(WebsurfxSearchResult::into_search_result)
            .collect::<Vec<_>>();

        let sources = Self::extract_citations(&results);
        let provider_status = Self::compute_provider_status(&results, &self.engine_errors_info);

        let safe_search_level = self.safe_search_level;
        let metadata = WebsurfxMetadata {
            engine_errors_info: self.engine_errors_info,
            disallowed: self.disallowed,
            filtered: self.filtered,
            safe_search_level,
            no_engines_selected: self.no_engines_selected,
        };
        let response = SearchResponse {
            query: query.into(),
            number_of_results: results.len() as u64,
            results,
            answers: Vec::new(),
            answer: None,
            sources,
            corrections: Vec::new(),
            suggestions: Vec::new(),
            provider_status,
            filters: SearchFilters {
                safe_search_level,
                time_range: None,
                filtered: self.filtered,
                disallowed: self.disallowed,
                no_providers_selected: self.no_engines_selected,
            },
        };
        WebsurfxMappedResponse { response, metadata }
    }

    fn extract_citations(results: &[SearchResult]) -> Vec<Citation> {
        results
            .iter()
            .map(SearchResult::citation)
            .collect::<Vec<Citation>>()
    }

    fn compute_provider_status(
        results: &[SearchResult],
        engine_errors: &[WebsurfxEngineError],
    ) -> Vec<ProviderStatus> {
        let mut provider_counts = BTreeMap::new();
        for result in results {
            for engine in &result.engines {
                *provider_counts.entry(engine.as_str()).or_insert(0) += 1;
            }
        }
        provider_counts
            .into_iter()
            .map(|(provider, count)| ProviderStatus::success(provider, count))
            .chain(
                engine_errors
                    .iter()
                    .map(|error| ProviderStatus::failed(error.engine.clone(), error.error.clone())),
            )
            .collect()
    }
}

impl WebsurfxSearchResult {
    pub fn into_search_result(self) -> SearchResult {
        SearchResult {
            title: self.title,
            url: self.url,
            content: self.description,
            engine: self.engine.first().cloned(),
            engines: self.engine,
            category: None,
            published_date: None,
            score: Some(self.relevance_score as f64),
        }
    }
}

impl From<WebsurfxSearchResult> for SearchResult {
    fn from(result: WebsurfxSearchResult) -> Self {
        result.into_search_result()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WebsurfxQuery {
    query: String,
    page: Option<u32>,
    safe_search: Option<u8>,
}

impl WebsurfxQuery {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            page: None,
            safe_search: None,
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn page(&self) -> Option<u32> {
        self.page
    }

    pub fn safe_search(&self) -> Option<u8> {
        self.safe_search
    }

    pub fn with_page(mut self, page: u32) -> Self {
        self.page = Some(page);
        self
    }

    pub fn with_safe_search(mut self, level: u8) -> Self {
        self.safe_search = Some(level);
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut serializer = form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("q", &self.query);
        if let Some(page) = self.page {
            serializer.append_pair("page", &page.to_string());
        }
        if let Some(level) = self.safe_search {
            serializer.append_pair("safesearch", &level.to_string());
        }
        serializer.append_pair("json", "true");
        serializer.finish()
    }
}

pub fn build_search_url(
    endpoint: impl AsRef<str>,
    query: &WebsurfxQuery,
) -> Result<Url, url::ParseError> {
    let mut url = Url::parse(endpoint.as_ref())?;
    let base_path = url.path().trim_end_matches('/');
    let path = if base_path.is_empty() {
        "/search".to_owned()
    } else if base_path == "/search" || base_path.ends_with("/search") {
        base_path.to_owned()
    } else {
        format!("{base_path}/search")
    };
    url.set_path(&path);
    url.set_query(Some(&query.to_query_string()));
    url.set_fragment(None);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_maps_websurfx_results_and_metadata() {
        let body = r#"{
            "results": [{
                "title": "Rust async",
                "url": "https://example.com/rust#intro",
                "description": "A guide to async Rust.",
                "engine": ["DuckDuckGo", "Wikipedia"],
                "relevanceScore": 0.75
            }],
            "engineErrorsInfo": [{
                "error": "RequestError",
                "engine": "Bing",
                "severity_color": "green"
            }],
            "disallowed": false,
            "filtered": true,
            "safeSearchLevel": 3,
            "noEnginesSelected": false
        }"#;
        let parsed: WebsurfxSearchResponse = serde_json::from_str(body).expect("valid response");
        assert_eq!(parsed.results[0].engine, ["DuckDuckGo", "Wikipedia"]);
        assert_eq!(parsed.results[0].relevance_score, 0.75);

        let mapped = parsed.into_search_response("rust async");
        assert_eq!(mapped.response.query, "rust async");
        assert_eq!(mapped.response.results.len(), 1);
        assert_eq!(mapped.response.results[0].content, "A guide to async Rust.");
        assert_eq!(
            mapped.response.results[0].engines,
            ["DuckDuckGo", "Wikipedia"]
        );
        assert_eq!(
            mapped.response.results[0].engine.as_deref(),
            Some("DuckDuckGo")
        );
        assert_eq!(mapped.response.results[0].score, Some(0.75));
        assert_eq!(mapped.response.provider_status.len(), 3);
        assert_eq!(mapped.response.filters.safe_search_level, 3);
        assert_eq!(mapped.metadata.engine_errors_info[0].engine, "Bing");
        assert!(mapped.metadata.filtered);
        assert_eq!(mapped.metadata.safe_search_level, 3);
    }

    #[test]
    fn gets_websurfx_query_properties() {
        let query = WebsurfxQuery::new("test query");
        assert_eq!(query.query(), "test query");
        assert_eq!(query.page(), None);
        assert_eq!(query.safe_search(), None);

        let query_with_options = WebsurfxQuery::new("test query")
            .with_page(1)
            .with_safe_search(2);
        assert_eq!(query_with_options.query(), "test query");
        assert_eq!(query_with_options.page(), Some(1));
        assert_eq!(query_with_options.safe_search(), Some(2));
    }

    #[test]
    fn builds_websurfx_search_url() {
        let query = WebsurfxQuery::new("rust async")
            .with_page(2)
            .with_safe_search(3);
        assert_eq!(
            query.to_query_string(),
            "q=rust+async&page=2&safesearch=3&json=true"
        );
        let url = build_search_url("http://localhost:8080/", &query).expect("valid endpoint");
        assert_eq!(
            url.as_str(),
            "http://localhost:8080/search?q=rust+async&page=2&safesearch=3&json=true"
        );
    }
}
