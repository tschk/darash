use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_read_source(c: &mut Criterion) {
    let mut path = std::env::temp_dir();
    path.push(format!("darash-fetch-bench-{}.html", std::process::id()));
    let content = "<h1>Local</h1>".repeat(1024 * 10); // About 140KB
    std::fs::write(&path, content).expect("fixture writes");

    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("read_source");
    group.bench_function("async_read", |b| {
        b.to_async(&rt).iter(|| async {
            black_box(darash::fetch::read_source(&path).await.unwrap());
        })
    });
    group.finish();

    std::fs::remove_file(&path).ok();
}

criterion_group!(benches, bench_read_source);
criterion_main!(benches);
