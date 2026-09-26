//! `mc-entitlement` —— Multica 的 **entitlement 策略客户端**（`docs/62-M9-PLAN.md` §2.1 的
//! 「策略客户端」层）。
//!
//! # 这个 crate 是什么
//!
//! 云侧把「本工作区的套餐/配额策略」推送给本地（一个只读的策略端点），本地把它缓存在
//! 进程内、给每个 gate 判出一个 [`Decision`]（`off` / `observe` / `enforce`）。
//!
//! | 模块 | 写者 | 内容 |
//! | --- | --- | --- |
//! | [`types`] | **M9-0（本片）** | `GateName` / `Action` / `Reason` / `Gate` / `Decision` / `Provider` / `Observer` / 两类 outcome 词表（**完整类型形状**） |
//! | [`cache`] | M9-9 | 有界 LRU + 新鲜/陈旧/退避三档（**常量由 anchor 定死**） |
//! | [`client`] | M9-9 | 单飞刷新 + 超时 + `Provider` 实现（**端点路径由 anchor 定死**） |
//! | [`stub`] | M9-9 | `entitlementtest.Stub` 等价物（anchor 已给可用的最小实现） |
//!
//! # 上游对照
//!
//! `server/internal/entitlement/`（5 文件 / 751 行 = `client.go` 422 + `cache.go` 113 +
//! `types.go` 128 + `doc.go` 10 + `entitlementtest/stub.go` 78）。
//!
//! # 🔴 三条**不照抄**的地方（全部登记 `docs/32` §9.13）
//!
//! 1. **`Provider` 不带 `ctx`**：上游是 `Gate(ctx, workspaceID, name) Decision`，本仓是
//!    [`Provider::gate`]（同步、无 `ctx`）。理由：本仓**已有一个同形的接缝**
//!    （`mc_autopilot::quota::QuotaPolicyProvider`，模块头逐字「实现者必须**同步、无 IO**」），
//!    而 M9-9 的适配器要把两者接起来 ⇒ 形状必须一致；超时/取消由 client 自己的
//!    「单飞刷新 + 3s 超时」承担（`docs/62` §2.1）。
//! 2. **`Action::Observe` 不是线上取值**：上游 `normalizeGate` 只接受 `off` / `enforce`，
//!    `observe` 是**本地**在「策略已陈旧」时对 `enforce` 的**降级**（`decisionFromEntry`
//!    的 `stale && ActionEnforce ⇒ ActionObserve`）。本仓把这个事实写进类型文档，
//!    免得有人以为能从云侧收到 `observe`。
//! 3. **无 goroutine / 无后台生命周期**：上游逐字「It has no goroutines or background
//!    lifecycle」⇒ 本 crate 同样只在**调用时**刷新（Rust 侧不 spawn）。
//!
//! # 与本波其余部分的关系
//!
//! - **M9-9** 填 `cache.rs` / `client.rs`，并写 `apps/mc-server/src/entitlement.rs` 的适配器
//!   （它实现 `mc_autopilot::quota::QuotaPolicyProvider` 并调 `install_policy_provider`）；
//! - **`mc-cloud` 与本 crate 共用同一个 env**（`MULTICA_CLOUD_URL`）但**不同路径**
//!   （`/api/v1/internal/entitlement-policies/{id}` vs `/api/v1/billing/…`）⇒ 只有
//!   「一个基址解析」是共用件，不足以合并（`docs/62` §2.2 判据 3）。

pub mod cache;
pub mod client;
pub mod stub;
pub mod types;

pub use types::{
    off_decision, Action, CacheOutcome, Decision, Gate, GateName, NotificationPolicy, Observer,
    Provider, Reason, RefreshOutcome, SCHEMA_VERSION,
};

#[cfg(test)]
mod tests {
    use super::*;
    use mc_core::Id;

    /// 形状用例（`docs/62` §5 的 anchor 测试四类之一）：`Provider` **对象安全**
    /// （可 `Arc<dyn Provider>`），且默认平面给 `off`。
    #[test]
    fn provider_is_object_safe_and_the_default_plane_is_off() {
        let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(stub::Stub::new());
        let decision = provider.gate(Id::new(), GateName::IssueCount);
        assert_eq!(decision.gate.action, Action::Off);
        assert_eq!(decision.reason, Reason::Stub);
        assert_eq!(decision.policy_revision, 0);
    }
}
