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

criterion_group!(benches, bench_citation);
criterion_main!(benches);
