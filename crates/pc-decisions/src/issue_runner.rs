//! `DecisionEffectRunner` 的 pc-issues 实现。
//!
//! 设计：
//! - 这个模块是 `pc-decisions` 与 `pc-issues` 的接缝点。
//! - 把 pc-issues 的 `IssueService` 调用结果转成 `(Value, String)`，
//!   让 effect_executor 完全不需要知道 pc-issues。
//! - 路由层组装：`
//!   let runner = IssueServiceRunner::new(&issue_service);
//!   svc.run_effects(id, user_id, &runner).await?;
//!   `

use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_issues::{AssignTarget, CommentAuthor, IssueService};

use crate::effect_executor::DecisionEffectRunner;

/// 把 `IssueService` 包装成 `DecisionEffectRunner`。
pub struct IssueServiceRunner<'a> {
    pub issues: &'a IssueService<'a>,
}

impl<'a> IssueServiceRunner<'a> {
    pub fn new(issues: &'a IssueService<'a>) -> Self {
        Self { issues }
    }
}

#[async_trait]
impl<'a> DecisionEffectRunner for IssueServiceRunner<'a> {
    async fn add_comment(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        body_md: &str,
        decided_by_user_id: &str,
    ) -> Result<String, String> {
        match self
            .issues
            .create_comment(
                company_id,
                issue_id,
                CommentAuthor::User(decided_by_user_id),
                body_md,
            )
            .await
        {
            Ok(row) => Ok(row.id.to_string()),
            Err(e) => Err(format!("add_comment failed: {e}")),
        }
    }

    async fn update_issue_status(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        new_status: &str,
    ) -> Result<Value, String> {
        match self
            .issues
            .update_status(company_id, issue_id, new_status)
            .await
        {
            Ok(row) => Ok(json!({
                "issueId": row.id,
                "status": row.status,
            })),
            Err(e) => Err(format!("update_status failed: {e}")),
        }
    }

    async fn assign_issue(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        assignee_agent_id: Option<Uuid>,
        assignee_user_id: Option<&str>,
    ) -> Result<Value, String> {
        let target = match (assignee_agent_id, assignee_user_id) {
            (Some(a), _) => AssignTarget::Agent(a),
            (None, Some(u)) => AssignTarget::User(u.to_string()),
            (None, None) => AssignTarget::Unassign,
        };
        match self.issues.assign(company_id, issue_id, target).await {
            Ok(row) => Ok(json!({
                "issueId": row.id,
                "assigneeAgentId": row.assignee_agent_id,
                "assigneeUserId": row.assignee_user_id,
            })),
            Err(e) => Err(format!("assign failed: {e}")),
        }
    }
}
