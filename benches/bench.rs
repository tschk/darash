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

fn bench_blocklist_sim(c: &mut Criterion) {
    let blocklist = [
        "bad".to_string(),
        "terrible".to_string(),
        "awful".to_string(),
        "spam".to_string(),
    ];
    let result = "This is a good result without any bad words, oops there is one";

    c.bench_function("matches_blocklist_baseline", |b| {
        b.iter(|| {
            let value = result.to_ascii_lowercase();
            black_box(
                blocklist
                    .iter()
                    .filter(|term| !term.trim().is_empty())
                    .any(|term| value.contains(&term.to_ascii_lowercase())),
            )
        })
    });

    let pre_lowercased_blocklist: Vec<String> = blocklist
        .iter()
        .filter(|term| !term.trim().is_empty())
        .map(|term| term.to_ascii_lowercase())
        .collect();

    c.bench_function("matches_blocklist_optimized", |b| {
        b.iter(|| {
            let value = result.to_ascii_lowercase();
            black_box(
                pre_lowercased_blocklist
                    .iter()
                    .any(|term| value.contains(term)),
            )
        })
    });
}

criterion_group!(benches, bench_citation, bench_blocklist_sim);
criterion_main!(benches);
