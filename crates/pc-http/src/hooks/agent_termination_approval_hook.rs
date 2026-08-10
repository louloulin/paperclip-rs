//! `AgentTerminationApprovalHook` — R595。
//!
//! 监听 AgentService 的 `Terminated` 事件，当 agent.role 属于高风险集合
//! （默认 `ceo` / `admin` / `owner`）时，通过 ApprovalService::create 创建
//! 对应的 approval request（类型 `AgentAction`，payload 携带 agent 信息）。
//!
//! 设计目标：
//! - 高内聚：单一职责（高风险 agent 终止 → approval request）
//! - 低耦合：不直接依赖 AgentService / CompanyService；只接受 hook trait + ApprovalService
//! - 失败容忍：ApprovalService 失败仅记 warn，不影响主流程（terminate 已成功）

use async_trait::async_trait;
use pc_agent::AgentLifecycleEvent;
use pc_approvals::ApprovalService;
use pc_repos::approval::{ApprovalType, NewApproval};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

/// 默认高风险 agent role — 终止这些角色需要 board approval。
const DEFAULT_HIGH_RISK_ROLES: &[&str] = &["ceo", "admin", "owner"];

#[derive(Clone)]
pub struct AgentTerminationApprovalHook {
    /// ApprovalService 实例 — lifetime 由调用方保证（实践用 Box::leak）。
    approval_service: Arc<ApprovalService<'static>>,
    /// 高风险 role 列表（可配置）。
    high_risk_roles: Vec<String>,
}

impl std::fmt::Debug for AgentTerminationApprovalHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTerminationApprovalHook")
            .field("high_risk_roles", &self.high_risk_roles)
            .finish()
    }
}

impl AgentTerminationApprovalHook {
    #[must_use]
    pub fn new(approval_service: Arc<ApprovalService<'static>>) -> Self {
        Self {
            approval_service,
            high_risk_roles: DEFAULT_HIGH_RISK_ROLES
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }

    /// 用自定义高风险 role 列表构造 hook。
    #[must_use]
    pub fn with_high_risk_roles(
        approval_service: Arc<ApprovalService<'static>>,
        roles: Vec<String>,
    ) -> Self {
        Self {
            approval_service,
            high_risk_roles: roles,
        }
    }

    fn is_high_risk(&self, role: &str) -> bool {
        self.high_risk_roles.iter().any(|r| r == role)
    }
}

#[async_trait]
impl pc_agent::AgentHook for AgentTerminationApprovalHook {
    async fn on_lifecycle(&self, event: AgentLifecycleEvent) -> pc_errors::Result<()> {
        let AgentLifecycleEvent::Terminated {
            id: agent_id,
            company_id,
            role,
        } = event
        else {
            return Ok(());
        };

        if !self.is_high_risk(&role) {
            return Ok(());
        }

        let approval = NewApproval {
            company_id,
            approval_type: ApprovalType::AgentAction,
            requested_by_agent_id: Some(agent_id),
            requested_by_user_id: None,
            payload: json!({
                "action": "agent_termination",
                "agent_id": agent_id.to_string(),
                "role": role,
                "reason": "high_risk_role_termination",
            }),
        };

        match self.approval_service.create(&approval).await {
            Ok(_) => {
                tracing::info!(
                    agent_id = %agent_id,
                    company_id = %company_id,
                    role = %role,
                    "high-risk agent termination triggered approval request"
                );
            }
            Err(e) => {
                tracing::warn!(
                    agent_id = %agent_id,
                    company_id = %company_id,
                    error = %e,
                    "failed to create approval for high-risk termination"
                );
            }
        }
        Ok(())
    }
}

// 防止未使用导入警告
const _: Option<Uuid> = None;
