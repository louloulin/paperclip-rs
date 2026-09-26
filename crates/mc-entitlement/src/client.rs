//! 策略客户端的**端点契约** —— 实现（单飞刷新 + 超时 + `Provider`）归 **M9-9**。
//!
//! anchor 期本文件只钉**一件事**：出站端点的**路径形状**，因为它是唯一一个
//! 「上游事实、不由实现自定」的字符串。逐字对照上游 `internal/entitlement/client.go:241`：
//!
//! ```text
//! u.Path = strings.TrimRight(c.baseURL.Path, "/") + "/api/v1/internal/entitlement-policies/" + workspaceID.String()
//! ```
//!
//! # M9-9 要填什么（上游 `client.go` 422 行）
//!
//! 1. `Client::new(cfg)` 的**三态**：空基址 ⇒ 禁用的客户端（`Ok`，**不是**错误，
//!    上游 `if strings.TrimSpace(cfg.BaseURL) == "" { return &Client{…}, nil }`）；
//!    非空非法 ⇒ `Err`（上游 `ErrInvalidConfig`，三件套 = 绝对 URL + 无凭据 + 无 query/fragment
//!    —— **复用 `mc_cloud::config::validate`**，别写第二份，`docs/62` §2.2 判据 3）；
//!    非空合法 ⇒ 启用的客户端；
//! 2. **单飞刷新**（每工作区一个 in-flight；Rust 侧用 `tokio::sync::Mutex` +
//!    「拿锁的人刷新、其余人等结果」的形态即可 —— 不引入 `singleflight` 依赖）；
//! 3. **不跟随跨源重定向**（上游 `CheckRedirect` 逐字返回 `http.ErrUseLastResponse`）；
//! 4. **三档时效**（[`crate::cache`] 的常量）：新鲜 ⇒ 直接用；陈旧 ⇒ `enforce` 降级为
//!    `observe`（[`crate::Gate::downgraded_when_stale`]）；退避期 ⇒ 不重试；
//! 5. **校验**（上游 `normalizePolicy` / `normalizeGate`）：`schema_version == 1`、
//!    `policy_revision > 0`、`subscription_version >= 0`、`valid_until` 非零、
//!    `0 < valid_for_seconds <= MAX_POLICY_TTL`、两个 gate **都必须在 `gates` 里**、
//!    每个 gate 的 `action ∈ {off, enforce}`、`limit >= 0`、
//!    period 三字段「全有或全无」、`autopilot_runs` **必须**有 period 三字段、
//!    `period_start < period_end` 且 `period_start < reset_at`；
//! 6. **版本回退不写缓存**（[`crate::cache::MAX_ENTRIES`] 段的第 3 条纪律）；
//! 7. **`Observer` 的四个调用点**（[`crate::Observer`]，全部低基数）。
//!
//! # 为什么 anchor 不写一个 `todo!()` 的 `Client`
//!
//! 一个 `pub fn new() -> Self { todo!() }` 在**编译期**看着像接好了、在**运行期**是 panic
//! （`docs/37` 反复登记的那类"静默假接入"的镜像）。anchor 的交付形态是：**端点路径可用**
//! （本文件的 [`policy_endpoint_path`]）+ **适配器诚实空跑**
//! （`apps/mc-server/src/entitlement.rs` 在配了基址时**不装平面并 warn**）。
//! ⇒ 没有任何路径能走进未实现的代码。

use mc_core::Id;

/// 上游 `client.go:241` 拼出的端点路径（**不含** base URL，也不含任何 query）。
///
/// `workspace_id` 是**唯一**的输入 —— 端点不回显、也不接受别的租户维度
/// （上游注释逐字：缓存键「is never inferred from response data」）。
#[must_use]
pub fn policy_endpoint_path(workspace_id: Id) -> String {
    format!("/api/v1/internal/entitlement-policies/{workspace_id}")
}

/// 端点路径前缀（给「是不是我们的策略端点」这类诊断用）。
pub const POLICY_ENDPOINT_PREFIX: &str = "/api/v1/internal/entitlement-policies/";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_path_matches_upstream_and_carries_only_the_workspace() {
        let id = Id::parse("11111111-2222-3333-4444-555555555555").expect("uuid");
        let path = policy_endpoint_path(id);
        assert_eq!(
            path,
            "/api/v1/internal/entitlement-policies/11111111-2222-3333-4444-555555555555"
        );
        assert!(path.starts_with(POLICY_ENDPOINT_PREFIX));
        assert!(!path.contains('?'), "上游逐字 RawQuery = \"\"");
        assert!(!path.contains('#'));
        // 不同工作区只有一个路径段不同（没有第二处租户维度）。
        let other = policy_endpoint_path(Id::nil());
        assert_eq!(
            other.trim_start_matches(POLICY_ENDPOINT_PREFIX),
            Id::nil().to_string()
        );
    }
}
