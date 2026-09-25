//! installation token 的缓存与**单飞刷新** —— 上游 `ghsnapshot/client.go` 的 token 缓存部分
//! （M8-0 anchor 建桩，**实现归 M8-1**）。
//!
//! # 三条语义（`docs/61` §4.1 的 M8-1 行 / R-M8-3）
//!
//! 1. **提前续期**：token 存活一小时，续期余量 5 分钟（`tokenRenewSkew`）—— 让在飞的请求
//!    永远不会撞上过期边界；
//! 2. **单飞**：并发请求同时发现过期时，**只换一次** token（上游 `singleflight.Group`；
//!    本仓用 `tokio::sync::Mutex` / `OnceCell`）。`DoD`：并发 8 个请求只换 1 次 token；
//! 3. **脱敏**：token 不派生 `Debug`、不进日志、不进错误消息（`docs/61` §2.4）。

use std::collections::HashMap;
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

/// 每个 installation 一枚缓存的 token + 一把锁（单飞的临界区）。
#[derive(Default)]
pub struct InstallationTokenCache {
    /// `installation_id -> token`。
    entries: Mutex<HashMap<i64, InstallationToken>>,
    /// 续期余量（秒）。
    renew_skew_secs: i64,
}

impl InstallationTokenCache {
    /// 默认续期余量（上游 `tokenRenewSkew = 5 * time.Minute`）。
    pub const DEFAULT_RENEW_SKEW_SECS: i64 = 5 * 60;

    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
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
}

impl std::fmt::Debug for InstallationTokenCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallationTokenCache")
            .field("renew_skew_secs", &self.renew_skew_secs)
            .finish_non_exhaustive()
    }
}

/// 「取或换」的**单飞**入口 —— **anchor 期是桩**，实现归 M8-1。
///
/// 契约（`docs/61` §4.1 的 M8-1 行）：并发调用同一 `installation_id` 时**只发一次**
/// token 交换请求；交换失败不得污染缓存。
///
/// # Errors
///
/// 上游 token 端点失败 / 401 / 载荷非法时返回 [`GithubError`]。
pub async fn get_or_fetch(
    _cache: &Arc<InstallationTokenCache>,
    _installation_id: i64,
    _now_unix: i64,
) -> Result<InstallationToken, GithubError> {
    todo!("M8-1：单飞 token 交换 + 缓存（docs/61 §4.1 的 M8-1 行）")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_token_debug_is_redacted_and_freshness_uses_skew() {
        let token = InstallationToken::new("ghs_very_secret", 1_000);
        assert!(!format!("{token:?}").contains("secret"));
        // 余量 300s：now=650 ⇒ 1000-300=700 > 650 仍新鲜；now=750 ⇒ 不新鲜。
        assert!(token.is_fresh(650, 300));
        assert!(!token.is_fresh(750, 300));
    }
}
