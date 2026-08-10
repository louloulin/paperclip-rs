//! 真实 DB 实现的 `HireAgentOperations`。
//!
//! 与 paperclip 上游 `services/approvals.ts` 中 hire_agent 副作用对齐：
//! - approve activate 模式：调 `AgentRepo::approve_pending`
//! - approve create 模式：调 `AgentRepo::create_full`
//! - reject：调 `AgentRepo::terminate`
//! - budget policy：调 `BudgetRepo::upsert_policy`
//!
//! 设计目标：让 `ApprovalService` + `HireAgentApprovalHook` 在真实 DB 上跑通。
//! 持有 `Arc<Db>` 让每个实例独立，并发安全。

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use pc_repos::agent::AgentRepo;
use pc_repos::budget::{BudgetRepo, UpsertPolicyInput};

use crate::hire_hook::{HireAgentApprovalPayload, HireAgentOperations};

/// 真实 DB 实现的 hire_agent operations。
///
/// 持有 `Arc<pc_repos::Db>`，每次方法调用时构造 `AgentRepo` / `BudgetRepo`
/// 并借用 db。`Arc` 让 trait object 多线程安全（`Db` 内部就是共享 `PgPool`）。
pub struct DbHireAgentOps {
    db: Arc<pc_repos::Db>,
}

impl DbHireAgentOps {
    /// 从 owned `Db` 构造（最常用）。
    #[must_use]
    pub fn new(db: pc_repos::Db) -> Self {
        Self { db: Arc::new(db) }
    }

    /// 从 `Arc<Db>` 构造（共享 DB 时省一次 Arc clone）。
    #[must_use]
    pub fn from_arc(db: Arc<pc_repos::Db>) -> Self {
        Self { db }
    }

    /// 借用内部 db（用于测试断言 / 集成）。
    #[must_use]
    pub fn db(&self) -> &pc_repos::Db {
        &self.db
    }
}

#[async_trait]
impl HireAgentOperations for DbHireAgentOps {
    async fn activate_agent(
        &self,
        company_id: &str,
        agent_id: &str,
        _payload: &HireAgentApprovalPayload,
    ) -> Result<(), String> {
        let agent_uuid = Uuid::parse_str(agent_id)
            .map_err(|e| format!("invalid agent_id uuid: {e}"))?;
        let _ = company_id; // approve_pending 不需要 company_id 过滤
        let db = (*self.db).clone();
        let repo = AgentRepo::new(&db);
        let row = repo
            .approve_pending(agent_uuid)
            .await
            .map_err(|e| format!("approve_pending: {e}"))?;
        if row.is_none() {
            return Err(format!("agent {agent_id} not in pending_approval state"));
        }
        Ok(())
    }

    async fn create_agent(
        &self,
        company_id: &str,
        payload: &HireAgentApprovalPayload,
    ) -> Result<String, String> {
        let company_uuid = Uuid::parse_str(company_id)
            .map_err(|e| format!("invalid company_id uuid: {e}"))?;
        let db = (*self.db).clone();
        let input = pc_repos::agent::CreateAgentRecord {
            id: Uuid::new_v4(),
            company_id: company_uuid,
            name: payload.name.clone().unwrap_or_else(|| "New Agent".to_string()),
            role: payload.role.clone().unwrap_or_else(|| "general".to_string()),
            title: payload.title.as_deref().map(str::to_string),
            icon: None,
            reports_to: payload
                .reports_to
                .as_deref()
                .and_then(|s| Uuid::parse_str(s).ok()),
            capabilities: payload.capabilities.clone(),
            adapter_type: payload
                .adapter_type
                .clone()
                .unwrap_or_else(|| "process".to_string()),
            adapter_config: payload
                .adapter_config
                .clone()
                .unwrap_or_else(|| serde_json::json!({})),
            runtime_config: serde_json::json!({}),
            default_environment_id: None,
            budget_monthly_cents: payload.budget_monthly_cents.unwrap_or(0) as i32,
            permissions: serde_json::json!({}),
            metadata: payload.metadata.clone(),
            status: "idle".into(),
        };
        let repo = AgentRepo::new(&db);
        let row = repo
            .create_full(input)
            .await
            .map_err(|e| format!("create_full: {e}"))?;
        Ok(row.id.to_string())
    }

    async fn upsert_budget_policy(
        &self,
        company_id: &str,
        scope_type: &str,
        scope_id: &str,
        amount_cents: i64,
    ) -> Result<(), String> {
        let company_uuid = Uuid::parse_str(company_id)
            .map_err(|e| format!("invalid company_id uuid: {e}"))?;
        let scope_uuid = Uuid::parse_str(scope_id)
            .map_err(|e| format!("invalid scope_id uuid: {e}"))?;
        let db = (*self.db).clone();
        let input = UpsertPolicyInput {
            scope_type: scope_type.into(),
            scope_id: scope_uuid,
            metric: "billed_cents".into(),
            window_kind: "calendar_month_utc".into(),
            amount: amount_cents as i32,
            warn_percent: 80,
            hard_stop_enabled: true,
            notify_enabled: true,
            is_active: true,
            updated_by_user_id: None,
        };
        BudgetRepo::new(&db)
            .upsert_policy(company_uuid, &input)
            .await
            .map_err(|e| format!("upsert_policy: {e}"))?;
        Ok(())
    }

    async fn terminate_agent(&self, agent_id: &str) -> Result<(), String> {
        let agent_uuid = Uuid::parse_str(agent_id)
            .map_err(|e| format!("invalid agent_id uuid: {e}"))?;
        let db = (*self.db).clone();
        AgentRepo::new(&db)
            .terminate(agent_uuid)
            .await
            .map_err(|e| format!("terminate: {e}"))?;
        Ok(())
    }

    async fn notify_hire_approved(
        &self,
        _company_id: &str,
        _agent_id: &str,
        _source_id: &str,
    ) -> Result<(), String> {
        // TODO: 接入 realtime bus 或 adapter onHireApproved hook。
        // 当前为 no-op：与上游 `non-fatal` 语义一致 — 失败不抛错，silent log。
        Ok(())
    }

    async fn reconcile_builtin_agent(
        &self,
        _company_id: &str,
        _builtin_key: &str,
    ) -> Result<(), String> {
        // TODO: 接入 built-in agent reconciliation service。
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn r583_db_ops_constructor_trait_object_safe() {
        // 用 lazy pool（不会真连 DB）。
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/db")
            .expect("lazy pool");
        let db = pc_repos::Db::from_pool(pool);
        let ops = DbHireAgentOps::new(db);
        let _: Arc<dyn HireAgentOperations> = Arc::new(ops);
    }
}
