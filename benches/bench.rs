use criterion::{black_box, criterion_group, criterion_main, Criterion};
use darash::fetch;
use darash::{SearchRequest, SearchResult, SearchSource};

fn bench_citation(c: &mut Criterion) {
    let result = SearchResult {
        title: "Some long title that takes a bit of time to clone".to_string(),
        url: "https://example.com/some/long/url/that/takes/time/to/clone".to_string(),
        content: "Some long content that takes a bit of time to clone, like this sentence, but much longer. It goes on and on and on and on and on and on and on and on and on and on and on and on.".to_string(),
        engine: Some("some engine".to_string()),
        engines: vec!["some engine".to_string()],
        published_date: Some("2021-01-01".to_string()),
        category: None,
        score: None,
    };

    let mut group = c.benchmark_group("citation");
    group.bench_function("citation()", |b| {
        b.iter(|| {
            black_box(result.citation());
        })
    });
    group.bench_function("into_citation()", |b| {
        b.iter_batched(
            || result.clone(),
            |r| black_box(r.into_citation()),
            criterion::BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_with_sources(c: &mut Criterion) {
    let mut group = c.benchmark_group("with_sources");
    group.bench_function("duplicate_sources", |b| {
        b.iter(|| {
            let req = SearchRequest::new("rust");
            let sources = vec![
                SearchSource::Web,
                SearchSource::Academic,
                SearchSource::Discussions,
                SearchSource::Web,
                SearchSource::Academic,
                SearchSource::Web,
            ];
            black_box(req.with_sources(sources));
        })
    });
    group.bench_function("many_sources", |b| {
        b.iter(|| {
            let req = SearchRequest::new("rust");
            let mut sources = Vec::new();
            for _ in 0..100 {
                sources.push(SearchSource::Web);
                sources.push(SearchSource::Academic);
                sources.push(SearchSource::Discussions);
            }
            black_box(req.with_sources(sources));
        })
    });
    group.finish();
}

fn bench_read_source(c: &mut Criterion) {
    let temp_dir = std::env::temp_dir();
    let mut temp_path = temp_dir.clone();
    temp_path.push(format!("darash-bench-test-{}.html", std::process::id()));

    // Create a fairly large file
    let content = "<h1>Local</h1>\n".repeat(10000);
    std::fs::write(&temp_path, content).expect("fixture writes");

    let path = temp_path.clone();

    let mut group = c.benchmark_group("read_source");
    group.bench_function("async", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                black_box(fetch::read_source(&path).await.unwrap());
            })
    });
    group.finish();

    std::fs::remove_file(&temp_path).ok();
}

criterion_group!(
    benches,
    bench_citation,
    bench_with_sources,
    bench_apply_budget,
    bench_read_source
);
criterion_main!(benches);

fn bench_apply_budget(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply_budget");
    let items = vec![
        "This is a short sentence.".to_string(),
        "This is another short sentence.".to_string(),
        "This is a slightly longer sentence, which might consume more tokens.".to_string(),
        "Tokens are important to track, so this sentence is also quite long, but not too long."
            .to_string(),
        "A final sentence to fill out the budget allocation with a bit more length to it."
            .to_string(),
    ];
    let budget = Some(15);

    group.bench_function("apply_budget", |b| {
        b.iter_batched(
            || items.clone(),
            |items| black_box(fetch::apply_budget(items, budget)),
            criterion::BatchSize::SmallInput,
        )
    });
    group.finish();
}
