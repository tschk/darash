use criterion::{black_box, criterion_group, criterion_main, Criterion};
use darash::fetch;
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

criterion_group!(benches, bench_citation, bench_read_source);
criterion_main!(benches);
