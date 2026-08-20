//! Criterion benches for pc-heartbeat hot path.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pc_heartbeat::enforce_issue_execution_lock_for;

fn bench_enforce_issue_execution_lock(c: &mut Criterion) {
    c.bench_function("enforce_issue_execution_lock_for", |b| {
        b.iter(|| enforce_issue_execution_lock_for(black_box(Some("transient"))))
    });
}

criterion_group!(benches, bench_enforce_issue_execution_lock);
criterion_main!(benches);