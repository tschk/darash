use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn matches_allowlist_result_slow(
    title: &str,
    url: &str,
    content: &str,
    allowlist: &[String],
) -> bool {
    let value = format!("{} {} {}", title, url, content);
    let value = value.to_ascii_lowercase();
    allowlist
        .iter()
        .filter(|term| !term.trim().is_empty())
        .any(|term| value.contains(&term.to_ascii_lowercase()))
}

fn matches_allowlist_result_fast(
    title: &str,
    url: &str,
    content: &str,
    allowlist: &[String],
) -> bool {
    let value = format!("{} {} {}", title, url, content);
    let value = value.to_ascii_lowercase();
    allowlist.iter().any(|term| value.contains(term))
}

fn bench_allowlist(c: &mut Criterion) {
    let title = "The quick brown fox";
    let url = "https://example.com/fox";
    let content = "The quick brown fox jumps over the lazy dog";

    // Create an allowlist with many items to exaggerate the issue
    let mut allowlist = vec![];
    for i in 0..100 {
        allowlist.push(format!("term{}", i));
    }
    allowlist.push("dog".to_string()); // match at the end

    // Slow version will do to_ascii_lowercase for every term on every check

    // Fast version requires pre-processing the allowlist (which we can simulate)
    let preprocessed_allowlist: Vec<String> = allowlist
        .iter()
        .filter(|term| !term.trim().is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect();

    let mut group = c.benchmark_group("allowlist");
    group.bench_function("slow", |b| {
        b.iter(|| {
            black_box(matches_allowlist_result_slow(
                title, url, content, &allowlist,
            ));
        })
    });
    group.bench_function("fast (preprocessed)", |b| {
        b.iter(|| {
            black_box(matches_allowlist_result_fast(
                title,
                url,
                content,
                &preprocessed_allowlist,
            ));
        })
    });
    group.finish();
}

criterion_group!(benches, bench_allowlist);
criterion_main!(benches);
