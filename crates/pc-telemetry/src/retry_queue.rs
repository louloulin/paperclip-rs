use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy)]
pub struct RetryBackoff {
    pub base: Duration,
    pub max: Duration,
    pub jitter_ratio: f64,
}

impl RetryBackoff {
    pub fn delay(self, failed_attempt: u32, random: f64) -> Duration {
        let exponent = failed_attempt.saturating_sub(1).min(31);
        let base = self.base.saturating_mul(2_u32.pow(exponent)).min(self.max);
        let jitter = self.jitter_ratio.clamp(0.0, 1.0) * (random.clamp(0.0, 1.0) * 2.0 - 1.0);
        Duration::from_secs_f64(
            (base.as_secs_f64() * (1.0 + jitter)).clamp(0.0, self.max.as_secs_f64()),
        )
    }
}

#[derive(Debug)]
struct Pending<T> {
    payload: T,
    due_at: Instant,
}

#[derive(Debug)]
pub struct RetryQueue<T> {
    capacity: usize,
    pending: VecDeque<Pending<T>>,
}

impl<T> RetryQueue<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            pending: VecDeque::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn push(&mut self, payload: T, _attempt: u32, due_at: Instant) -> Option<T> {
        self.pending.push_back(Pending { payload, due_at });
        if self.pending.len() > self.capacity {
            self.pending.pop_front().map(|item| item.payload)
        } else {
            None
        }
    }

    pub fn drain_due(&mut self, now: Instant) -> Vec<T> {
        let mut due = Vec::new();
        let mut waiting = VecDeque::new();
        while let Some(item) = self.pending.pop_front() {
            if item.due_at <= now {
                due.push(item.payload);
            } else {
                waiting.push_back(item);
            }
        }
        self.pending = waiting;
        due
    }
}
