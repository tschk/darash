use std::time::Duration;

use darash::{
    Error, SafeSearch, SearchClient, SearchMode, SearchQuery, SearchRequest, SearchResponse,
    SearchSource, TimeRange, WebsurfxQuery,
};
use serde_json::json;

#[test]
fn search_request_defaults_and_serializes_as_the_public_contract() {
    let request = SearchRequest::new("rust async");

    assert_eq!(request.query(), "rust async");
    assert_eq!(request.mode(), SearchMode::Balanced);
    assert_eq!(request.sources(), [SearchSource::Web]);
    assert_eq!(
        serde_json::to_value(&request).expect("request serializes"),
        json!({
            "query": "rust async",
            "mode": "balanced",
            "sources": ["web"]
        })
    );

    let restored: SearchRequest = serde_json::from_value(json!({"query":"rust async"}))
        .expect("query-only request uses defaults");
    assert_eq!(restored.mode(), SearchMode::Balanced);
    assert_eq!(restored.sources(), [SearchSource::Web]);

    let selected = SearchRequest::new("rust")
        .with_mode(SearchMode::Quality)
        .with_sources([
            SearchSource::Academic,
            SearchSource::Academic,
            SearchSource::Discussions,
        ]);
    assert_eq!(selected.mode(), SearchMode::Quality);
    assert_eq!(
        selected.sources(),
        [SearchSource::Academic, SearchSource::Discussions]
    );
}

#[test]
fn search_query_serializes_public_searxng_parameters() {
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
fn safe_search_values_and_modes_are_stable() {
    for (safe_search, value) in [
        (SafeSearch::Off, "0"),
        (SafeSearch::Moderate, "1"),
        (SafeSearch::Strict, "2"),
    ] {
        let query = SearchQuery::new("rust").with_safe_search(safe_search);
        assert!(query
            .to_query_string()
            .contains(&format!("safesearch={value}")));
    }

    assert_eq!(SearchMode::Speed.as_str(), "speed");
    assert_eq!(SearchMode::Balanced.as_str(), "balanced");
    assert_eq!(SearchMode::Quality.as_str(), "quality");
    assert_eq!(SearchSource::Web.as_str(), "web");
    assert_eq!(SearchSource::Academic.as_str(), "academic");
    assert_eq!(SearchSource::Discussions.as_str(), "discussions");
}

#[tokio::test]
async fn query_validation_completes_before_network_access() {
    let client = SearchClient::new("http://127.0.0.1:1").expect("endpoint is valid");

    assert!(matches!(
        client.search(&SearchQuery::new("  ")).await,
        Err(Error::EmptyQuery)
    ));
    assert!(matches!(
        client.search(&SearchQuery::new("rust").with_page(0)).await,
        Err(Error::InvalidPage)
    ));
    assert!(matches!(
        client
            .search(&SearchQuery::new("rust").with_page(u32::MAX))
            .await,
        Err(Error::PageOverflow)
    ));
    assert!(matches!(
        client.search_websurfx(&WebsurfxQuery::new("  ")).await,
        Err(Error::EmptyQuery)
    ));

    let timeout = client
        .config()
        .clone()
        .with_timeout(Duration::from_millis(1))
        .timeout();
    assert_eq!(timeout, Duration::from_millis(1));
}

#[test]
fn search_response_preserves_result_and_metadata_fields() {
    let response: SearchResponse = serde_json::from_value(json!({
        "query": "rust async",
        "number_of_results": 1,
        "answers": [{"answer": "Async Rust uses futures."}],
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
        "corrections": ["rust futures"],
        "suggestions": ["rust async book"],
        "sources": [{
            "title": "Async Rust",
            "url": "https://example.com/rust",
            "snippet": "A guide to async Rust.",
            "source": "brave",
            "published_date": "2026-01-02"
        }]
    }))
    .expect("response metadata deserializes");

    assert_eq!(response.query, "rust async");
    assert_eq!(response.number_of_results, 1);
    assert_eq!(response.answers, ["Async Rust uses futures."]);
    assert_eq!(response.answer, None);
    assert_eq!(response.corrections, ["rust futures"]);
    assert_eq!(response.suggestions, ["rust async book"]);
    assert_eq!(response.results[0].category.as_deref(), Some("general"));
    assert_eq!(
        response.results[0].published_date.as_deref(),
        Some("2026-01-02")
    );
    assert_eq!(response.results[0].score, Some(1.25));
    assert_eq!(response.cited_sources()[0].source.as_deref(), Some("brave"));
    assert_eq!(
        response.cited_sources()[0].published_date.as_deref(),
        Some("2026-01-02")
    );

    let encoded = serde_json::to_value(&response).expect("response serializes");
    assert_eq!(encoded["results"][0]["publishedDate"], "2026-01-02");
    assert_eq!(encoded["sources"][0]["published_date"], "2026-01-02");
}
