//! 确定性替身平面（上游 `internal/entitlement/entitlementtest/stub.go` 的等价物）。
//!
//! anchor 期给一份**可用的最小实现**：它不联网、不为任何工作区编造策略、每次调用都记一笔
//! `(workspace_id, gate)`。M9-9 的「套餐/配额矩阵」18 格（`docs/62` §9.8 判据：3 个
//! `Action` × 2 个 `Gate` × 3 个缓存态）就是靠它跑出来的 —— 那一波会把它扩到需要的形状。
//!
//! # 三条与上游逐字一致的语义
//!
//! 1. **`set` 会把 `reason` 强制改成 [`Reason::Stub`]**（上游逐字
//!    `decision.Reason = entitlement.ReasonStub`）⇒ 替身结论**永远不会**被误当成真平面；
//! 2. **未设置的工作区 / gate ⇒ `off` + `Reason::Stub`**（fail-open，不 panic）；
//! 3. **`set` 与 `gate` 都返回克隆**（上游 `cloneDecision`）⇒ 调用方改不动内部状态。
//!
//! # 并发
//!
//! 上游用 `sync.RWMutex`。本仓用 `parking_lot`… **不**：本 crate 的依赖边被 anchor 冻结
//! （`Cargo.toml` 逐字「M9-9 的写者不得再新增三方依赖」），而 `parking_lot` 不在表里 ⇒
//! 用 `std::sync::RwLock`（毒化在锁范围内不会发生：`gate` 的临界区不 panic）。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use mc_core::Id;

use crate::types::{off_decision, Decision, GateName, Provider, Reason};

/// 一次调用记录（上游 `entitlementtest.Call`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Call {
    /// 被问的工作区。
    pub workspace_id: Id,
    /// 被问的 gate。
    pub gate: GateName,
}

/// 确定性替身（`Provider` 实现）。
#[derive(Debug, Default)]
pub struct Stub {
    decisions: RwLock<HashMap<(Id, GateName), Decision>>,
    calls: RwLock<Vec<Call>>,
}

impl Stub {
    /// 空替身：任何问题都答 `off` + `Reason::Stub`。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 用 `Arc` 包一个（`install_policy_provider` 的实参形态）。
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// 设置某工作区某 gate 的判决（`reason` 强制为 [`Reason::Stub`]）。
    pub fn set(&self, workspace_id: Id, name: GateName, mut decision: Decision) {
        decision.reason = Reason::Stub;
        if let Ok(mut guard) = self.decisions.write() {
            guard.insert((workspace_id, name), decision);
        }
    }

    /// 清空某工作区的全部判决（回到 fail-open）。
    pub fn clear_workspace(&self, workspace_id: Id) {
        if let Ok(mut guard) = self.decisions.write() {
            guard.retain(|(candidate, _), _| *candidate != workspace_id);
        }
    }

    /// 至今收到的全部调用（**顺序**，可重复）。
    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        self.calls
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// 调用次数（诊断用）。
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls.read().map_or(0, |guard| guard.len())
    }
}

impl Provider for Stub {
    fn gate(&self, workspace_id: Id, name: GateName) -> Decision {
        if let Ok(mut guard) = self.calls.write() {
            guard.push(Call {
                workspace_id,
                gate: name,
            });
        }
        self.decisions
            .read()
            .ok()
            .and_then(|guard| guard.get(&(workspace_id, name)).cloned())
            .unwrap_or_else(|| off_decision(Reason::Stub))
    }

    /// 替身恒「启用」（它不依赖任何部署密钥）。
    fn is_enabled(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Action, Gate};
    use mc_core::Timestamp;

    #[test]
    fn unset_workspaces_fail_open_and_are_recorded() {
        let stub = Stub::new();
        let workspace = Id::new();
        let decision = stub.gate(workspace, GateName::AutopilotRuns);
        assert_eq!(decision.reason, Reason::Stub);
        assert_eq!(decision.gate, Gate::off());
        assert!(!decision.is_enforcing());
        assert_eq!(
            stub.calls(),
            vec![Call {
                workspace_id: workspace,
                gate: GateName::AutopilotRuns
            }]
        );
        assert_eq!(stub.call_count(), 1);
    }

    #[test]
    fn set_forces_the_stub_reason_and_returns_clones() {
        let stub = Stub::new();
        let workspace = Id::new();
        let mut gate = Gate::off();
        gate.action = Action::Enforce;
        gate.limit = Some(7);
        gate.period_start = Some(Timestamp::default());
        gate.period_end = Some(Timestamp::default());
        gate.reset_at = Some(Timestamp::default());
        stub.set(
            workspace,
            GateName::IssueCount,
            Decision {
                gate: gate.clone(),
                // 传入一个**真平面**的 reason，替身必须把它改写掉。
                reason: Reason::Refreshed,
                policy_revision: 3,
                subscription_version: 9,
                cloud_valid_until: Timestamp::default(),
            },
        );

        let first = stub.gate(workspace, GateName::IssueCount);
        assert_eq!(first.reason, Reason::Stub, "替身结论不得被当成真平面");
        assert!(first.is_enforcing());
        assert_eq!(first.gate.limit, Some(7));
        assert_eq!(first.policy_revision, 3);
        assert_eq!(first.subscription_version, 9);

        // 克隆语义：改一个拿回来的值不会污染替身内部。
        let mut mutated = stub.gate(workspace, GateName::IssueCount);
        mutated.gate.action = Action::Off;
        assert!(stub.gate(workspace, GateName::IssueCount).is_enforcing());

        // 另一个 gate 仍 fail-open。
        assert_eq!(
            stub.gate(workspace, GateName::IssueCount).gate.action,
            Action::Enforce
        );
        assert_eq!(
            stub.gate(workspace, GateName::AutopilotRuns).gate.action,
            Action::Off
        );

        stub.clear_workspace(workspace);
        assert_eq!(
            stub.gate(workspace, GateName::IssueCount).gate.action,
            Action::Off
        );
        // 调用记录**不**被清空（它记的是"问过什么"，不是状态）：1+1+1+1+1 次 + 上面清空后那一次。
        assert_eq!(stub.call_count(), 6);
        assert!(stub.is_enabled());
    }

    #[test]
    fn shared_stub_is_usable_as_a_dyn_provider() {
        let stub = Stub::shared();
        let provider: Arc<dyn Provider> = stub.clone();
        assert_eq!(
            provider.gate(Id::new(), GateName::IssueCount).reason,
            Reason::Stub
        );
        assert_eq!(stub.call_count(), 1);
    }
}
