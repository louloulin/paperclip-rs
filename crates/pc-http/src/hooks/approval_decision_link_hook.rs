//! `ApprovalDecisionLinkHook` — R601。
//!
//! 监听 ApprovalService 的 `on_approved` / `on_rejected` / `on_cancelled`，
//! 当 `payload["decision_id"]` 存在时，联动修改对应 decision 的状态：
//!
//! - `on_approved`  → `decision.status = "decided"`, `chosen_option_id = "approved"`
//! - `on_rejected`  → `decision.status = "dismissed"`, `metadata.dismissReason = "approval_rejected"`
//! - `on_cancelled` → `decision.status = "cancelled"`
//!
//! 设计目标：
//! - 闭合 service 联动网络最后一环：approval → decision
//! - 高内聚：单一职责（approval 与 decision 双向联动）
//! - 低耦合：直接接受 `Arc<Db>`；不依赖 DecisionService（避免 signing 复杂依赖）
//! - 失败容忍：DB 写失败仅 trace warn，不影响 approval 主流程
//!
//! 注意：直接走 `DecisionRepo::mark_*`，不走 `DecisionService::decide`。
//! 后者会触发签名校验 — 我们要的是联动落库，不是可信决策流。

use async_trait::async_trait;
use pc_approvals::ApprovalHook;
use pc_repos::approval::ApprovalRow;
use pc_repos::decision::DecisionRepo;
use std::sync::Arc;
use uuid::Uuid;

/// 默认 payload key。
const DEFAULT_DECISION_ID_KEY: &str = "decision_id";
/// approved 决策的 chosen_option_id。
const APPROVED_CHOSEN: &str = "approved";

#[derive(Clone)]
pub struct ApprovalDecisionLinkHook {
    /// 共享 Db（Arc 便于跨线程使用）。
    db: Arc<pc_repos::Db>,
    /// payload 中读取 decision_id 的字段名（可配置；默认 "decision_id"）。
    decision_id_key: String,
}

impl std::fmt::Debug for ApprovalDecisionLinkHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalDecisionLinkHook")
            .field("decision_id_key", &self.decision_id_key)
            .finish()
    }
}

impl ApprovalDecisionLinkHook {
    #[must_use]
    pub fn new(db: Arc<pc_repos::Db>) -> Self {
        Self {
            db,
            decision_id_key: DEFAULT_DECISION_ID_KEY.to_string(),
        }
    }

    #[must_use]
    pub fn with_decision_id_key(db: Arc<pc_repos::Db>, key: impl Into<String>) -> Self {
        Self {
            db,
            decision_id_key: key.into(),
        }
    }

    fn parse_decision_id(&self, approval: &ApprovalRow) -> Option<Uuid> {
        let raw = approval.payload.get(&self.decision_id_key)?;
        let s = raw.as_str()?;
        Uuid::parse_str(s).ok()
    }
}

#[async_trait]
impl ApprovalHook for ApprovalDecisionLinkHook {
    async fn on_approved(
        &self,
        approval: &ApprovalRow,
    ) -> pc_approvals::ApprovalHookOutcome {
        let Some(decision_id) = self.parse_decision_id(approval) else {
            return pc_approvals::ApprovalHookOutcome::Skipped;
        };

        let repo = DecisionRepo::new(self.db.as_ref());
        let decided_by = approval.decided_by_user_id.as_deref();
        match repo
            .mark_decided(decision_id, APPROVED_CHOSEN, decided_by, None)
            .await
        {
            Ok(true) => {
                tracing::info!(
                    approval_id = %approval.id,
                    decision_id = %decision_id,
                    "approval approved -> decision decided"
                );
                pc_approvals::ApprovalHookOutcome::Ok
            }
            Ok(false) => {
                tracing::warn!(
                    approval_id = %approval.id,
                    decision_id = %decision_id,
                    "approval approved but decision not found (no rows updated)"
                );
                pc_approvals::ApprovalHookOutcome::Skipped
            }
            Err(e) => {
                tracing::warn!(
                    approval_id = %approval.id,
                    decision_id = %decision_id,
                    error = %e,
                    "failed to mark decision as decided"
                );
                pc_approvals::ApprovalHookOutcome::Failed(e.to_string())
            }
        }
    }

    async fn on_rejected(
        &self,
        approval: &ApprovalRow,
    ) -> pc_approvals::ApprovalHookOutcome {
        let Some(decision_id) = self.parse_decision_id(approval) else {
            return pc_approvals::ApprovalHookOutcome::Skipped;
        };

        let repo = DecisionRepo::new(self.db.as_ref());
        let decided_by = approval.decided_by_user_id.as_deref().unwrap_or("unknown");
        match repo
            .mark_dismissed(decision_id, "approval_rejected", decided_by)
            .await
        {
            Ok(true) => pc_approvals::ApprovalHookOutcome::Ok,
            Ok(false) => pc_approvals::ApprovalHookOutcome::Skipped,
            Err(e) => {
                tracing::warn!(
                    approval_id = %approval.id,
                    decision_id = %decision_id,
                    error = %e,
                    "failed to mark decision as dismissed"
                );
                pc_approvals::ApprovalHookOutcome::Failed(e.to_string())
            }
        }
    }

    async fn on_cancelled(
        &self,
        approval: &ApprovalRow,
    ) -> pc_approvals::ApprovalHookOutcome {
        let Some(decision_id) = self.parse_decision_id(approval) else {
            return pc_approvals::ApprovalHookOutcome::Skipped;
        };

        let repo = DecisionRepo::new(self.db.as_ref());
        match repo.mark_cancelled(decision_id).await {
            Ok(true) => pc_approvals::ApprovalHookOutcome::Ok,
            Ok(false) => pc_approvals::ApprovalHookOutcome::Skipped,
            Err(e) => {
                tracing::warn!(
                    approval_id = %approval.id,
                    decision_id = %decision_id,
                    error = %e,
                    "failed to mark decision as cancelled"
                );
                pc_approvals::ApprovalHookOutcome::Failed(e.to_string())
            }
        }
    }
}
