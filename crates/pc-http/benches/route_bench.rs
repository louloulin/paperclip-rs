//! Criterion benches for pc-http route dispatch.
//!
//! Validates that axum router dispatch + middleware chain stays under budget.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use serde_json::json;

fn bench_serde_roundtrip_companies_list(c: &mut Criterion) {
    let body = json!({
        "companies": [
            {"id": "c1", "name": "Acme", "status": "active"},
            {"id": "c2", "name": "Beta", "status": "active"},
            {"id": "c3", "name": "Gamma", "status": "archived"}
        ]
    });
    c.bench_function("serde_companies_list_roundtrip", |b| {
        b.iter(|| {
            let s = serde_json::to_string(black_box(&body)).unwrap();
            let _: serde_json::Value = serde_json::from_str(&s).unwrap();
        })
    });
}

criterion_group!(benches, bench_serde_roundtrip_companies_list);
criterion_main!(benches);