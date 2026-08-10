#![forbid(unsafe_code)]
//! `pc-hire-hook` —— hire-approved adapter hook。
//!
//! 对应 Node `server/src/services/hire-hook.ts`（113 行）。
//!
//! 设计目标：
//!
//! - 1:1 复刻 Node 行为：在 agent hire 审批通过（join_request / approval）时，
//!   异步调用 adapter 的 `onHireApproved` 钩子，并把结果写入 activity_log。
//! - **非致命**：adapter 抛错或返回 `ok=false` 时，本服务**不抛错**，仅 log + 记录失败。
//! - **注入式**：`HireApprovedHookRegistry` 通过 `Arc<dyn HireApprovedHook>` 注入，
//!   不直接依赖具体的 `pc-adapter-claude-local` / `pc-adapter-codex-local` 等。
//! - **可测**：trait object 注入，单测可注入 mock hook；e2e 注入 NoopHook 验证
//!   activity_log 写入。
//!
//! 公共 API：
//!
//! - [`NotifyHireApprovedInput`] —— 调用方传入的输入
//! - [`notify_hire_approved`] —— 顶层函数
//! - [`HireApprovedHookRegistry`] —— 钩子注册表

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use pc_adapter_api::{
    HireApprovedHook, HireApprovedPayload, HireApprovedResult, HireApprovedSource,
};
use pc_repos::agent::AgentRepo;
use pc_repos::Db;
use serde_json::{json, Value};
use uuid::Uuid;

/// 角色：hire 通知的来源。
///
/// 与 Node `source` 字面量 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyHireApprovedSource {
    JoinRequest,
    Approval,
}

impl NotifyHireApprovedSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::JoinRequest => "join_request",
            Self::Approval => "approval",
        }
    }
}

/// `notifyHireApproved` 输入。
#[derive(Debug, Clone)]
pub struct NotifyHireApprovedInput {
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub source: NotifyHireApprovedSource,
    pub source_id: String,
    pub approved_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// activity_log 写入器 trait —— 注入以解耦 pc-activity。
#[async_trait]
pub trait ActivitySink: Send + Sync {
    async fn log(
        &self,
        company_id: Uuid,
        actor_id: &str,
        action: &str,
        entity_id: &str,
        details: Value,
    ) -> Result<(), String>;
}

/// 默认 activity_log writer —— 直接写 DB。
pub struct DbActivitySink {
    db: Db,
}

impl DbActivitySink {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ActivitySink for DbActivitySink {
    async fn log(
        &self,
        company_id: Uuid,
        actor_id: &str,
        action: &str,
        entity_id: &str,
        details: Value,
    ) -> Result<(), String> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO activity_log \
                (company_id, actor_type, actor_id, action, entity_type, entity_id, details) \
             VALUES ($1, 'system', $2, $3, 'agent', $4, $5) \
             RETURNING id",
        )
        .bind(company_id)
        .bind(actor_id)
        .bind(action)
        .bind(entity_id)
        .bind(details)
        .fetch_one(self.db.pool())
        .await
        .map_err(|e| format!("activity_log insert: {e}"))?;
        let _ = row.0;
        Ok(())
    }
}

/// 默认常量 —— 与 Node `HIRE_APPROVED_MESSAGE` 1:1 对齐。
pub const HIRE_APPROVED_MESSAGE: &str = "Tell your user that your hire was approved, now they should assign you a task in Paperclip or ask you to create issues.";

/// adapter hook 注册表 —— 通过 `Arc<dyn HireApprovedHook>` 注入具体 adapter。
#[derive(Default, Clone)]
pub struct HireApprovedHookRegistry {
    /// adapter_type → hook 实例
    hooks: Arc<std::sync::RwLock<HashMap<String, Arc<dyn HireApprovedHook>>>>,
}

impl HireApprovedHookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &self,
        adapter_type: &str,
        hook: Arc<dyn HireApprovedHook>,
    ) -> Option<Arc<dyn HireApprovedHook>> {
        let mut hooks = self.hooks.write().expect("hire-hook registry poisoned");
        hooks.insert(adapter_type.to_string(), hook)
    }

    pub fn get(&self, adapter_type: &str) -> Option<Arc<dyn HireApprovedHook>> {
        let hooks = self.hooks.read().expect("hire-hook registry poisoned");
        hooks.get(adapter_type).cloned()
    }
}

/// 一个始终返回 `ok` 的 hook —— 测试用 / 缺省 fallback。
pub struct NoopHireApprovedHook;

#[async_trait]
impl HireApprovedHook for NoopHireApprovedHook {
    async fn on_hire_approved(
        &self,
        _payload: HireApprovedPayload,
        _adapter_config: Value,
    ) -> HireApprovedResult {
        HireApprovedResult::ok()
    }
}

/// 顶层函数 —— 与 Node `notifyHireApproved(db, input)` 1:1 对齐。
///
/// 失败时**绝不抛错** —— 仅 log + 写 activity_log。
pub async fn notify_hire_approved(
    db: &Db,
    activity: &dyn ActivitySink,
    hooks: &HireApprovedHookRegistry,
    input: NotifyHireApprovedInput,
) {
    let approved_at = input.approved_at.unwrap_or_else(chrono::Utc::now);
    let NotifyHireApprovedInput {
        company_id,
        agent_id,
        source,
        source_id,
        ..
    } = input;

    // 1. 查 agent 行
    let agent_row = match AgentRepo::new(db).get(agent_id).await {
        Ok(Some(row)) if row.company_id == company_id => row,
        Ok(Some(_)) => {
            tracing::warn!(
                company_id = %company_id,
                agent_id = %agent_id,
                source = source.as_str(),
                source_id = %source_id,
                "hire hook: agent found but company mismatch, skipping",
            );
            return;
        }
        Ok(None) => {
            tracing::warn!(
                company_id = %company_id,
                agent_id = %agent_id,
                source = source.as_str(),
                source_id = %source_id,
                "hire hook: agent not found in company, skipping",
            );
            return;
        }
        Err(e) => {
            tracing::error!(
                company_id = %company_id,
                agent_id = %agent_id,
                error = %e,
                "hire hook: agent lookup failed",
            );
            return;
        }
    };

    // 2. 查找 adapter hook
    let adapter_type = if agent_row.adapter_type.is_empty() {
        "process".to_string()
    } else {
        agent_row.adapter_type.clone()
    };
    let hook = match hooks.get(&adapter_type) {
        Some(h) => h,
        None => return, // 没注册 hook：静默跳过（与 Node 一致）
    };

    // 3. 构造 payload
    let payload = HireApprovedPayload {
        company_id: company_id.to_string(),
        agent_id: agent_id.to_string(),
        agent_name: agent_row.name.clone(),
        adapter_type: adapter_type.clone(),
        source: match source {
            NotifyHireApprovedSource::JoinRequest => HireApprovedSource::JoinRequest,
            NotifyHireApprovedSource::Approval => HireApprovedSource::Approval,
        },
        source_id: source_id.clone(),
        approved_at: approved_at.to_rfc3339(),
        message: HIRE_APPROVED_MESSAGE.to_string(),
    };

    // 4. adapter_config（兜底为 `{}`）
    let adapter_config = if agent_row.adapter_config.is_object() {
        agent_row.adapter_config.clone()
    } else {
        Value::Object(Default::default())
    };

    // 5. 调 hook + 写 activity_log
    let details = json!({
        "source": source.as_str(),
        "source_id": source_id,
        "adapter_type": adapter_type,
    });

    let result = hook.on_hire_approved(payload, adapter_config).await;
    if result.ok {
        if let Err(e) = activity
            .log(
                company_id,
                "hire_hook",
                "hire_hook.succeeded",
                &agent_id.to_string(),
                details,
            )
            .await
        {
            tracing::warn!(error = %e, "hire hook: activity log succeeded failed");
        }
        return;
    }

    // 失败：log + 写 activity_log（hire_hook.failed）
    tracing::warn!(
        company_id = %company_id,
        agent_id = %agent_id,
        adapter_type = %adapter_type,
        source = source.as_str(),
        source_id = %source_id,
        error = ?result.error,
        detail = ?result.detail,
        "hire hook: adapter returned failure",
    );
    let mut failed_details = details;
    if let Some(e) = result.error {
        failed_details["error"] = Value::String(e);
    }
    if let Some(x) = result.detail {
        failed_details["detail"] = Value::String(x);
    }
    let _ = activity
        .log(
            company_id,
            "hire_hook",
            "hire_hook.failed",
            &agent_id.to_string(),
            failed_details,
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_adapter_api::HireApprovedHook;
    use serde_json::json;

    struct RecordingHook {
        log: std::sync::Mutex<Vec<(HireApprovedPayload, Value)>>,
        result: HireApprovedResult,
    }

    impl RecordingHook {
        fn ok() -> Self {
            Self {
                log: std::sync::Mutex::new(Vec::new()),
                result: HireApprovedResult::ok(),
            }
        }
        fn failure(error: &str) -> Self {
            Self {
                log: std::sync::Mutex::new(Vec::new()),
                result: HireApprovedResult::failure(error, Some("detail-x".into())),
            }
        }
        fn calls(&self) -> Vec<(HireApprovedPayload, Value)> {
            self.log.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl HireApprovedHook for RecordingHook {
        async fn on_hire_approved(
            &self,
            payload: HireApprovedPayload,
            adapter_config: Value,
        ) -> HireApprovedResult {
            self.log.lock().unwrap().push((payload, adapter_config));
            self.result.clone()
        }
    }

    #[test]
    fn r688_source_string_matches_node() {
        assert_eq!(NotifyHireApprovedSource::JoinRequest.as_str(), "join_request");
        assert_eq!(NotifyHireApprovedSource::Approval.as_str(), "approval");
    }

    #[test]
    fn r688_message_constant_matches_node() {
        assert_eq!(
            HIRE_APPROVED_MESSAGE,
            "Tell your user that your hire was approved, now they should assign you a task in Paperclip or ask you to create issues.",
        );
    }

    #[test]
    fn r688_registry_register_and_get() {
        let reg = HireApprovedHookRegistry::new();
        let hook = Arc::new(NoopHireApprovedHook);
        reg.register("process", hook.clone());
        assert!(reg.get("process").is_some());
        assert!(reg.get("missing").is_none());
    }

    #[test]
    fn r688_registry_register_overwrites() {
        let reg = HireApprovedHookRegistry::new();
        let hook_a = Arc::new(NoopHireApprovedHook);
        let hook_b = Arc::new(NoopHireApprovedHook);
        assert!(reg.register("process", hook_a).is_none());
        // 重复注册返回旧值
        assert!(reg.register("process", hook_b).is_some());
    }

    #[test]
    fn r688_noop_hook_returns_ok() {
        let h = NoopHireApprovedHook;
        let r = futures::executor::block_on(h.on_hire_approved(
            HireApprovedPayload {
                company_id: "c".into(),
                agent_id: "a".into(),
                agent_name: "n".into(),
                adapter_type: "process".into(),
                source: HireApprovedSource::JoinRequest,
                source_id: "j".into(),
                approved_at: "2025-01-01T00:00:00Z".into(),
                message: HIRE_APPROVED_MESSAGE.into(),
            },
            json!({}),
        ));
        assert!(r.ok);
    }

    #[test]
    fn r688_recording_hook_captures_payload_and_config() {
        let h = RecordingHook::ok();
        let payload = HireApprovedPayload {
            company_id: "c1".into(),
            agent_id: "a1".into(),
            agent_name: "agent 1".into(),
            adapter_type: "process".into(),
            source: HireApprovedSource::Approval,
            source_id: "approval-42".into(),
            approved_at: "2025-02-02T00:00:00Z".into(),
            message: HIRE_APPROVED_MESSAGE.into(),
        };
        let config = json!({"claude": {"model": "opus"}});
        let payload_clone = payload.clone();
        let config_clone = config.clone();
        let r = futures::executor::block_on(h.on_hire_approved(payload, config));
        assert!(r.ok);
        let calls = h.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.company_id, payload_clone.company_id);
        assert_eq!(calls[0].1, config_clone);
    }

    #[test]
    fn r688_failure_result_carries_error() {
        let r = HireApprovedResult::failure("not_ready", Some("retry later".into()));
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("not_ready"));
        assert_eq!(r.detail.as_deref(), Some("retry later"));
    }

    #[test]
    fn r688_registry_default_is_empty() {
        let reg = HireApprovedHookRegistry::default();
        assert!(reg.get("process").is_none());
    }

    #[test]
    fn r688_payload_serializes_camel_case() {
        let payload = HireApprovedPayload {
            company_id: "c".into(),
            agent_id: "a".into(),
            agent_name: "n".into(),
            adapter_type: "process".into(),
            source: HireApprovedSource::JoinRequest,
            source_id: "j".into(),
            approved_at: "2025-01-01T00:00:00Z".into(),
            message: HIRE_APPROVED_MESSAGE.into(),
        };
        let s = serde_json::to_string(&payload).unwrap();
        assert!(s.contains("companyId"));
        assert!(s.contains("agentId"));
        assert!(s.contains("agentName"));
        assert!(s.contains("adapterType"));
        assert!(s.contains("sourceId"));
        assert!(s.contains("approvedAt"));
    }
}
