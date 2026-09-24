use criterion::{black_box, criterion_group, criterion_main, Criterion};
use darash::SearchResult;

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

fn bench_disk_cache(c: &mut Criterion) {
    let report = darash::fetch::FetchReport {
        status: Some(200),
        ok: true,
        url: "https://example.com/some/long/url/that/takes/time/to/clone".to_string(),
        redirected: false,
        ms: 100,
        content_type: Some("text/html".to_string()),
        bytes: 1000,
        body: "Some long content that takes a bit of time to clone, like this sentence, but much longer. It goes on and on and on and on and on and on and on and on and on and on and on and on.".repeat(100).to_string(),
    };

    let mut group = c.benchmark_group("disk_cache");
    let rt = tokio::runtime::Runtime::new().unwrap();

    group.bench_function("store", |b| {
        b.to_async(&rt).iter(|| async {
            let _ = darash::disk_cache::store(
                "https://example.com/some/long/url/that/takes/time/to/clone",
                &report,
            ).await;
        })
    });
    group.finish();
}

criterion_group!(benches, bench_citation, bench_disk_cache);
criterion_main!(benches);
