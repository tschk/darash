use criterion::{black_box, criterion_group, criterion_main, Criterion};
use darash::fetch;

fn bench_rows(c: &mut Criterion) {
    let mut html = String::new();
    html.push_str("<html><body>");
    for i in 0..10000 {
        html.push_str(&format!(
            "<div class=\"item\"><h2>Title {}</h2><a href=\"/link{}\">Link</a><p>Desc {}</p></div>",
            i, i, i
        ));
    }
    html.push_str("</body></html>");

    let mut group = c.benchmark_group("fetch");
    group.bench_function("rows", |b| {
        b.iter(|| {
            black_box(fetch::rows(
                &html,
                ".item",
                "title=h2, url=a@href, desc=p",
            ).unwrap());
        })
    });
    group.finish();
}

criterion_group!(benches, bench_rows);
criterion_main!(benches);
