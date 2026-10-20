use criterion::{black_box, criterion_group, criterion_main, Criterion};
use darash::fetch;

fn bench_rows(c: &mut Criterion) {
    let mut html = String::from("<html><body>");
    for i in 0..100 {
        html.push_str(&format!(
            "<div class=\"item\"><h2 class=\"title\">Title {}</h2><a href=\"/link{}\">Link</a><div class=\"desc\">Desc {}</div></div>",
            i, i, i
        ));
    }
    html.push_str("</body></html>");

    let mut group = c.benchmark_group("fetch");
    group.bench_function("rows", |b| {
        b.iter(|| {
            black_box(
                fetch::rows(
                    black_box(&html),
                    ".item",
                    "title=h2, url=a@href, desc=.desc",
                )
                .unwrap(),
            );
        })
    });
    group.finish();
}

criterion_group!(benches, bench_rows);
criterion_main!(benches);
