use criterion::{black_box, criterion_group, criterion_main, Criterion};

use std::path::Path;

async fn orig_launch(executable: &Path) {
    let executable = std::fs::canonicalize(executable).unwrap();
    black_box(executable);
}

async fn optimized_launch(executable: &Path) {
    let executable = tokio::task::spawn_blocking({
        let path = executable.to_owned();
        move || std::fs::canonicalize(path)
    })
    .await
    .unwrap()
    .unwrap();
    black_box(executable);
}

fn bench_browser(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("browser");

    group.bench_function("orig_launch", |b| {
        b.to_async(&rt).iter(|| async {
            orig_launch(Path::new("/bin/sh")).await;
        })
    });
    group.bench_function("optimized_launch", |b| {
        b.to_async(&rt).iter(|| async {
            optimized_launch(Path::new("/bin/sh")).await;
        })
    });
    group.finish();
}

criterion_group!(benches, bench_browser);
criterion_main!(benches);
