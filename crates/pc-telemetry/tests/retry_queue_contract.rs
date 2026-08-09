use std::time::{Duration, Instant};

use pc_telemetry::{RetryBackoff, RetryQueue};

#[test]
fn bounded_queue_evicts_oldest_batch() {
    let mut queue = RetryQueue::new(2);
    assert!(queue.push("oldest", 1, Instant::now()).is_none());
    assert!(queue.push("middle", 1, Instant::now()).is_none());
    assert_eq!(queue.push("newest", 1, Instant::now()), Some("oldest"));
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.drain_due(Instant::now()), vec!["middle", "newest"]);
}

#[test]
fn drain_due_keeps_future_batches() {
    let now = Instant::now();
    let mut queue = RetryQueue::new(4);
    queue.push("due", 1, now);
    queue.push("future", 1, now + Duration::from_secs(5));
    assert_eq!(queue.drain_due(now), vec!["due"]);
    assert_eq!(queue.len(), 1);
}

#[test]
fn backoff_is_exponential_jittered_and_capped() {
    let backoff = RetryBackoff {
        base: Duration::from_secs(1),
        max: Duration::from_secs(5),
        jitter_ratio: 0.25,
    };
    assert_eq!(backoff.delay(1, 0.5), Duration::from_secs(1));
    assert_eq!(backoff.delay(3, 0.0), Duration::from_secs(3));
    assert_eq!(backoff.delay(9, 1.0), Duration::from_secs(5));
}
