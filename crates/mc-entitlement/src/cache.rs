//! 有界策略缓存的**常量与词表** —— 实现（LRU + 三档时效）归 **M9-9**。
//!
//! 本文件在 anchor 期只定**常量**：六个数字逐字取自上游 `internal/entitlement/cache.go`
//! 与 `client.go:31-39`。把它们钉在这里（而不是散在 M9-9 的实现里）的理由与
//! `mc-cloud::transport` 的 `DEFAULT_TIMEOUT` / `MAX_RESPONSE_BODY_SIZE` 相同：
//! **判据只有一处**，M9-9 的用例与实现不会各自抄一份数字。
//!
//! # 三档时效的语义（上游 `cacheEntry`，M9-9 照着实现）
//!
//! ```text
//! receivedAt ──freshUntil──> (新鲜: 直接用, Reason::CacheFresh)
//!            ──staleUntil──> (陈旧但可用: enforce 降级为 observe, Reason::Stale)
//!            ──retryAfter──> (退避中: 不重试; 无策略 ⇒ Reason::Unavailable)
//! ```
//!
//! - `freshUntil = receivedAt + valid_for_seconds`（**回话说了算**）；
//! - `staleUntil = freshUntil + grace`（[`STALE_GRACE`]）；
//! - `retryAfter` 只在**失败**时推进（[`FAILURE_RETRY`]）；
//! - `valid_for_seconds` 本身被 [`MAX_POLICY_TTL`] **上界**校验（超过即 `InvalidPolicy`）。
//!
//! # 三条纪律（M9-9 的 `DoD` 会逐条点名）
//!
//! 1. **缓存键只能是 `Id`**（上游注释逐字：「The workspace key is never inferred from
//!    response data, preventing one tenant's policy from entering another tenant's cache
//!    slot」）⇒ 响应体里**不得**有 workspace 字段可用；
//! 2. **上限是 `MAX_ENTRIES` 的 LRU**（`get` 会 `MoveToFront`）；
//! 3. **版本回退 ⇒ 拒绝写入**（上游 `cache.put` 返回 `errVersionRegression`）——
//!    即「新策略的 `subscription_version` 比缓存里的小」时**保留旧的**并让刷新失败。

use std::time::Duration;

/// 上游 `defaultMaxEntries`（`client.go:33`）：LRU 上限。
pub const MAX_ENTRIES: usize = 10_000;

/// 上游 `defaultStaleGrace`（`client.go:34`）：陈旧宽限 —— 过了新鲜期后还能用多久。
pub const STALE_GRACE: Duration = Duration::from_mins(15);

/// 上游 `defaultFailureRetry`（`client.go:35`）：失败后的退避间隔。
pub const FAILURE_RETRY: Duration = Duration::from_secs(5);

/// 上游 `maxPolicyTTL`（`client.go:36`）：**回话**的 `valid_for_seconds` 的硬上界
/// （超过即 [`crate::Reason::InvalidPolicy`]）。
pub const MAX_POLICY_TTL: Duration = Duration::from_mins(5);

/// 上游 `maxResponseBodySize`（`client.go:37`）：策略响应体上限。
pub const MAX_RESPONSE_BODY_SIZE: usize = 64 << 10;

/// 上游 `defaultRequestTimeout`（`client.go:32`）：**每次** fetch 的超时
/// （单飞刷新共用同一个超时，见上游 `refresh` 的 `context.WithoutCancel` 注释）。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(test)]
mod tests {
    use super::*;

    /// 六个数字必须在类型层面就能被 M9-9 复用（不许各抄一份）。
    #[test]
    fn upstream_limits_are_pinned_once() {
        assert_eq!(MAX_ENTRIES, 10_000);
        assert_eq!(STALE_GRACE, Duration::from_mins(15));
        assert_eq!(FAILURE_RETRY, Duration::from_secs(5));
        assert_eq!(MAX_POLICY_TTL, Duration::from_mins(5));
        assert_eq!(MAX_RESPONSE_BODY_SIZE, 64 * 1024);
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(3));
        // 三档时效的序关系（实现里 `freshUntil < staleUntil`，退避与它们独立）。
        assert!(STALE_GRACE > Duration::ZERO);
        assert!(MAX_POLICY_TTL > Duration::ZERO);
    }
}
