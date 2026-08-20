use criterion::{black_box, criterion_group, criterion_main, Criterion};

// I need to find a way to test it. Wait, the method is private.
// I can just copy the relevant code into the bench file to see how it performs.
use std::collections::HashSet;

#[derive(Clone)]
struct SearchResult {
    pub title: String,
    pub url: String,
    pub content: String,
    pub engine: Option<String>,
    pub engines: Vec<String>,
    pub category: Option<String>,
    pub published_date: Option<String>,
    pub score: Option<f64>,
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

fn rank_results_original(query: &str, results: &mut [SearchResult]) {
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
    for (index, result) in results.iter_mut().enumerate() {
        let title = tokenize(&result.title);
        let content = tokenize(&result.content);
        let url = tokenize(&result.url);
        let mut score = 0.0;
        for term in &query_terms {
            let document_frequency = documents
                .iter()
                .filter(|document| document.contains(term))
                .count() as f64;
            let inverse_document_frequency =
                ((document_count + 1.0) / (document_frequency + 1.0)).ln() + 1.0;
            let title_frequency = title.iter().filter(|token| *token == term).count() as f64;
            let content_frequency = content.iter().filter(|token| *token == term).count() as f64;
            let url_frequency = url.iter().filter(|token| *token == term).count() as f64;
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

fn rank_results_optimized(query: &str, results: &mut [SearchResult]) {
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

    // Optimization: precalculate document frequencies for query terms
    let mut term_to_inverse_document_frequency = std::collections::HashMap::new();
    for term in &query_terms {
        let document_frequency = documents
            .iter()
            .filter(|document| document.contains(term))
            .count() as f64;
        let inverse_document_frequency =
            ((document_count + 1.0) / (document_frequency + 1.0)).ln() + 1.0;
        term_to_inverse_document_frequency.insert(term.clone(), inverse_document_frequency);
    }

    for (index, result) in results.iter_mut().enumerate() {
        let title = tokenize(&result.title);
        let content = tokenize(&result.content);
        let url = tokenize(&result.url);
        let mut score = 0.0;
        for term in &query_terms {
            let inverse_document_frequency = term_to_inverse_document_frequency.get(term).unwrap();
            let title_frequency = title.iter().filter(|token| *token == term).count() as f64;
            let content_frequency = content.iter().filter(|token| *token == term).count() as f64;
            let url_frequency = url.iter().filter(|token| *token == term).count() as f64;
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

fn generate_results(n: usize) -> Vec<SearchResult> {
    (0..n).map(|i| SearchResult {
        title: format!("Title word{} another word{}", i % 10, i % 5),
        url: format!("https://example.com/page{}", i),
        content: format!("This is the content for page {}. It has some repeated words and terms to make it realistic. word{} word{} word{}", i, i % 10, i % 5, i % 3),
        engine: None,
        engines: vec![],
        category: None,
        published_date: None,
        score: None,
    }).collect()
}

fn bench_ranking(c: &mut Criterion) {
    let mut results = generate_results(100);
    let query = "word1 word2 word3 word4 word5 word6 word7 word8 word9";

    let mut group = c.benchmark_group("ranking");
    group.bench_function("original", |b| {
        b.iter(|| {
            let mut r = results.clone();
            rank_results_original(black_box(query), black_box(&mut r));
        })
    });
    group.bench_function("optimized", |b| {
        b.iter(|| {
            let mut r = results.clone();
            rank_results_optimized(black_box(query), black_box(&mut r));
        })
    });
    group.finish();
}

criterion_group!(benches, bench_ranking);
criterion_main!(benches);
