use criterion::{black_box, criterion_group, criterion_main, Criterion};

// A simplified blocklist check to measure allocation performance
fn matches_blocklist(value: &str, blocklist: &[String]) -> bool {
    let value = value.to_ascii_lowercase();
    blocklist
        .iter()
        .filter(|term| !term.trim().is_empty())
        .any(|term| value.contains(&term.to_ascii_lowercase()))
}

fn matches_blocklist_optimized(value: &str, pre_lowercased_blocklist: &[String]) -> bool {
    let value = value.to_ascii_lowercase();
    pre_lowercased_blocklist
        .iter()
        .any(|term| value.contains(term))
}

fn bench_blocklist(c: &mut Criterion) {
    let blocklist = vec![
        "spam".to_string(),
        "badsite.com".to_string(),
        "malware".to_string(),
        "tracker".to_string(),
        "ads".to_string(),
    ];
    let pre_lowercased_blocklist: Vec<String> = blocklist
        .iter()
        .filter(|term| !term.trim().is_empty())
        .map(|term| term.to_ascii_lowercase())
        .collect();

    let value = "A very long string that we want to check against our blocklist but it does not contain any blocked terms".to_string();

    let mut group = c.benchmark_group("blocklist");
    group.bench_function("matches_blocklist", |b| {
        b.iter(|| {
            black_box(matches_blocklist(black_box(&value), black_box(&blocklist)));
        })
    });
    group.bench_function("matches_blocklist_optimized", |b| {
        b.iter(|| {
            black_box(matches_blocklist_optimized(
                black_box(&value),
                black_box(&pre_lowercased_blocklist),
            ));
        })
    });
    group.finish();
}

criterion_group!(benches, bench_blocklist);
criterion_main!(benches);
