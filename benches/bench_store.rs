use criterion::{black_box, criterion_group, criterion_main, Criterion};
use darash::fetch::FetchReport;
use tokio::runtime::Runtime;

fn bench_store(c: &mut Criterion) {
    let report = FetchReport {
        url: "https://example.com/some/test/url".to_string(),
        status: Some(200),
        ok: true,
        redirected: false,
        ms: 100,
        content_type: Some("text/html".to_string()),
        bytes: 1024 * 1024,
        body: String::from_utf8(vec![b'a'; 1024 * 1024]).unwrap(), // 1MB payload
    };

    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("disk_cache");
    group.bench_function("store()", |b| {
        b.to_async(&rt).iter(|| async {
            black_box(
                darash::disk_cache::store("https://example.com/some/test/url", &report).await,
            );
        })
    });
    group.finish();
}

criterion_group!(benches, bench_store);
criterion_main!(benches);
