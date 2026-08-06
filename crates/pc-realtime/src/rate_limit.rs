//! Round 255: Realtime rate limit + connection count limit。
//!
//! 背景：
//! - 现存 `/api/live-events`（WebSocket）与 `/api/realtime/stream`（SSE）都没有
//!   per-IP 限流 / per-company 连接数限制，恶意客户端可以并发开大量连接耗尽
//!   broadcast buffer + Tokio task 资源。
//! - Node paperclip 端有「per-IP token bucket」+「per-company max connections」机制。
//!
//! 设计：
//! - `TokenBucket`：经典令牌桶实现（`capacity` + `refill_per_second`）。
//!   - `try_acquire(n) -> bool`：非阻塞，检查 + 扣减。
//!   - `try_acquire_with_refill(n, now) -> bool`：在调用时按时间补 token。
//!   - `available_tokens() -> u64`：当前可用 token（用于 metrics / 调试）。
//! - `IpRateLimiter`：按 IP 维度分配 token bucket（`DashMap<IpAddr, Arc<TokenBucket>>`）。
//!   - `try_acquire(ip, n) -> bool`：自动懒初始化 bucket。
//! - `ConnectionLimiter`：按 company_id 维度跟踪当前活跃连接数（`Arc<DashMap<Uuid, AtomicI64>>`）。
//!   - `try_acquire(company_id) -> bool`：检查 + 增加计数，返回是否允许。
//!   - `release(company_id)`：连接关闭时减回。
//! - 所有结构都是 `Send + Sync + 'static`，可直接放进 `AppState`。

use std::net::IpAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
#[allow(unused_imports)]
use std::time::{Duration, Instant};

use dashmap::DashMap;
use uuid::Uuid;

/// 默认 token bucket 容量（每 IP 突发可接收 32 个连接）。
pub const DEFAULT_BUCKET_CAPACITY: u64 = 32;
/// 默认 refill rate（每 IP 每秒补 8 个 token）。
pub const DEFAULT_BUCKET_REFILL_PER_SECOND: u64 = 8;
/// 默认每 company 同时最大连接数。
pub const DEFAULT_MAX_CONNECTIONS_PER_COMPANY: i64 = 100;

/// 经典令牌桶（线程安全）。
///
/// 使用原子操作实现，但 `refill` 与 `try_acquire` 必须串行化（在调用方保证）。
/// 内部用 `AtomicU64` 存储「token 数量 + 时间戳」的 pack 值（简化实现）。
#[derive(Debug)]
pub struct TokenBucket {
    /// 桶容量上限
    capacity: u64,
    /// 每秒补 token 数
    refill_per_second: u64,
    /// 当前可用 token 数（f64 * 1000 存储，保留 3 位小数）
    tokens_milli: AtomicU64,
    /// 上次 refill 的时刻
    last_refill: parking_lot::Mutex<Instant>,
}

impl TokenBucket {
    /// 构造：`capacity` 个 token 满桶，每秒补 `refill_per_second` 个 token。
    pub fn new(capacity: u64, refill_per_second: u64) -> Self {
        Self {
            capacity,
            refill_per_second,
            tokens_milli: AtomicU64::new(capacity.saturating_mul(1000)),
            last_refill: parking_lot::Mutex::new(Instant::now()),
        }
    }

    /// 默认配置（capacity=32, refill_per_sec=8）。
    pub fn default_config() -> Self {
        Self::new(DEFAULT_BUCKET_CAPACITY, DEFAULT_BUCKET_REFILL_PER_SECOND)
    }

    /// 当前可用 token 数（用于 metrics / 调试）。
    pub fn available_tokens(&self) -> u64 {
        self.tokens_milli.load(Ordering::Relaxed) / 1000
    }

    /// 在 `now` 时刻按 elapsed 时间补 token，再尝试 acquire `n` 个 token。
    ///
    /// - 返回 `true`：成功 acquire，bucket 已扣减 `n` 个 token。
    /// - 返回 `false`：bucket 不足，状态不变。
    pub fn try_acquire_at(&self, now: Instant, n: u64) -> bool {
        // 1. 计算 elapsed，自上次 refill 以来补 token
        let mut last = self.last_refill.lock();
        let elapsed = now.saturating_duration_since(*last);
        // add = elapsed_millis * refill_per_sec * 1000 / 1000 = elapsed_millis * refill_per_sec (in milli-token)
        let add = (elapsed.as_millis() as u64).saturating_mul(self.refill_per_second);
        let mut current = self.tokens_milli.load(Ordering::Relaxed);
        loop {
            let new = current
                .saturating_add(add)
                .min(self.capacity.saturating_mul(1000));
            if new == current && add > 0 {
                // 推进 last_refill 一次（避免重复累加）
                *last = now;
                current = new;
                break;
            }
            match self.tokens_milli.compare_exchange_weak(
                current,
                new,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    *last = now;
                    current = new;
                    break;
                }
                Err(actual) => current = actual,
            }
        }
        // 2. 尝试扣减 n * 1000
        let need = n.saturating_mul(1000);
        if current < need {
            return false;
        }
        let new = current - need;
        match self.tokens_milli.compare_exchange_weak(
            current,
            new,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(_) => {
                // 并发 acquire 失败，重试一次
                let cur2 = self.tokens_milli.load(Ordering::Relaxed);
                if cur2 < need {
                    false
                } else {
                    self.tokens_milli
                        .compare_exchange_weak(
                            cur2,
                            cur2 - need,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                }
            }
        }
    }

    /// 便捷调用：使用 `Instant::now()` 调用 `try_acquire_at`。
    pub fn try_acquire(&self, n: u64) -> bool {
        self.try_acquire_at(Instant::now(), n)
    }
}

/// Per-IP token bucket 注册表。
///
/// 用 `DashMap<IpAddr, Arc<TokenBucket>>` 实现懒初始化：首次见到 IP 时分配默认 bucket。
pub struct IpRateLimiter {
    buckets: DashMap<IpAddr, Arc<TokenBucket>>,
    default_capacity: u64,
    default_refill_per_second: u64,
}

impl std::fmt::Debug for IpRateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpRateLimiter")
            .field("bucket_count", &self.buckets.len())
            .finish()
    }
}

impl Default for IpRateLimiter {
    fn default() -> Self {
        Self::new(DEFAULT_BUCKET_CAPACITY, DEFAULT_BUCKET_REFILL_PER_SECOND)
    }
}

impl IpRateLimiter {
    pub fn new(default_capacity: u64, default_refill_per_second: u64) -> Self {
        Self {
            buckets: DashMap::new(),
            default_capacity,
            default_refill_per_second,
        }
    }

    /// 尝试为 IP acquire `n` 个 token。
    pub fn try_acquire(&self, ip: IpAddr, n: u64) -> bool {
        let bucket = self
            .buckets
            .entry(ip)
            .or_insert_with(|| {
                Arc::new(TokenBucket::new(
                    self.default_capacity,
                    self.default_refill_per_second,
                ))
            })
            .clone();
        bucket.try_acquire(n)
    }

    /// 当前追踪的 IP 数（用于 metrics）。
    pub fn tracked_ip_count(&self) -> usize {
        self.buckets.len()
    }

    /// 测试辅助：移除某个 IP 的 bucket（让下次重新分配）。
    pub fn forget_ip(&self, ip: IpAddr) -> bool {
        self.buckets.remove(&ip).is_some()
    }
}

/// Per-company 并发连接数限制器。
///
/// 用 `DashMap<Uuid, Arc<AtomicI64>>` 跟踪每个 company_id 的活跃连接数。
/// 调用方必须成对调用 `try_acquire` + `release`（RAII 风格的 guard 见 `ConnectionGuard`）。
pub struct ConnectionLimiter {
    counts: DashMap<Uuid, Arc<AtomicI64>>,
    max_per_company: i64,
}

impl std::fmt::Debug for ConnectionLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionLimiter")
            .field("max_per_company", &self.max_per_company)
            .field("company_count", &self.counts.len())
            .finish()
    }
}

impl Default for ConnectionLimiter {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CONNECTIONS_PER_COMPANY)
    }
}

impl ConnectionLimiter {
    pub fn new(max_per_company: i64) -> Self {
        Self {
            counts: DashMap::new(),
            max_per_company,
        }
    }

    /// 尝试为 company acquire 一个连接槽。
    /// - 返回 `Some(guard)`：成功，guard drop 时自动 release。
    /// - 返回 `None`：超过 max_per_company 上限。
    pub fn try_acquire(&self, company_id: Uuid) -> Option<ConnectionGuard> {
        let counter = self
            .counts
            .entry(company_id)
            .or_insert_with(|| Arc::new(AtomicI64::new(0)))
            .clone();
        // CAS 循环：检查 + 增加
        loop {
            let current = counter.load(Ordering::Relaxed);
            if current >= self.max_per_company {
                return None;
            }
            if counter
                .compare_exchange_weak(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return Some(ConnectionGuard { counter });
            }
        }
    }

    /// 当前追踪的 company 数（用于 metrics）。
    pub fn tracked_company_count(&self) -> usize {
        self.counts.len()
    }

    /// 测试辅助：查询某 company 当前活跃连接数。
    pub fn current_count(&self, company_id: Uuid) -> i64 {
        self.counts
            .get(&company_id)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// 测试辅助：清空所有计数（不释放 guard）。
    pub fn reset_all(&self) {
        self.counts.clear();
    }
}

/// RAII guard：drop 时自动 `release` 一次连接槽。
///
/// `'_static` 设计：`ConnectionGuard` 持有 `Arc<AtomicI64>` 与无状态的 drop 逻辑，
/// 可以安全 move 进 `'static` closure（如 `ws.on_upgrade(...)`）。
pub struct ConnectionGuard {
    counter: Arc<AtomicI64>,
}

impl ConnectionGuard {
    /// 当前槽计数。
    pub fn current(&self) -> i64 {
        self.counter.load(Ordering::Relaxed)
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn token_bucket_starts_full() {
        let b = TokenBucket::new(10, 1);
        assert_eq!(b.available_tokens(), 10);
    }

    #[test]
    fn token_bucket_acquire_decreases_available() {
        let b = TokenBucket::new(10, 1);
        assert!(b.try_acquire(3));
        assert_eq!(b.available_tokens(), 7);
    }

    #[test]
    fn token_bucket_refuses_when_empty() {
        let b = TokenBucket::new(5, 0); // 不补 token
        assert!(b.try_acquire(5));
        assert!(!b.try_acquire(1));
    }

    #[test]
    fn token_bucket_refills_over_time() {
        let b = TokenBucket::new(10, 10); // 每秒补 10 个
        let start = Instant::now();
        assert!(b.try_acquire_at(start, 10));
        // 立即再 acquire 应该失败
        assert!(!b.try_acquire_at(start, 1));
        // 等 1 秒后再 acquire 应该成功
        let later = start + Duration::from_secs(1);
        assert!(b.try_acquire_at(later, 10));
    }

    #[test]
    fn token_bucket_refill_caps_at_capacity() {
        let b = TokenBucket::new(5, 100);
        let start = Instant::now();
        // 用尽 5 个
        assert!(b.try_acquire_at(start, 5));
        // 等 10 秒（理论上补 1000 个 token，但 capacity=5）
        let later = start + Duration::from_secs(10);
        assert!(b.try_acquire_at(later, 5));
        assert_eq!(b.available_tokens(), 0);
    }

    #[test]
    fn ip_rate_limiter_allocates_bucket_on_first_use() {
        let limiter = IpRateLimiter::default();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert!(limiter.try_acquire(ip, 1));
        assert_eq!(limiter.tracked_ip_count(), 1);
    }

    #[test]
    fn ip_rate_limiter_separates_per_ip() {
        let limiter = IpRateLimiter::new(2, 0); // capacity=2, 不补
        let ip1 = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ip2 = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        assert!(limiter.try_acquire(ip1, 2));
        assert!(!limiter.try_acquire(ip1, 1));
        // ip2 仍然有独立 quota
        assert!(limiter.try_acquire(ip2, 2));
    }

    #[test]
    fn connection_limiter_admits_under_limit() {
        let limiter = ConnectionLimiter::new(3);
        let company = Uuid::new_v4();
        let _g1 = limiter.try_acquire(company).expect("g1");
        let _g2 = limiter.try_acquire(company).expect("g2");
        let _g3 = limiter.try_acquire(company).expect("g3");
        assert_eq!(limiter.current_count(company), 3);
    }

    #[test]
    fn connection_limiter_refuses_over_limit() {
        let limiter = ConnectionLimiter::new(2);
        let company = Uuid::new_v4();
        let g1 = limiter.try_acquire(company).expect("g1");
        let g2 = limiter.try_acquire(company).expect("g2");
        assert!(limiter.try_acquire(company).is_none());
        assert_eq!(limiter.current_count(company), 2);
        drop(g1);
        assert_eq!(limiter.current_count(company), 1);
        assert!(limiter.try_acquire(company).is_some());
        drop(g2);
        assert_eq!(limiter.current_count(company), 0);
    }

    #[test]
    fn connection_limiter_separates_per_company() {
        let limiter = ConnectionLimiter::new(1);
        let c1 = Uuid::new_v4();
        let c2 = Uuid::new_v4();
        let _g1 = limiter.try_acquire(c1).expect("c1 g1");
        assert!(limiter.try_acquire(c1).is_none());
        let _g2 = limiter.try_acquire(c2).expect("c2 g1");
        assert!(limiter.try_acquire(c2).is_none());
        assert_eq!(limiter.current_count(c1), 1);
        assert_eq!(limiter.current_count(c2), 1);
    }

    #[test]
    fn connection_guard_decrements_on_drop() {
        let limiter = ConnectionLimiter::new(2);
        let company = Uuid::new_v4();
        {
            let _g = limiter.try_acquire(company).unwrap();
            assert_eq!(limiter.current_count(company), 1);
        }
        assert_eq!(limiter.current_count(company), 0);
    }
}
