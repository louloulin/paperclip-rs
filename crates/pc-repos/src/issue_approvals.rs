//! Issue ↔ Approval 关联。
//!
//! 对齐 `paperclip/server/src/services/issue-approvals.ts`：
//! - `list_approvals_for_issue(issue_id)`：返回 issue 关联的所有 approval，
//!   payload 经 `crate::redact::sanitize_record` 遮罩
//! - `list_issues_for_approval(approval_id)`：返回 approval 关联的所有 issue
//! - `link(issue_id, approval_id, actor?)`：单条 upsert（同主键 → no-op）
//! - `unlink(issue_id, approval_id)`：删除关联
//! - `link_many_for_approval(approval_id, issue_ids, actor?)`：批量 link，
//!   要求所有 issue 属于同一 company
//!
//! 所有写操作都要求 `issue` 与 `approval` 属于同一 `company`，否则
//! 抛 `IssueApprovalError::CrossCompany`（对应 Node 端 `unprocessable`）。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use crate::redact::sanitize_record;
use crate::{Db, RepoError, RepoResult};

const LINK_COLS: &str = "company_id, issue_id, approval_id, linked_by_agent_id, \
     linked_by_user_id, created_at";

/// Issue ↔ Approval 关联行（`issue_approvals` 表）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueApprovalLinkRow {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub approval_id: Uuid,
    pub linked_by_agent_id: Option<Uuid>,
    pub linked_by_user_id: Option<String>,
    pub created_at: pc_core::Timestamp,
}

#[derive(Debug, Clone, Default)]
pub struct LinkActor {
    pub agent_id: Option<Uuid>,
    pub user_id: Option<String>,
}

/// 关联查询（与 approvals join）+ payload 已 redact。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalForIssueItem {
    pub id: Uuid,
    pub company_id: Uuid,
    #[serde(rename = "type")]
    pub approval_type: String,
    pub requested_by_agent_id: Option<Uuid>,
    pub requested_by_user_id: Option<String>,
    pub status: String,
    pub payload: Value,
    pub decision_note: Option<String>,
    pub decided_by_user_id: Option<String>,
    pub decided_at: Option<pc_core::Timestamp>,
    pub created_at: pc_core::Timestamp,
    pub updated_at: pc_core::Timestamp,
}

/// 关联查询（与 issues join），用于「approval 反查 issue 列表」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueForApprovalItem {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub goal_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: String,
    pub priority: Option<String>,
    pub assignee_agent_id: Option<Uuid>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub issue_number: Option<i32>,
    pub identifier: Option<String>,
    pub request_depth: Option<i32>,
    pub billing_code: Option<String>,
    pub started_at: Option<pc_core::Timestamp>,
    pub completed_at: Option<pc_core::Timestamp>,
    pub cancelled_at: Option<pc_core::Timestamp>,
    pub created_at: pc_core::Timestamp,
    pub updated_at: pc_core::Timestamp,
}

#[derive(Debug, FromRow)]
struct IssueForApprovalDbRow {
    id: Uuid,
    company_id: Uuid,
    project_id: Option<Uuid>,
    goal_id: Option<Uuid>,
    parent_id: Option<Uuid>,
    title: Option<String>,
    description: Option<String>,
    status: String,
    priority: Option<String>,
    assignee_agent_id: Option<Uuid>,
    created_by_agent_id: Option<Uuid>,
    created_by_user_id: Option<String>,
    issue_number: Option<i32>,
    identifier: Option<String>,
    request_depth: Option<i32>,
    billing_code: Option<String>,
    started_at: Option<pc_core::Timestamp>,
    completed_at: Option<pc_core::Timestamp>,
    cancelled_at: Option<pc_core::Timestamp>,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

impl From<IssueForApprovalDbRow> for IssueForApprovalItem {
    fn from(row: IssueForApprovalDbRow) -> Self {
        Self {
            id: row.id,
            company_id: row.company_id,
            project_id: row.project_id,
            goal_id: row.goal_id,
            parent_id: row.parent_id,
            title: row.title,
            description: row.description,
            status: row.status,
            priority: row.priority,
            assignee_agent_id: row.assignee_agent_id,
            created_by_agent_id: row.created_by_agent_id,
            created_by_user_id: row.created_by_user_id,
            issue_number: row.issue_number,
            identifier: row.identifier,
            request_depth: row.request_depth,
            billing_code: row.billing_code,
            started_at: row.started_at,
            completed_at: row.completed_at,
            cancelled_at: row.cancelled_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IssueApprovalError {
    #[error("issue not found")]
    IssueNotFound,
    #[error("approval not found")]
    ApprovalNotFound,
    #[error("one or more issues not found")]
    IssuesNotFound,
    #[error("issue and approval must belong to the same company")]
    CrossCompany,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub type IssueApprovalResult<T> = Result<T, IssueApprovalError>;

#[derive(Clone)]
pub struct IssueApprovalRepo {
    db: Db,
}

impl IssueApprovalRepo {
    pub fn new(db: &Db) -> Self {
        Self { db: db.clone() }
    }

    async fn get_issue_company(&self, issue_id: Uuid) -> IssueApprovalResult<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT company_id FROM issues WHERE id = $1",
        )
        .bind(issue_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(c,)| c))
    }

    async fn get_approval_company(
        &self,
        approval_id: Uuid,
    ) -> IssueApprovalResult<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT company_id FROM approvals WHERE id = $1",
        )
        .bind(approval_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(c,)| c))
    }

    /// 列出 issue 关联的所有 approval，payload 经 redact。
    pub async fn list_approvals_for_issue(
        &self,
        issue_id: Uuid,
    ) -> IssueApprovalResult<Vec<ApprovalForIssueItem>> {
        if self.get_issue_company(issue_id).await?.is_none() {
            return Err(IssueApprovalError::IssueNotFound);
        }
        let rows: Vec<(
            Uuid,
            Uuid,
            String,
            Option<Uuid>,
            Option<String>,
            String,
            Option<Value>,
            Option<String>,
            Option<String>,
            Option<pc_core::Timestamp>,
            pc_core::Timestamp,
            pc_core::Timestamp,
        )> = sqlx::query_as(
            r#"
            SELECT a.id, a.company_id, a.type, a.requested_by_agent_id,
                   a.requested_by_user_id, a.status, a.payload,
                   a.decision_note, a.decided_by_user_id, a.decided_at,
                   a.created_at, a.updated_at
            FROM issue_approvals ia
            INNER JOIN approvals a ON a.id = ia.approval_id
            WHERE ia.issue_id = $1
            ORDER BY ia.created_at DESC
            "#,
        )
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await?;

        let mut items = Vec::with_capacity(rows.len());
        for (
            id,
            company_id,
            approval_type,
            requested_by_agent_id,
            requested_by_user_id,
            status,
            payload,
            decision_note,
            decided_by_user_id,
            decided_at,
            created_at,
            updated_at,
        ) in rows
        {
            let redacted = match payload {
                Some(value) => sanitize_record(&value),
                None => Value::Object(serde_json::Map::new()),
            };
            items.push(ApprovalForIssueItem {
                id,
                company_id,
                approval_type,
                requested_by_agent_id,
                requested_by_user_id,
                status,
                payload: redacted,
                decision_note,
                decided_by_user_id,
                decided_at,
                created_at,
                updated_at,
            });
        }
        Ok(items)
    }

    /// 列出 approval 关联的所有 issue。
    pub async fn list_issues_for_approval(
        &self,
        approval_id: Uuid,
    ) -> IssueApprovalResult<Vec<IssueForApprovalItem>> {
        if self.get_approval_company(approval_id).await?.is_none() {
            return Err(IssueApprovalError::ApprovalNotFound);
        }
        let rows: Vec<IssueForApprovalDbRow> = sqlx::query_as(
            r#"
            SELECT i.id, i.company_id, i.project_id, i.goal_id, i.parent_id,
                   i.title, i.description, i.status, i.priority,
                   i.assignee_agent_id, i.created_by_agent_id, i.created_by_user_id,
                   i.issue_number, i.identifier, i.request_depth, i.billing_code,
                   i.started_at, i.completed_at, i.cancelled_at, i.created_at, i.updated_at
            FROM issue_approvals ia
            INNER JOIN issues i ON i.id = ia.issue_id
            WHERE ia.approval_id = $1
            ORDER BY ia.created_at DESC
            "#,
        )
        .bind(approval_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// 关联一条 issue ↔ approval（idempotent：同主键直接 no-op）。
    pub async fn link(
        &self,
        issue_id: Uuid,
        approval_id: Uuid,
        actor: Option<LinkActor>,
    ) -> IssueApprovalResult<Option<IssueApprovalLinkRow>> {
        let issue_company = self
            .get_issue_company(issue_id)
            .await?
            .ok_or(IssueApprovalError::IssueNotFound)?;
        let approval_company = self
            .get_approval_company(approval_id)
            .await?
            .ok_or(IssueApprovalError::ApprovalNotFound)?;
        if issue_company != approval_company {
            return Err(IssueApprovalError::CrossCompany);
        }

        let actor = actor.unwrap_or_default();
        let sql = format!(
            "INSERT INTO issue_approvals ({LINK_COLS}) VALUES ($1,$2,$3,$4,$5,$6) \
             ON CONFLICT (issue_id, approval_id) DO NOTHING"
        );
        sqlx::query(&sql)
            .bind(issue_company)
            .bind(issue_id)
            .bind(approval_id)
            .bind(actor.agent_id)
            .bind(actor.user_id.as_deref())
            .bind(pc_core::Timestamp::now())
            .execute(self.db.pool())
            .await?;

        let row: Option<IssueApprovalLinkRow> = sqlx::query_as(&format!(
            "SELECT {LINK_COLS} FROM issue_approvals \
             WHERE issue_id = $1 AND approval_id = $2"
        ))
        .bind(issue_id)
        .bind(approval_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// 解除关联。
    pub async fn unlink(
        &self,
        issue_id: Uuid,
        approval_id: Uuid,
    ) -> IssueApprovalResult<()> {
        let issue_company = self
            .get_issue_company(issue_id)
            .await?
            .ok_or(IssueApprovalError::IssueNotFound)?;
        let approval_company = self
            .get_approval_company(approval_id)
            .await?
            .ok_or(IssueApprovalError::ApprovalNotFound)?;
        if issue_company != approval_company {
            return Err(IssueApprovalError::CrossCompany);
        }
        sqlx::query(
            "DELETE FROM issue_approvals WHERE issue_id = $1 AND approval_id = $2",
        )
        .bind(issue_id)
        .bind(approval_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 批量把 issue 关联到 approval；要求所有 issue 属于同一 company。
    pub async fn link_many_for_approval(
        &self,
        approval_id: Uuid,
        issue_ids: &[Uuid],
        actor: Option<LinkActor>,
    ) -> IssueApprovalResult<()> {
        if issue_ids.is_empty() {
            return Ok(());
        }
        let approval_company = self
            .get_approval_company(approval_id)
            .await?
            .ok_or(IssueApprovalError::ApprovalNotFound)?;

        // 去重
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<Uuid> = issue_ids
            .iter()
            .copied()
            .filter(|id| seen.insert(*id))
            .collect();

        // 校验所有 issue 存在 + 同 company
        let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, company_id FROM issues WHERE id = ANY($1::uuid[])",
        )
        .bind(&unique)
        .fetch_all(self.db.pool())
        .await?;
        if rows.len() != unique.len() {
            return Err(IssueApprovalError::IssuesNotFound);
        }
        for (_id, company_id) in &rows {
            if *company_id != approval_company {
                return Err(IssueApprovalError::CrossCompany);
            }
        }

        // 批量 insert ON CONFLICT DO NOTHING
        let actor = actor.unwrap_or_default();
        let now = pc_core::Timestamp::now();
        for issue_id in &unique {
            sqlx::query(
                "INSERT INTO issue_approvals (company_id, issue_id, approval_id, linked_by_agent_id, linked_by_user_id, created_at) \
                 VALUES ($1,$2,$3,$4,$5,$6) \
                 ON CONFLICT (issue_id, approval_id) DO NOTHING",
            )
            .bind(approval_company)
            .bind(issue_id)
            .bind(approval_id)
            .bind(actor.agent_id)
            .bind(actor.user_id.as_deref())
            .bind(now)
            .execute(self.db.pool())
            .await?;
        }
        Ok(())
    }
}

// 把 RepoResult 别名透传以便上层使用
pub type IssueRepoResult<T> = RepoResult<T>;

// 静默使用：保留 RepoError 以便潜在迁移
#[allow(dead_code)]
fn _ensure_repo_error_in_scope() -> RepoError {
    RepoError::Invalid("unused".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn link_actor_defaults_to_none() {
        let a = LinkActor::default();
        assert!(a.agent_id.is_none());
        assert!(a.user_id.is_none());
    }

    #[test]
    fn approval_for_issue_item_serializes_camel_case_with_payload() {
        let item = ApprovalForIssueItem {
            id: Uuid::nil(),
            company_id: Uuid::nil(),
            approval_type: "issue_approval".into(),
            requested_by_agent_id: None,
            requested_by_user_id: None,
            status: "pending".into(),
            payload: json!({"note": "ok"}),
            decision_note: None,
            decided_by_user_id: None,
            decided_at: None,
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        };
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v["type"], "issue_approval");
        assert_eq!(v["requestedByAgentId"], Value::Null);
        assert_eq!(v["payload"]["note"], "ok");
    }

    #[test]
    fn issue_approval_link_row_serializes_camel_case() {
        let row = IssueApprovalLinkRow {
            company_id: Uuid::nil(),
            issue_id: Uuid::nil(),
            approval_id: Uuid::nil(),
            linked_by_agent_id: None,
            linked_by_user_id: Some("u-1".into()),
            created_at: pc_core::Timestamp::now(),
        };
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["linkedByUserId"], "u-1");
        assert_eq!(v["linkedByAgentId"], Value::Null);
    }

    #[test]
    fn issue_approval_error_display_is_user_facing() {
        assert_eq!(IssueApprovalError::IssueNotFound.to_string(), "issue not found");
        assert_eq!(
            IssueApprovalError::ApprovalNotFound.to_string(),
            "approval not found"
        );
        assert_eq!(
            IssueApprovalError::CrossCompany.to_string(),
            "issue and approval must belong to the same company"
        );
    }
}
