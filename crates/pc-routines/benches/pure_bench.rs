//! Criterion benches for pc-routines pure helpers.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pc_routines::pure::{
    assert_routine_can_enable, is_valid_routine_date_string, is_valid_routine_variable_name,
    normalize_draft_routine_status, normalize_webhook_timestamp_ms,
    parse_boolean_variable_value, parse_date_variable_value, parse_number_variable_value,
};
use serde_json::json;

fn bench_is_valid_routine_variable_name(c: &mut Criterion) {
    let inputs = ["valid_name", "_under_score", "1invalid", "with-dash", "CamelCase"];
    c.bench_function("is_valid_routine_variable_name", |b| {
        b.iter(|| {
            for s in &inputs {
                let _ = is_valid_routine_variable_name(black_box(s));
            }
        })
    });
}

fn bench_is_valid_routine_date_string(c: &mut Criterion) {
    let inputs = ["2026-08-20", "2026-13-45", "not-a-date", "2026-02-29"];
    c.bench_function("is_valid_routine_date_string", |b| {
        b.iter(|| {
            for s in &inputs {
                let _ = is_valid_routine_date_string(black_box(s));
            }
        })
    });
}

fn bench_normalize_webhook_timestamp_ms(c: &mut Criterion) {
    let inputs = ["1755000000000", "1755000000.123", "invalid", ""];
    c.bench_function("normalize_webhook_timestamp_ms", |b| {
        b.iter(|| {
            for s in &inputs {
                let _ = normalize_webhook_timestamp_ms(black_box(s));
            }
        })
    });
}

fn bench_parse_boolean_variable_value(c: &mut Criterion) {
    let value = json!("true");
    c.bench_function("parse_boolean_variable_value", |b| {
        b.iter(|| parse_boolean_variable_value(black_box("flag"), black_box(&value)))
    });
}

fn bench_parse_number_variable_value(c: &mut Criterion) {
    let value = json!(42.5);
    c.bench_function("parse_number_variable_value", |b| {
        b.iter(|| parse_number_variable_value(black_box("count"), black_box(&value)))
    });
}

fn bench_parse_date_variable_value(c: &mut Criterion) {
    let value = json!("2026-08-20");
    c.bench_function("parse_date_variable_value", |b| {
        b.iter(|| parse_date_variable_value(black_box("date"), black_box(&value)))
    });
}

fn bench_normalize_draft_routine_status(c: &mut Criterion) {
    c.bench_function("normalize_draft_routine_status", |b| {
        b.iter(|| normalize_draft_routine_status(black_box("draft"), black_box(Some("agent-1"))))
    });
}

fn bench_assert_routine_can_enable(c: &mut Criterion) {
    c.bench_function("assert_routine_can_enable", |b| {
        b.iter(|| {
            let _ = assert_routine_can_enable(
                black_box("draft"),
                black_box(Some("agent-1")),
            );
        })
    });
}

criterion_group!(
    benches,
    bench_is_valid_routine_variable_name,
    bench_is_valid_routine_date_string,
    bench_normalize_webhook_timestamp_ms,
    bench_parse_boolean_variable_value,
    bench_parse_number_variable_value,
    bench_parse_date_variable_value,
    bench_normalize_draft_routine_status,
    bench_assert_routine_can_enable,
);
criterion_main!(benches);