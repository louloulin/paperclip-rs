//! Secret value TTL 缓存。
//!
//! 设计目标：
//! - 避免每次 secret 解析都打一次 provider RPC。
//! - 缓存命中时 `O(1)` 返回；未命中调用 provider。
//! - TTL 到期或显式 invalidate 才重新拉取。
//! - 支持负缓存（记录"未找到"，避免短时间内反复打 provider）。
//!
//! 与 Node `secretsCache.ts` 思路一致，但用 Rust 简化：
//! - 单进程 in-memory；不持久化。
//! - 线程安全（parking_lot::Mutex 风格，但用 std::sync::Mutex 避免新增 dep）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 缓存条目。
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub value: Option<String>,
    pub fetched_at: Instant,
    pub ttl: Duration,
    /// 负缓存标记：value 为 None 且 not_found == true 表示上次解析"未找到"。
    pub not_found: bool,
}

impl CacheEntry {
    #[must_use]
    pub fn is_fresh(&self, now: Instant) -> bool {
        now.duration_since(self.fetched_at) < self.ttl
    }
}

/// 缓存统计。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub stores: u64,
    pub invalidations: u64,
    pub evictions: u64,
}

/// 线程安全的 secret TTL 缓存。
pub struct SecretCache {
    inner: Mutex<HashMap<String, CacheEntry>>,
    default_ttl: Duration,
    max_entries: usize,
    stats: Mutex<CacheStats>,
}

impl SecretCache {
    /// 构造一个默认 TTL = 5min、最大 1024 条目的缓存。
    #[must_use]
    pub fn new() -> Self {
        Self::with_ttl_and_capacity(Duration::from_secs(300), 1024)
    }

    #[must_use]
    pub fn with_ttl(ttl: Duration) -> Self {
        Self::with_ttl_and_capacity(ttl, 1024)
    }

    #[must_use]
    pub fn with_ttl_and_capacity(ttl: Duration, max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            default_ttl: ttl,
            max_entries,
            stats: Mutex::new(CacheStats::default()),
        }
    }

    /// 拉取一个 key；命中返回 Some(value)，miss 或 expired 返回 None。
    pub fn get(&self, key: &str) -> Option<CacheEntry> {
        let mut inner = self.inner.lock().expect("cache mutex");
        let mut stats = self.stats.lock().expect("stats mutex");
        if let Some(entry) = inner.get(key) {
            if entry.is_fresh(Instant::now()) {
                stats.hits += 1;
                return Some(entry.clone());
            }
        }
        stats.misses += 1;
        None
    }

    /// 写入正缓存（解析成功）。
    pub fn put(&self, key: impl Into<String>, value: impl Into<String>) {
        self.put_with_ttl(key.into(), value.into(), self.default_ttl);
    }

    /// 写入带自定义 TTL 的正缓存。
    pub fn put_with_ttl(&self, key: String, value: String, ttl: Duration) {
        let mut inner = self.inner.lock().expect("cache mutex");
        let mut stats = self.stats.lock().expect("stats mutex");
        // 容量保护：满则驱逐最旧条目。
        if inner.len() >= self.max_entries && !inner.contains_key(&key) {
            if let Some(oldest_key) = inner
                .iter()
                .min_by_key(|(_, v)| v.fetched_at)
                .map(|(k, _)| k.clone())
            {
                inner.remove(&oldest_key);
                stats.evictions += 1;
            }
        }
        inner.insert(
            key,
            CacheEntry {
                value: Some(value),
                fetched_at: Instant::now(),
                ttl,
                not_found: false,
            },
        );
        stats.stores += 1;
    }

    /// 写入负缓存（上次解析返回"未找到"）。
    pub fn put_not_found(&self, key: impl Into<String>) {
        let key = key.into();
        let mut inner = self.inner.lock().expect("cache mutex");
        let mut stats = self.stats.lock().expect("stats mutex");
        if inner.len() >= self.max_entries && !inner.contains_key(&key) {
            if let Some(oldest_key) = inner
                .iter()
                .min_by_key(|(_, v)| v.fetched_at)
                .map(|(k, _)| k.clone())
            {
                inner.remove(&oldest_key);
                stats.evictions += 1;
            }
        }
        inner.insert(
            key,
            CacheEntry {
                value: None,
                fetched_at: Instant::now(),
                ttl: self.default_ttl,
                not_found: true,
            },
        );
        stats.stores += 1;
    }

    /// 显式 invalidate。
    pub fn invalidate(&self, key: &str) -> bool {
        let mut inner = self.inner.lock().expect("cache mutex");
        let mut stats = self.stats.lock().expect("stats mutex");
        if inner.remove(key).is_some() {
            stats.invalidations += 1;
            true
        } else {
            false
        }
    }

    /// 全量清空。
    pub fn clear(&self) {
        let mut inner = self.inner.lock().expect("cache mutex");
        let mut stats = self.stats.lock().expect("stats mutex");
        stats.invalidations += inner.len() as u64;
        inner.clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().expect("cache mutex").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn stats(&self) -> CacheStats {
        *self.stats.lock().expect("stats mutex")
    }

    /// 清理所有已过期条目；返回清理数量。
    pub fn purge_expired(&self) -> usize {
        let mut inner = self.inner.lock().expect("cache mutex");
        let now = Instant::now();
        let before = inner.len();
        inner.retain(|_, v| v.is_fresh(now));
        before - inner.len()
    }
}

impl Default for SecretCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn r567_cache_put_get_round_trip() {
        let c = SecretCache::new();
        c.put("k1", "v1");
        let entry = c.get("k1").expect("hit");
        assert_eq!(entry.value.as_deref(), Some("v1"));
        assert!(!entry.not_found);
        assert_eq!(c.stats().hits, 1);
        assert_eq!(c.stats().misses, 0);
    }

    #[test]
    fn r567_cache_miss_increments_misses() {
        let c = SecretCache::new();
        assert!(c.get("nope").is_none());
        assert_eq!(c.stats().misses, 1);
    }

    #[test]
    fn r567_cache_ttl_expiry_evicts() {
        let c = SecretCache::with_ttl(Duration::from_millis(20));
        c.put("k1", "v1");
        sleep(Duration::from_millis(40));
        assert!(c.get("k1").is_none(), "should expire");
        assert_eq!(c.stats().misses, 1);
    }

    #[test]
    fn r567_cache_not_found_entry_marks_negative() {
        let c = SecretCache::new();
        c.put_not_found("missing-key");
        let entry = c.get("missing-key").expect("negative hit");
        assert!(entry.not_found);
        assert!(entry.value.is_none());
    }

    #[test]
    fn r567_cache_invalidate_removes_entry() {
        let c = SecretCache::new();
        c.put("k1", "v1");
        assert!(c.invalidate("k1"));
        assert!(c.get("k1").is_none());
        assert!(!c.invalidate("k1"), "double invalidate returns false");
        assert_eq!(c.stats().invalidations, 1);
    }

    #[test]
    fn r567_cache_clear_empties_all() {
        let c = SecretCache::new();
        c.put("a", "1");
        c.put("b", "2");
        c.clear();
        assert!(c.is_empty());
    }

    #[test]
    fn r567_cache_eviction_when_full() {
        let c = SecretCache::with_ttl_and_capacity(Duration::from_secs(60), 2);
        c.put("a", "1");
        c.put("b", "2");
        c.put("c", "3"); // 触发驱逐最旧
        assert_eq!(c.len(), 2);
        assert_eq!(c.stats().evictions, 1);
    }

    #[test]
    fn r567_cache_purge_expired() {
        let c = SecretCache::with_ttl(Duration::from_millis(10));
        c.put("a", "1");
        c.put("b", "2");
        sleep(Duration::from_millis(20));
        let purged = c.purge_expired();
        assert_eq!(purged, 2);
        assert!(c.is_empty());
    }
}
