use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_canonicalize(c: &mut Criterion) {
    let mut group = c.benchmark_group("canonicalize");
    group.bench_function("std::fs::canonicalize", |b| {
        b.iter(|| {
            black_box(std::fs::canonicalize("/tmp").unwrap());
        })
    });
    group.bench_function("tokio::fs::canonicalize", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                black_box(tokio::fs::canonicalize("/tmp").await.unwrap());
            })
    });
    group.finish();
}

criterion_group!(benches, bench_canonicalize);
criterion_main!(benches);
