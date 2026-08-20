//! Criterion benches for pc-decisions pure helpers.
//!
//! Run with: `cargo bench -p pc-decisions`
//! Generates HTML report under `target/criterion/`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pc_decisions::pure::{
    classify_effect_type, effect_target_ids, interpolate, same_ids, target_actions,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap};

fn bench_classify_effect_type(c: &mut Criterion) {
    let inputs = ["create_agent", "update_issue", "delete_company", "unknown_action"];
    c.bench_function("classify_effect_type", |b| {
        b.iter(|| {
            for s in &inputs {
                let _ = classify_effect_type(black_box(s));
            }
        })
    });
}

fn bench_effect_target_ids(c: &mut Criterion) {
    let value = json!({
        "agent_id": "a1",
        "issue_id": "i1",
        "company_id": "c1",
        "nested": {
            "agent_id": "a2"
        },
        "unrelated": "x"
    });
    c.bench_function("effect_target_ids", |b| {
        b.iter(|| effect_target_ids(black_box(&value)))
    });
}

fn bench_target_actions(c: &mut Criterion) {
    let value = json!({
        "options": {
            "agent": {"actions": ["create", "update"]},
            "issue": {"actions": ["close"]},
            "company": {"actions": ["read", "export"]}
        }
    });
    c.bench_function("target_actions", |b| {
        b.iter(|| target_actions(black_box(&value)))
    });
}

fn bench_same_ids(c: &mut Criterion) {
    let left: Vec<String> = (0..100).map(|i| format!("id-{i}")).collect();
    let mut right = left.clone();
    right.reverse();
    c.bench_function("same_ids_100", |b| {
        b.iter(|| same_ids(black_box(&left), black_box(&right)))
    });
}

fn bench_interpolate(c: &mut Criterion) {
    let template = "Hello {{name}}, your task {{task_id}} is {{status}} on {{date}}.";
    let mut values = HashMap::new();
    values.insert("name".into(), "Alice".into());
    values.insert("task_id".into(), "PAP-123".into());
    values.insert("status".into(), "in_progress".into());
    values.insert("date".into(), "2026-08-20".into());
    c.bench_function("interpolate_4vars", |b| {
        b.iter(|| interpolate(black_box(template), black_box(&values)))
    });
}

criterion_group!(
    benches,
    bench_classify_effect_type,
    bench_effect_target_ids,
    bench_target_actions,
    bench_same_ids,
    bench_interpolate
);
criterion_main!(benches);