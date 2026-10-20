use criterion::{black_box, criterion_group, criterion_main, Criterion};
use darash::fetch;

fn bench_fetch_rows(c: &mut Criterion) {
    let mut html = String::from("<html><body>");
    for i in 0..1000 {
        html.push_str(&format!(
            r#"<div class="item"><h2>Item {}</h2><a href="url{}">Link</a></div>"#,
            i, i
        ));
    }
    html.push_str("</body></html>");

    c.bench_function("fetch::rows", |b| {
        b.iter(|| {
            black_box(
                fetch::rows(
                    black_box(&html),
                    black_box(".item"),
                    black_box("title=h2, url=a@href"),
                )
                .unwrap(),
            );
        });
    });
}

criterion_group!(benches, bench_fetch_rows);
criterion_main!(benches);
