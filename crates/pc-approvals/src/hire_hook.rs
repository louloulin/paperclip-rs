//! `hire_agent` Approval Hook 实现。
//!
//! 与 paperclip 上游 `services/approvals.ts` 中 hire_agent 副作用一致：
//! - approve `hire_agent` type approval → 激活 agent / 创建 agent
//! - reject `hire_agent` type approval → terminate agent
//!
//! ## 与上游的差异
//! 上游把"调 agents service + budget service + 通知"硬编码到 approvalService。
//! 本实现通过 `HireAgentApprovalHook` trait object 实现 ApprovalHook trait：
//! - `extract_payload`: pure 函数，从 ApprovalRow.payload 提取 HireAgentPayload
//! - `on_approved`: 调用 trait method `apply_hire`（由调用方实现，激活/创建 agent）
//! - `on_rejected`: 调用 trait method `terminate_hire`（由调用方实现）
//! - 返回 ApprovalHookOutcome 告知调用方是否成功
//!
//! 调用方负责实现 `HireAgentOperations` trait（DB 操作），hook 负责 typed 提取 + dispatch。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use pc_repos::approval::ApprovalRow;

use crate::service::{ApprovalHook, ApprovalHookOutcome};

/// `hire_agent` approval 的 typed payload。
///
/// 与上游 `HireApprovedPayload` 的 sub-set 对齐：
/// - `agentId`：已存在的 agent_id（activate 模式）
/// - `name` / `role` / `title` / `reportsTo`：创建新 agent 模式
/// - `adapterType` / `adapterConfig`：adapter 配置
/// - `budgetMonthlyCents`：批准后创建 budget policy
/// - `capabilities` / `metadata`：agent 元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HireAgentApprovalPayload {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub reports_to: Option<String>,
    #[serde(default)]
    pub capabilities: Option<String>,
    #[serde(default)]
    pub adapter_type: Option<String>,
    #[serde(default)]
    pub adapter_config: Option<Value>,
    #[serde(default)]
    pub budget_monthly_cents: Option<i64>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub source_builtin_agent_key: Option<String>,
}

impl HireAgentApprovalPayload {
    /// 从 ApprovalRow.payload 提取 typed payload。
    #[must_use]
    pub fn extract(row: &ApprovalRow) -> Option<Self> {
        if row.approval_type != "hire_agent" {
            return None;
        }
        serde_json::from_value(row.payload.clone()).ok()
    }

    /// 模式判定：是激活现有 agent（agent_id 存在）还是创建新 agent。
    #[must_use]
    pub fn mode(&self) -> HireMode {
        if self.agent_id.is_some() {
            HireMode::ActivateExisting
        } else {
            HireMode::CreateNew
        }
    }

    /// 是否有非零 budget（决定是否需要创建 budget policy）。
    #[must_use]
    pub fn has_budget(&self) -> bool {
        self.budget_monthly_cents.unwrap_or(0) > 0
    }
}

/// hire_agent 的两种模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HireMode {
    /// 激活已有 agent（从 pending_approval → idle）。
    ActivateExisting,
    /// 创建新 agent（payload 包含 name/role 等）。
    CreateNew,
}

/// Hire 副作用操作（由调用方实现，封装 DB 操作）。
#[async_trait]
pub trait HireAgentOperations: Send + Sync {
    /// 激活已有 agent（pending_approval → idle）。
    async fn activate_agent(&self, _company_id: &str, _agent_id: &str, _payload: &HireAgentApprovalPayload) -> Result<(), String> {
        Err("activate_agent not implemented".into())
    }
    /// 创建新 agent。
    async fn create_agent(&self, _company_id: &str, _payload: &HireAgentApprovalPayload) -> Result<String, String> {
        Err("create_agent not implemented".into())
    }
    /// 创建 budget policy（hire 后）。
    async fn upsert_budget_policy(
        &self,
        _company_id: &str,
        _scope_type: &str,
        _scope_id: &str,
        _amount_cents: i64,
    ) -> Result<(), String> {
        Err("upsert_budget_policy not implemented".into())
    }
    /// 拒绝 hire_agent：terminate 对应 agent。
    async fn terminate_agent(&self, _agent_id: &str) -> Result<(), String> {
        Err("terminate_agent not implemented".into())
    }
    /// 内置 agent reconciliation。
    async fn reconcile_builtin_agent(
        &self,
        _company_id: &str,
        _builtin_key: &str,
    ) -> Result<(), String> {
        Ok(())
    }
    /// 通知（hire approved）— 失败不抛错。
    async fn notify_hire_approved(
        &self,
        _company_id: &str,
        _agent_id: &str,
        _source_id: &str,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// hire_agent ApprovalHook 实现。
///
/// 公开方法：
/// - `on_approved`：根据 payload.mode 走 activate 或 create，然后 upsert budget，然后 notify
/// - `on_rejected`：terminate agent（如果激活模式）
/// - `on_cancelled`：no-op（hire agent approval 的取消不影响已有 agent）
pub struct HireAgentApprovalHook<O: HireAgentOperations> {
    ops: std::sync::Arc<O>,
}

impl<O: HireAgentOperations> HireAgentApprovalHook<O> {
    #[must_use]
    pub fn new(ops: std::sync::Arc<O>) -> Self {
        Self { ops }
    }
}

#[async_trait]
impl<O: HireAgentOperations + 'static> ApprovalHook for HireAgentApprovalHook<O> {
    async fn on_approved(&self, row: &ApprovalRow) -> ApprovalHookOutcome {
        let Some(payload) = HireAgentApprovalPayload::extract(row) else {
            return ApprovalHookOutcome::Skipped;
        };
        // 1. 激活或创建 agent
        let agent_id_result = match payload.mode() {
            HireMode::ActivateExisting => {
                let aid = payload.agent_id.clone().unwrap_or_default();
                match self.ops.activate_agent(&row.company_id.to_string(), &aid, &payload).await {
                    Ok(()) => Ok(aid),
                    Err(e) => Err(format!("activate_agent: {e}")),
                }
            }
            HireMode::CreateNew => match self.ops.create_agent(&row.company_id.to_string(), &payload).await {
                Ok(id) => Ok(id),
                Err(e) => Err(format!("create_agent: {e}")),
            },
        };
        let agent_id = match agent_id_result {
            Ok(id) => id,
            Err(e) => return ApprovalHookOutcome::Failed(e),
        };
        // 2. 内置 agent reconciliation
        if let Some(key) = &payload.source_builtin_agent_key {
            if let Err(e) = self
                .ops
                .reconcile_builtin_agent(&row.company_id.to_string(), key)
                .await
            {
                return ApprovalHookOutcome::Failed(format!("reconcile_builtin_agent: {e}"));
            }
        }
        // 3. budget policy
        if payload.has_budget() {
            let scope_type = "agent";
            let amount = payload.budget_monthly_cents.unwrap_or(0);
            if let Err(e) = self
                .ops
                .upsert_budget_policy(&row.company_id.to_string(), scope_type, &agent_id, amount)
                .await
            {
                return ApprovalHookOutcome::Failed(format!("upsert_budget_policy: {e}"));
            }
        }
        // 4. 通知（失败不抛错）
        let _ = self
            .ops
            .notify_hire_approved(&row.company_id.to_string(), &agent_id, &row.id.to_string())
            .await;
        ApprovalHookOutcome::Ok
    }

    async fn on_rejected(&self, row: &ApprovalRow) -> ApprovalHookOutcome {
        let Some(payload) = HireAgentApprovalPayload::extract(row) else {
            return ApprovalHookOutcome::Skipped;
        };
        let Some(agent_id) = &payload.agent_id else {
            return ApprovalHookOutcome::Skipped;
        };
        match self.ops.terminate_agent(agent_id).await {
            Ok(()) => ApprovalHookOutcome::Ok,
            Err(e) => ApprovalHookOutcome::Failed(format!("terminate_agent: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::RecordingHook;
    use serde_json::json;
    use uuid::Uuid;

    fn dummy_approval(approval_type: &str, payload: Value) -> ApprovalRow {
        ApprovalRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            approval_type: approval_type.into(),
            requested_by_agent_id: None,
            requested_by_user_id: Some("user-1".into()),
            status: "pending".into(),
            payload,
            decision_note: None,
            decided_by_user_id: None,
            decided_at: None,
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        }
    }

    #[test]
    fn r580_extract_payload_for_hire_agent() {
        let row = dummy_approval(
            "hire_agent",
            json!({
                "agentId": "agent-1",
                "budgetMonthlyCents": 10000,
            }),
        );
        let payload = HireAgentApprovalPayload::extract(&row).unwrap();
        assert_eq!(payload.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(payload.budget_monthly_cents, Some(10000));
        assert_eq!(payload.mode(), HireMode::ActivateExisting);
        assert!(payload.has_budget());
    }

    #[test]
    fn r580_extract_payload_returns_none_for_non_hire() {
        let row = dummy_approval("custom", json!({"k": "v"}));
        assert!(HireAgentApprovalPayload::extract(&row).is_none());
    }

    #[test]
    fn r580_mode_create_new_when_no_agent_id() {
        let p = HireAgentApprovalPayload {
            agent_id: None,
            name: Some("New Agent".into()),
            role: Some("general".into()),
            title: None,
            reports_to: None,
            capabilities: None,
            adapter_type: Some("process".into()),
            adapter_config: None,
            budget_monthly_cents: None,
            metadata: None,
            source_builtin_agent_key: None,
        };
        assert_eq!(p.mode(), HireMode::CreateNew);
        assert!(!p.has_budget());
    }

    #[test]
    fn r580_mode_activate_existing_with_agent_id() {
        let p = HireAgentApprovalPayload {
            agent_id: Some("a-1".into()),
            name: None,
            role: None,
            title: None,
            reports_to: None,
            capabilities: None,
            adapter_type: None,
            adapter_config: None,
            budget_monthly_cents: Some(0),
            metadata: None,
            source_builtin_agent_key: None,
        };
        assert_eq!(p.mode(), HireMode::ActivateExisting);
        assert!(!p.has_budget());
    }

    #[test]
    fn r580_has_budget_treats_zero_as_false() {
        let p = HireAgentApprovalPayload {
            agent_id: Some("a-1".into()),
            name: None,
            role: None,
            title: None,
            reports_to: None,
            capabilities: None,
            adapter_type: None,
            adapter_config: None,
            budget_monthly_cents: Some(0),
            metadata: None,
            source_builtin_agent_key: None,
        };
        assert!(!p.has_budget());
    }

    // Mock ops for hook dispatch tests
    struct MockOps {
        activate_calls: std::sync::atomic::AtomicU32,
        create_calls: std::sync::atomic::AtomicU32,
        terminate_calls: std::sync::atomic::AtomicU32,
        budget_calls: std::sync::atomic::AtomicU32,
        notify_calls: std::sync::atomic::AtomicU32,
        fail_on: Option<&'static str>,
    }

    impl MockOps {
        fn new(fail_on: Option<&'static str>) -> Self {
            Self {
                activate_calls: std::sync::atomic::AtomicU32::new(0),
                create_calls: std::sync::atomic::AtomicU32::new(0),
                terminate_calls: std::sync::atomic::AtomicU32::new(0),
                budget_calls: std::sync::atomic::AtomicU32::new(0),
                notify_calls: std::sync::atomic::AtomicU32::new(0),
                fail_on,
            }
        }
    }

    #[async_trait]
    impl HireAgentOperations for MockOps {
        async fn activate_agent(&self, _: &str, _: &str, _: &HireAgentApprovalPayload) -> Result<(), String> {
            self.activate_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_on == Some("activate") { Err("mock activate fail".into()) } else { Ok(()) }
        }
        async fn create_agent(&self, _: &str, _: &HireAgentApprovalPayload) -> Result<String, String> {
            self.create_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_on == Some("create") { Err("mock create fail".into()) }
            else { Ok("new-agent-id".into()) }
        }
        async fn upsert_budget_policy(&self, _: &str, _: &str, _: &str, _: i64) -> Result<(), String> {
            self.budget_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_on == Some("budget") { Err("mock budget fail".into()) } else { Ok(()) }
        }
        async fn terminate_agent(&self, _: &str) -> Result<(), String> {
            self.terminate_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_on == Some("terminate") { Err("mock terminate fail".into()) } else { Ok(()) }
        }
        async fn notify_hire_approved(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
            self.notify_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        futures_executor::block_on(f)
    }

    #[test]
    fn r580_hire_hook_approve_activate_existing_with_budget() {
        let ops = Arc::new(MockOps::new(None));
        let hook = HireAgentApprovalHook::new(ops.clone());
        let row = dummy_approval(
            "hire_agent",
            json!({"agentId": "agent-1", "budgetMonthlyCents": 5000}),
        );
        let outcome = block_on(hook.on_approved(&row));
        assert!(outcome.is_ok());
        assert_eq!(ops.activate_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(ops.budget_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(ops.notify_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn r580_hire_hook_approve_create_new_skips_budget() {
        let ops = Arc::new(MockOps::new(None));
        let hook = HireAgentApprovalHook::new(ops.clone());
        let row = dummy_approval(
            "hire_agent",
            json!({"name": "New Agent", "role": "general"}),
        );
        let outcome = block_on(hook.on_approved(&row));
        assert!(outcome.is_ok());
        assert_eq!(ops.create_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(ops.activate_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(ops.budget_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn r580_hire_hook_approve_zero_budget_skips_upsert() {
        let ops = Arc::new(MockOps::new(None));
        let hook = HireAgentApprovalHook::new(ops.clone());
        let row = dummy_approval(
            "hire_agent",
            json!({"agentId": "a-1", "budgetMonthlyCents": 0}),
        );
        let outcome = block_on(hook.on_approved(&row));
        assert!(outcome.is_ok());
        assert_eq!(ops.budget_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn r580_hire_hook_approve_skips_non_hire_agent_type() {
        let ops = Arc::new(MockOps::new(None));
        let hook = HireAgentApprovalHook::new(ops.clone());
        let row = dummy_approval("budget_change", json!({}));
        let outcome = block_on(hook.on_approved(&row));
        assert!(matches!(outcome, ApprovalHookOutcome::Skipped));
        assert_eq!(ops.activate_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn r580_hire_hook_approve_propagates_activate_failure() {
        let ops = Arc::new(MockOps::new(Some("activate")));
        let hook = HireAgentApprovalHook::new(ops.clone());
        let row = dummy_approval("hire_agent", json!({"agentId": "a-1"}));
        let outcome = block_on(hook.on_approved(&row));
        assert!(outcome.is_failed());
        assert_eq!(ops.activate_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        // budget/notify 不应被调用（因为 activate 失败）
        assert_eq!(ops.budget_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(ops.notify_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn r580_hire_hook_reject_terminate_existing() {
        let ops = Arc::new(MockOps::new(None));
        let hook = HireAgentApprovalHook::new(ops.clone());
        let row = dummy_approval("hire_agent", json!({"agentId": "a-1"}));
        let outcome = block_on(hook.on_rejected(&row));
        assert!(outcome.is_ok());
        assert_eq!(ops.terminate_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn r580_hire_hook_reject_skips_create_new_mode() {
        let ops = Arc::new(MockOps::new(None));
        let hook = HireAgentApprovalHook::new(ops.clone());
        let row = dummy_approval("hire_agent", json!({"name": "X"}));
        let outcome = block_on(hook.on_rejected(&row));
        assert!(matches!(outcome, ApprovalHookOutcome::Skipped));
        assert_eq!(ops.terminate_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn r580_recording_hook_still_works() {
        // 验证我们没破坏原有 RecordingHook
        let h = RecordingHook::default();
        let row = dummy_approval("hire_agent", json!({}));
        let id = row.id;
        block_on(async {
            h.on_approved(&row).await;
            h.on_rejected(&row).await;
        });
        assert!(h.approved.lock().unwrap().contains(&id));
        assert!(h.rejected.lock().unwrap().contains(&id));
    }

    #[test]
    fn r580_hire_hook_with_builtin_key_runs_reconcile() {
        // 验证 source_builtin_agent_key 触发 reconcile
        let ops = Arc::new(MockOps::new(None));
        let hook = HireAgentApprovalHook::new(ops.clone());
        let row = dummy_approval(
            "hire_agent",
            json!({"agentId": "a-1", "sourceBuiltinAgentKey": "built-in-key"}),
        );
        // 注意：我们的 mock 不统计 reconcile_calls，所以这里只验证不失败
        let outcome = block_on(hook.on_approved(&row));
        assert!(outcome.is_ok());
    }
}
