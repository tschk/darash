use criterion::{black_box, criterion_group, criterion_main, Criterion};
use serde_json::{json, Value};

fn bench_record_generation_original(c: &mut Criterion) {
    let headers: Vec<String> = (0..20).map(|i| format!("header_{}", i)).collect();
    let rows: Vec<Vec<String>> = (0..1000).map(|_| {
        (0..20).map(|i| format!("value_{}", i)).collect()
    }).collect();

    c.bench_function("record_gen_original", |b| {
        b.iter(|| {
            let _records: Vec<_> = black_box(&rows)
                .iter()
                .map(|row| {
                    let mut object = serde_json::Map::new();
                    for (index, header) in black_box(&headers).iter().enumerate() {
                        object.insert(
                            header.clone(),
                            json!(row.get(index).cloned().unwrap_or_default()),
                        );
                    }
                    object
                })
                .collect();
        });
    });
}

fn bench_record_generation_template_get_mut(c: &mut Criterion) {
    let headers: Vec<String> = (0..20).map(|i| format!("header_{}", i)).collect();
    let rows: Vec<Vec<String>> = (0..1000).map(|_| {
        (0..20).map(|i| format!("value_{}", i)).collect()
    }).collect();

    c.bench_function("record_gen_template_get_mut", |b| {
        b.iter(|| {
            let mut template = serde_json::Map::new();
            for header in black_box(&headers) {
                template.insert(header.clone(), Value::Null);
            }

            let _records: Vec<_> = black_box(&rows)
                .iter()
                .map(|row| {
                    let mut object = template.clone();
                    for (index, header) in black_box(&headers).iter().enumerate() {
                        if let Some(val) = object.get_mut(header) {
                            *val = Value::String(row.get(index).cloned().unwrap_or_default());
                        }
                    }
                    object
                })
                .collect();
        });
    });
}

fn bench_record_generation_template_insert(c: &mut Criterion) {
    let headers: Vec<String> = (0..20).map(|i| format!("header_{}", i)).collect();
    let rows: Vec<Vec<String>> = (0..1000).map(|_| {
        (0..20).map(|i| format!("value_{}", i)).collect()
    }).collect();

    c.bench_function("record_gen_template_insert", |b| {
        b.iter(|| {
            let mut template = serde_json::Map::new();
            for header in black_box(&headers) {
                template.insert(header.clone(), Value::Null);
            }

            let _records: Vec<_> = black_box(&rows)
                .iter()
                .map(|row| {
                    let mut object = template.clone();
                    for (index, header) in black_box(&headers).iter().enumerate() {
                        object.insert(
                            header.clone(),
                            Value::String(row.get(index).cloned().unwrap_or_default())
                        );
                    }
                    object
                })
                .collect();
        });
    });
}


criterion_group!(benches, bench_record_generation_original, bench_record_generation_template_get_mut, bench_record_generation_template_insert);
criterion_main!(benches);
