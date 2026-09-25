//! installation token 的缓存与**单飞刷新** —— 上游 `ghsnapshot/client.go` 的 token 缓存部分
//! （**写者 M8-1**，`docs/61` §4.1 的 M8-1 行 / R-M8-3）。
//!
//! # 三条语义
//!
//! 1. **提前续期**：token 存活一小时，续期余量 5 分钟（上游 `tokenRenewSkew`）—— 让在飞的请求
//!    永远不会撞上过期边界；
//! 2. **单飞**：并发请求同时发现过期时，**只换一次** token（上游 `singleflight.Group`；
//!    本仓用「每 installation 一把 `tokio::sync::Mutex`」的等价物，**不新增依赖**）。
//!    `DoD`：并发 8 个请求只换 1 次 token；
//! 3. **脱敏**：token 不派生 `Debug`、不进日志、不进错误消息（`docs/61` §2.4）。
//!
//! # 为什么单飞要「锁 + 二次检查」而不是 `OnceCell`
//!
//! `OnceCell` 只能表达「一辈子一次」，而这里要表达「**每轮过期一次**」：token 到期后必须
//! 能再换一次。所以本文件用 `flights: Mutex<HashMap<i64, Arc<Mutex<()>>>>` —— 外层的 map 锁
//! 只用来**取那把飞行锁**（不跨 `await` 持有业务状态），飞行锁内先二次检查缓存、
//! 未命中才真的发请求。后到的调用者在飞行锁上排队，等到的一定是**新** token。
//!
//! **撤下来的 token 不会再被复用**：`put` 覆盖 `entries`，而 `get_fresh` 用
//! `expires_at - renew_skew > now` 判定，所以窗口内的旧值不会漏出。

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::rest::GithubError;

/// 一枚 installation access token（**不派生 `Debug`**）。
#[derive(Clone)]
pub struct InstallationToken {
    /// 裸 token 字符串（唯一出口是 [`InstallationToken::expose`]）。
    token: String,
    /// Unix 秒过期时刻。
    expires_at_unix: i64,
}

impl InstallationToken {
    /// 构造（生产在 token 交换处调用；测试用它造确定性 token）。
    pub fn new(token: impl Into<String>, expires_at_unix: i64) -> Self {
        Self {
            token: token.into(),
            expires_at_unix,
        }
    }

    /// 取裸 token（交给 `Authorization` 头）。**不要**把它塞进任何日志/错误。
    pub fn expose(&self) -> &str {
        &self.token
    }

    pub fn expires_at_unix(&self) -> i64 {
        self.expires_at_unix
    }

    /// 是否仍然新鲜（带续期余量）。
    pub fn is_fresh(&self, now_unix: i64, renew_skew_secs: i64) -> bool {
        self.expires_at_unix - renew_skew_secs > now_unix
    }
}

impl std::fmt::Debug for InstallationToken {
    /// 手写脱敏（不派生）：**绝不**打印 token 字节。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallationToken")
            .field("token", &"<redacted>")
            .field("expires_at_unix", &self.expires_at_unix)
            .finish()
    }
}

/// 每个 installation 一枚缓存的 token + 一把**飞行锁**（单飞的临界区）。
#[derive(Default)]
pub struct InstallationTokenCache {
    /// `installation_id -> token`。
    entries: Mutex<HashMap<i64, InstallationToken>>,
    /// `installation_id -> 飞行锁`（单飞；见模块头）。
    flights: Mutex<HashMap<i64, Arc<Mutex<()>>>>,
    /// 续期余量（秒）。
    renew_skew_secs: i64,
}

impl InstallationTokenCache {
    /// 默认续期余量（上游 `tokenRenewSkew = 5 * time.Minute`）。
    pub const DEFAULT_RENEW_SKEW_SECS: i64 = 5 * 60;

    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            flights: Mutex::new(HashMap::new()),
            renew_skew_secs: Self::DEFAULT_RENEW_SKEW_SECS,
        }
    }

    /// 自定义续期余量（测试注入）。
    pub fn with_renew_skew(renew_skew_secs: i64) -> Self {
        let mut cache = Self::new();
        cache.renew_skew_secs = renew_skew_secs;
        cache
    }

    pub fn renew_skew_secs(&self) -> i64 {
        self.renew_skew_secs
    }

    /// 命中缓存返回 clone；未命中 / 不新鲜返回 `None`。**不**在这里发请求。
    pub async fn get_fresh(
        &self,
        installation_id: i64,
        now_unix: i64,
    ) -> Option<InstallationToken> {
        let entries = self.entries.lock().await;
        entries
            .get(&installation_id)
            .filter(|token| token.is_fresh(now_unix, self.renew_skew_secs))
            .cloned()
    }

    /// 写入一枚新 token（生产在交换成功后调用）。
    pub async fn put(&self, installation_id: i64, token: InstallationToken) {
        self.entries.lock().await.insert(installation_id, token);
    }

    /// 缓存里的 installation id 数（诊断/测试）。
    pub async fn len(&self) -> usize {
        self.entries.lock().await.len()
    }

    /// 是否为空。
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// 取该 installation 的飞行锁（不存在则新建）。
    async fn flight_lock(&self, installation_id: i64) -> Arc<Mutex<()>> {
        let mut flights = self.flights.lock().await;
        flights
            .entry(installation_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

impl std::fmt::Debug for InstallationTokenCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallationTokenCache")
            .field("renew_skew_secs", &self.renew_skew_secs)
            .finish_non_exhaustive()
    }
}

/// 「取或换」的**单飞**入口（上游 `Client.installationToken` + `singleflight.Group`）。
///
/// 语义逐条：
/// 1. 缓存新鲜 ⇒ 直接返回，**不**调用 `fetch`；
/// 2. 不新鲜 ⇒ 拿该 installation 的飞行锁：拿到后**二次检查**（等锁期间别人可能已经换好），
///    仍未命中才调用 `fetch`；
/// 3. `fetch` 失败 ⇒ 直接返回错误，**不污染缓存**（下一次调用会重试，而不是把失败缓存住）。
///
/// `fetch` 是 `FnOnce`（只在真的要发请求时调用一次）—— 这也是「8 个并发只换 1 次」的机制：
/// 后到的 7 个在飞行锁上排队，进来时二次检查已命中。
///
/// # Errors
///
/// `fetch` 的错误原样上抛（含 [`GithubError`] 的全部变体）。
pub async fn get_or_fetch<F, Fut>(
    cache: &Arc<InstallationTokenCache>,
    installation_id: i64,
    now_unix: i64,
    fetch: F,
) -> Result<InstallationToken, GithubError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<InstallationToken, GithubError>>,
{
    if let Some(token) = cache.get_fresh(installation_id, now_unix).await {
        return Ok(token);
    }
    let lock = cache.flight_lock(installation_id).await;
    let _guard = lock.lock().await;
    // 二次检查：等锁期间可能已经有别的调用者换好了。
    if let Some(token) = cache.get_fresh(installation_id, now_unix).await {
        return Ok(token);
    }
    let token = fetch().await?;
    cache.put(installation_id, token.clone()).await;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn installation_token_debug_is_redacted_and_freshness_uses_skew() {
        let token = InstallationToken::new("ghs_very_secret", 1_000);
        assert!(!format!("{token:?}").contains("secret"));
        // 余量 300s：now=650 ⇒ 1000-300=700 > 650 仍新鲜；now=750 ⇒ 不新鲜。
        assert!(token.is_fresh(650, 300));
        assert!(!token.is_fresh(750, 300));
    }

    #[tokio::test]
    async fn cache_hit_skips_the_fetch_entirely() {
        let cache = Arc::new(InstallationTokenCache::new());
        cache.put(7, InstallationToken::new("cached", 10_000)).await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let token = get_or_fetch(&cache, 7, 1_000, || async move {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(InstallationToken::new("fresh", 20_000))
        })
        .await
        .expect("cached token");
        assert_eq!(token.expose(), "cached");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// `DoD`：并发 8 个请求只换 1 次 token（冷缓存）。
    #[tokio::test]
    async fn eight_concurrent_callers_mint_exactly_one_token() {
        let cache = Arc::new(InstallationTokenCache::new());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // 让 fetch 里真的有一次 `await`（否则并发窗口塌缩成顺序执行，测不到单飞）。
        let delay = tokio::time::Duration::from_millis(30);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            let calls = calls.clone();
            handles.push(tokio::spawn(async move {
                get_or_fetch(&cache, 99, 1_000, || async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    Ok(InstallationToken::new("minted", 4_600))
                })
                .await
            }));
        }
        let mut tokens = Vec::new();
        for handle in handles {
            tokens.push(
                handle
                    .await
                    .expect("join")
                    .expect("token")
                    .expose()
                    .to_string(),
            );
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(tokens.iter().all(|t| t == "minted"));
        assert_eq!(cache.len().await, 1);
    }

    #[tokio::test]
    async fn stale_token_is_renewed_and_failure_does_not_poison_the_cache() {
        let cache = Arc::new(InstallationTokenCache::new());
        // 已过期（expiry - skew <= now）。
        cache.put(1, InstallationToken::new("old", 1_000)).await;
        let err = get_or_fetch(&cache, 1, 1_000, || async {
            Err::<InstallationToken, _>(GithubError::Unauthorized)
        })
        .await
        .expect_err("fetch fails");
        assert!(matches!(err, GithubError::Unauthorized));
        // 失败不污染：旧值原样留着（且仍被判为不新鲜）。
        assert!(cache.get_fresh(1, 1_000).await.is_none());
        assert_eq!(cache.len().await, 1);

        let fresh = get_or_fetch(&cache, 1, 1_000, || async {
            Ok(InstallationToken::new("new", 5_000))
        })
        .await
        .expect("renewed");
        assert_eq!(fresh.expose(), "new");
    }

    #[tokio::test]
    async fn renew_skew_boundary_is_exclusive_on_the_left() {
        let cache = Arc::new(InstallationTokenCache::with_renew_skew(300));
        assert_eq!(cache.renew_skew_secs(), 300);
        cache.put(5, InstallationToken::new("t", 1_000)).await;
        // 1000 - 300 = 700 > 700 ⇒ false（恰好等于余量边界时**算过期**）。
        assert!(cache.get_fresh(5, 700).await.is_none());
        assert!(cache.get_fresh(5, 699).await.is_some());
    }
}
