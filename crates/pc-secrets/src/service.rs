//! Secret service 层：包装 cache + rotation policy + 解析缓存。
//!
//! 与 paperclip 上游 `services/secrets.ts` 思路一致：
//! - 上层 route 调用 service 而不是直接用 Repo
//! - service 负责：解析缓存（cache-aside）+ 轮换策略评估 + 失效广播
//!
//! 与上游不同的是：本 crate 仍保留 `SecretRepo`（pc-repos）作为
//! 纯持久化层，service 在 Repo 之上加缓存 / 策略 / 错误归一化。

use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::cache::{CacheStats, SecretCache};
use crate::error::SecretProviderError;
use crate::rotation::{
    evaluate_rotation, RotationEvaluation, RotationEvaluationInput, RotationPolicyConfig,
};

/// service 错误（向调用方归一化）。
#[derive(Debug, thiserror::Error)]
pub enum SecretServiceError {
    #[error("provider error: {0}")]
    Provider(#[from] SecretProviderError),
    #[error("repository error: {0}")]
    Repo(String),
    #[error("secret not found")]
    NotFound,
    #[error("invalid cache key: {0}")]
    InvalidKey(String),
}

pub type SecretServiceResult<T> = Result<T, SecretServiceError>;

/// 业务级 secret service。
///
/// 公开 API：
/// - 缓存管理：`cache_stats` / `clear_cache` / `invalidate`
/// - 轮换策略：`evaluate` / `evaluate_secret_age`
/// - 解析缓存：`resolve_cached`（cache-aside loader 闭包）
/// - 失效广播：`invalidate_after_write` / `invalidate_after_rotate`
#[derive(Clone)]
pub struct SecretService {
    cache: Arc<SecretCache>,
    rotation_policy: Arc<RotationPolicyConfig>,
}

impl Default for SecretService {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretService {
    /// 默认配置（无 TTL 上限，默认 cache + 默认 rotation policy）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: Arc::new(SecretCache::new()),
            rotation_policy: Arc::new(RotationPolicyConfig::default()),
        }
    }

    /// 指定 cache 注入（典型用法：测试或共享全局 cache）。
    #[must_use]
    pub fn with_cache(cache: Arc<SecretCache>) -> Self {
        Self {
            cache,
            rotation_policy: Arc::new(RotationPolicyConfig::default()),
        }
    }

    /// 指定 rotation policy。
    #[must_use]
    pub fn with_rotation_policy(mut self, policy: RotationPolicyConfig) -> Self {
        self.rotation_policy = Arc::new(policy);
        self
    }

    /// 同时指定 cache 与 rotation policy。
    #[must_use]
    pub fn with_config(cache: Arc<SecretCache>, policy: RotationPolicyConfig) -> Self {
        Self {
            cache,
            rotation_policy: Arc::new(policy),
        }
    }

    /// 当前 cache 引用（供 route 层读 stats）。
    #[must_use]
    pub fn cache(&self) -> &SecretCache {
        &self.cache
    }

    /// 当前 rotation policy 引用。
    #[must_use]
    pub fn rotation_policy(&self) -> &RotationPolicyConfig {
        &self.rotation_policy
    }

    // ------------------------------------------------------------------
    // Cache API
    // ------------------------------------------------------------------

    /// Cache stats 快照。
    #[must_use]
    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats()
    }

    /// 清空全部 cache（运维 / 测试用）。
    pub fn clear_cache(&self) -> usize {
        self.cache.clear();
        self.cache.len()
    }

    /// 按 key 主动失效（write-through 场景）。
    /// 返回 `true` 表示此前有命中。
    #[must_use]
    pub fn invalidate(&self, key: &str) -> bool {
        self.cache.invalidate(key)
    }

    /// 按 company_id + secret_id 失效（rotate / delete 后调用）。
    #[must_use]
    pub fn invalidate_secret(&self, company_id: uuid::Uuid, secret_id: uuid::Uuid) -> bool {
        self.invalidate(&cache_key(company_id, secret_id))
    }

    // ------------------------------------------------------------------
    // Rotation API
    // ------------------------------------------------------------------

    /// 给定完整 input，评估轮换策略。
    #[must_use]
    pub fn evaluate(&self, input: &RotationEvaluationInput) -> RotationEvaluation {
        evaluate_rotation(&self.rotation_policy, input, Utc::now())
    }

    /// 简化的轮换评估（只需要 created_at + last_rotated_at）。
    #[must_use]
    pub fn evaluate_secret_age(
        &self,
        created_at: DateTime<Utc>,
        last_rotated_at: Option<DateTime<Utc>>,
    ) -> RotationEvaluation {
        let input = RotationEvaluationInput {
            created_at,
            last_rotated_at,
            use_count: 0,
            manual: false,
            emergency: false,
        };
        self.evaluate(&input)
    }

    /// 强制 manual rotation 的评估（用户主动 rotate 按钮）。
    #[must_use]
    pub fn evaluate_manual_rotation(
        &self,
        created_at: DateTime<Utc>,
        last_rotated_at: Option<DateTime<Utc>>,
    ) -> RotationEvaluation {
        let input = RotationEvaluationInput {
            created_at,
            last_rotated_at,
            use_count: 0,
            manual: true,
            emergency: false,
        };
        self.evaluate(&input)
    }

    // ------------------------------------------------------------------
    // Resolve cached (cache-aside)
    // ------------------------------------------------------------------

    /// Cache-aside 解析 secret value。
    ///
    /// 命中：直接返回缓存的 value。
    /// 未命中：调用 `loader` 闭包去 Repo / provider 读取；返回 `Some` 写入 cache。
    /// 闭包返回 `None`：写入 negative cache（避免反复穿透）。
    pub async fn resolve_cached<F, Fut>(
        &self,
        company_id: uuid::Uuid,
        secret_id: uuid::Uuid,
        loader: F,
    ) -> SecretServiceResult<Option<String>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = SecretServiceResult<Option<String>>>,
    {
        let key = cache_key(company_id, secret_id);
        if let Some(entry) = self.cache.get(&key) {
            return match entry.value.as_deref() {
                Some(v) => Ok(Some(v.to_string())),
                None => Ok(None),
            };
        }
        let now = Instant::now();
        let value = loader().await?;
        match &value {
            Some(v) => self.cache.put(&key, v.clone()),
            None => self.cache.put_not_found(&key),
        }
        let _ = now;
        Ok(value)
    }

    /// 同步版解析缓存（用于 fn pointer loader）。
    pub fn resolve_cached_sync<F>(
        &self,
        company_id: uuid::Uuid,
        secret_id: uuid::Uuid,
        loader: F,
    ) -> SecretServiceResult<Option<String>>
    where
        F: FnOnce() -> SecretServiceResult<Option<String>>,
    {
        let key = cache_key(company_id, secret_id);
        if let Some(entry) = self.cache.get(&key) {
            return match entry.value.as_deref() {
                Some(v) => Ok(Some(v.to_string())),
                None => Ok(None),
            };
        }
        let value = loader()?;
        match &value {
            Some(v) => self.cache.put(&key, v.clone()),
            None => self.cache.put_not_found(&key),
        }
        Ok(value)
    }

    // ------------------------------------------------------------------
    // Write-through 失效辅助
    // ------------------------------------------------------------------

    /// 写入后失效（create / update 后调用）。
    #[must_use]
    pub fn invalidate_after_write(&self, company_id: uuid::Uuid, secret_id: uuid::Uuid) -> bool {
        self.invalidate_secret(company_id, secret_id)
    }

    /// 轮换后失效（rotate_secret 后调用）。
    pub fn invalidate_after_rotate(&self, company_id: uuid::Uuid, secret_id: uuid::Uuid) -> bool {
        self.invalidate_secret(company_id, secret_id)
    }

    /// 删除后失效（soft_delete / hard_delete 后调用）。
    pub fn invalidate_after_delete(&self, company_id: uuid::Uuid, secret_id: uuid::Uuid) -> bool {
        self.invalidate_secret(company_id, secret_id)
    }
}

/// 标准化的 cache key：`secret:{company_id}:{secret_id}`。
#[must_use]
pub fn cache_key(company_id: uuid::Uuid, secret_id: uuid::Uuid) -> String {
    format!("secret:{company_id}:{secret_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::SecretCache;
    use chrono::Duration;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn svc_with_cache(cache: &Arc<SecretCache>) -> SecretService {
        SecretService::with_cache(cache.clone())
    }

    #[test]
    fn r570_default_service_has_no_rotation_policy() {
        let s = SecretService::new();
        let created = Utc::now() - Duration::days(30);
        let eval = s.evaluate_secret_age(created, None);
        assert!(!eval.should_rotate());
    }

    #[test]
    fn r570_service_with_max_age_rotates() {
        let mut policy = RotationPolicyConfig::default();
        policy.max_age = Some(Duration::days(30));
        let s = SecretService::new().with_rotation_policy(policy);
        let created = Utc::now() - Duration::days(60);
        let eval = s.evaluate_secret_age(created, None);
        assert!(eval.should_rotate());
    }

    #[test]
    fn r570_manual_rotation_always_required() {
        let s = SecretService::new();
        let now = Utc::now();
        let eval = s.evaluate_manual_rotation(now, Some(now));
        assert!(eval.should_rotate(), "manual flag should always trigger");
    }

    #[tokio::test]
    async fn r570_resolve_cached_hit_returns_cached_value() {
        let cache = Arc::new(SecretCache::new());
        let s = svc_with_cache(&cache);
        let cid = uuid::Uuid::new_v4();
        let sid = uuid::Uuid::new_v4();
        let key = cache_key(cid, sid);
        s.cache.put(&key, "cached-value");
        let called = AtomicU32::new(0);
        let r = s
            .resolve_cached(cid, sid, || async {
                called.fetch_add(1, Ordering::SeqCst);
                Ok(Some("loader-value".into()))
            })
            .await
            .unwrap();
        assert_eq!(r.as_deref(), Some("cached-value"));
        assert_eq!(
            called.load(Ordering::SeqCst),
            0,
            "loader should not run on cache hit"
        );
    }

    #[tokio::test]
    async fn r570_resolve_cached_miss_invokes_loader_and_writes_cache() {
        let cache = Arc::new(SecretCache::new());
        let s = svc_with_cache(&cache);
        let cid = uuid::Uuid::new_v4();
        let sid = uuid::Uuid::new_v4();
        let called = AtomicU32::new(0);
        let r = s
            .resolve_cached(cid, sid, || async {
                called.fetch_add(1, Ordering::SeqCst);
                Ok(Some("loader-value".into()))
            })
            .await
            .unwrap();
        assert_eq!(r.as_deref(), Some("loader-value"));
        assert_eq!(called.load(Ordering::SeqCst), 1);
        let r2 = s
            .resolve_cached(cid, sid, || async {
                called.fetch_add(1, Ordering::SeqCst);
                Ok(Some("loader-value-2".into()))
            })
            .await
            .unwrap();
        assert_eq!(
            r2.as_deref(),
            Some("loader-value"),
            "second call should be cached"
        );
        assert_eq!(called.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn r570_resolve_cached_miss_writes_negative_cache_for_none() {
        let cache = Arc::new(SecretCache::with_ttl(std::time::Duration::from_millis(50)));
        let s = svc_with_cache(&cache);
        let cid = uuid::Uuid::new_v4();
        let sid = uuid::Uuid::new_v4();
        let called = AtomicU32::new(0);
        let r = s
            .resolve_cached(cid, sid, || async {
                called.fetch_add(1, Ordering::SeqCst);
                Ok::<Option<String>, SecretServiceError>(None)
            })
            .await
            .unwrap();
        assert!(r.is_none());
        let r2 = s
            .resolve_cached(cid, sid, || async {
                called.fetch_add(1, Ordering::SeqCst);
                Ok(Some("nope".into()))
            })
            .await
            .unwrap();
        assert!(r2.is_none(), "negative cache should prevent loader re-run");
        assert_eq!(called.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn r570_resolve_cached_loader_error_propagates_without_caching() {
        let cache = Arc::new(SecretCache::new());
        let s = svc_with_cache(&cache);
        let cid = uuid::Uuid::new_v4();
        let sid = uuid::Uuid::new_v4();
        let r = s
            .resolve_cached(cid, sid, || async {
                Err::<Option<String>, _>(SecretServiceError::NotFound)
            })
            .await;
        assert!(matches!(r, Err(SecretServiceError::NotFound)));
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn r570_resolve_cached_sync_hit_returns_cached_value() {
        let cache = Arc::new(SecretCache::new());
        let s = svc_with_cache(&cache);
        let cid = uuid::Uuid::new_v4();
        let sid = uuid::Uuid::new_v4();
        s.cache.put(&cache_key(cid, sid), "sync-cached");
        let r = s
            .resolve_cached_sync(cid, sid, || Ok(Some("loader".into())))
            .unwrap();
        assert_eq!(r.as_deref(), Some("sync-cached"));
    }

    #[test]
    fn r570_invalidate_after_write_clears_cache() {
        let cache = Arc::new(SecretCache::new());
        let s = svc_with_cache(&cache);
        let cid = uuid::Uuid::new_v4();
        let sid = uuid::Uuid::new_v4();
        s.cache.put(&cache_key(cid, sid), "before");
        assert_eq!(cache.len(), 1);
        assert!(s.invalidate_after_write(cid, sid));
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn r570_invalidate_after_rotate_and_delete_alias_to_invalidate() {
        let cache = Arc::new(SecretCache::new());
        let s = svc_with_cache(&cache);
        let cid = uuid::Uuid::new_v4();
        let sid = uuid::Uuid::new_v4();
        s.cache.put(&cache_key(cid, sid), "v1");
        s.invalidate_after_rotate(cid, sid);
        assert_eq!(cache.len(), 0);
        s.cache.put(&cache_key(cid, sid), "v2");
        s.invalidate_after_delete(cid, sid);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn r570_clear_cache_resets_stats() {
        let cache = Arc::new(SecretCache::new());
        let s = svc_with_cache(&cache);
        s.cache.put("k1", "v1");
        s.cache.put("k2", "v2");
        let stats = s.cache_stats();
        assert!(stats.stores >= 2);
        s.clear_cache();
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn r570_cache_key_format() {
        let cid = uuid::Uuid::nil();
        let sid = uuid::Uuid::nil();
        assert_eq!(
            cache_key(cid, sid),
            "secret:00000000-0000-0000-0000-000000000000:00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn r570_service_clone_shares_cache() {
        let cache = Arc::new(SecretCache::new());
        let s1 = svc_with_cache(&cache);
        let s2 = s1.clone();
        s1.cache.put("shared", "v");
        assert_eq!(s2.cache.get("shared").unwrap().value.as_deref(), Some("v"));
    }

    #[test]
    fn r570_evaluate_with_max_uses_triggers_rotation() {
        let mut policy = RotationPolicyConfig::default();
        policy.max_uses = Some(10);
        let s = SecretService::new().with_rotation_policy(policy);
        let now = Utc::now();
        let input = RotationEvaluationInput {
            created_at: now,
            last_rotated_at: Some(now),
            use_count: 100,
            manual: false,
            emergency: false,
        };
        let eval = s.evaluate(&input);
        assert!(eval.should_rotate());
    }
}
