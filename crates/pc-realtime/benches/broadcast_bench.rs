//! Criterion benches for pc-realtime hot path.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pc_realtime::LiveEvent;
use uuid::Uuid;

fn bench_live_event_new(c: &mut Criterion) {
    c.bench_function("live_event_new", |b| {
        b.iter(|| {
            let _ = LiveEvent::new(
                black_box("issue.status_changed"),
                black_box("issue"),
                black_box(Uuid::nil()),
            );
        })
    });
}

fn bench_live_event_clone(c: &mut Criterion) {
    let event = LiveEvent::new("issue.status_changed", "issue", Uuid::nil());
    c.bench_function("live_event_clone", |b| {
        b.iter(|| black_box(event.clone()))
    });
}

criterion_group!(benches, bench_live_event_new, bench_live_event_clone);
criterion_main!(benches);