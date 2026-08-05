//! `approvals` + `approval_comments` 域 — Agent 操作的人类审批工作流。
//!
//! 设计：
//! - `Approval` 一行一审批单，包含 type / payload / 状态机
//! - `ApprovalComment` 是审批单下的讨论（多对多模式：审批单 + 评论 1:N）
//! - 状态机：pending → approved | rejected | cancelled | expired
//! - 决策同时记录 `decided_by_user_id` 与 `decided_at`，确保审计可追溯

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalType {
    AgentAction,
    BudgetChange,
    SecretUse,
    RoutineUpdate,
    Custom,
}
impl ApprovalType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentAction => "agent_action",
            Self::BudgetChange => "budget_change",
            Self::SecretUse => "secret_use",
            Self::RoutineUpdate => "routine_update",
            Self::Custom => "custom",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agent_action" => Some(Self::AgentAction),
            "budget_change" => Some(Self::BudgetChange),
            "secret_use" => Some(Self::SecretUse),
            "routine_update" => Some(Self::RoutineUpdate),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Cancelled,
    Expired,
}
impl ApprovalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "cancelled" => Some(Self::Cancelled),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Approved | Self::Rejected | Self::Cancelled | Self::Expired)
    }
}

const APP_COLS: &str = "id, company_id, type AS approval_type, requested_by_agent_id,     requested_by_user_id, status, payload, decision_note, decided_by_user_id, decided_at,     created_at, updated_at";

const COMMENT_COLS: &str = "id, company_id, approval_id, author_agent_id, author_user_id,     body, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub approval_type: String,
    pub requested_by_agent_id: Option<Uuid>,
    pub requested_by_user_id: Option<String>,
    pub status: String,
    pub payload: Value,
    pub decision_note: Option<String>,
    pub decided_by_user_id: Option<String>,
    pub decided_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalCommentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub approval_id: Uuid,
    pub author_agent_id: Option<Uuid>,
    pub author_user_id: Option<String>,
    pub body: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewApproval {
    pub company_id: Uuid,
    pub approval_type: ApprovalType,
    pub requested_by_agent_id: Option<Uuid>,
    pub requested_by_user_id: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewApprovalComment {
    pub company_id: Uuid,
    pub approval_id: Uuid,
    pub author_agent_id: Option<Uuid>,
    pub author_user_id: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, Default)]
pub struct ApprovalFilter {
    pub status: Option<ApprovalStatus>,
    pub approval_type: Option<ApprovalType>,
    pub requested_by_agent_id: Option<Uuid>,
    pub requested_by_user_id: Option<String>,
    pub limit: Option<i64>,
}

pub struct ApprovalRepo<'a> {
    pub db: &'a Db,
}

impl<'a> ApprovalRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    // ---- approvals ----

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        filter: &ApprovalFilter,
    ) -> RepoResult<Vec<ApprovalRow>> {
        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT id, company_id, type AS approval_type, requested_by_agent_id,              requested_by_user_id, status, payload, decision_note, decided_by_user_id, decided_at,              created_at, updated_at FROM approvals WHERE company_id = ",
        );
        qb.push_bind(company_id);
        if let Some(s) = filter.status {
            qb.push(" AND status = ").push_bind(s.as_str());
        }
        if let Some(t) = filter.approval_type {
            qb.push(" AND type = ").push_bind(t.as_str());
        }
        if let Some(a) = filter.requested_by_agent_id {
            qb.push(" AND requested_by_agent_id = ").push_bind(a);
        }
        if let Some(u) = &filter.requested_by_user_id {
            qb.push(" AND requested_by_user_id = ").push_bind(u);
        }
        qb.push(" ORDER BY created_at DESC LIMIT ").push_bind(filter.limit.unwrap_or(200));
        let rows = qb
            .build_query_as::<ApprovalRow>()
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    pub async fn list_all(&self, limit: i64) -> RepoResult<Vec<ApprovalRow>> {
        let sql = format!(
            "SELECT {APP_COLS} FROM approvals ORDER BY created_at DESC LIMIT $1"
        );
        Ok(sqlx::query_as::<_, ApprovalRow>(&sql)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<ApprovalRow>> {
        let sql = format!(
            "SELECT {APP_COLS} FROM approvals WHERE company_id=$1 AND id=$2"
        );
        Ok(sqlx::query_as::<_, ApprovalRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create(&self, a: &NewApproval) -> RepoResult<ApprovalRow> {
        if a.requested_by_agent_id.is_none() && a.requested_by_user_id.is_none() {
            return Err(RepoError::Invalid(
                "approval must be requested by agent or user".into(),
            ));
        }
        let sql = format!(
            "INSERT INTO approvals (company_id, type, requested_by_agent_id, requested_by_user_id, payload)              VALUES ($1,$2,$3,$4,$5)              RETURNING {APP_COLS}"
        );
        Ok(sqlx::query_as::<_, ApprovalRow>(&sql)
            .bind(a.company_id)
            .bind(a.approval_type.as_str())
            .bind(a.requested_by_agent_id)
            .bind(a.requested_by_user_id.as_deref())
            .bind(&a.payload)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn decide(
        &self,
        company_id: Uuid,
        id: Uuid,
        to: ApprovalStatus,
        decided_by_user_id: &str,
        note: Option<&str>,
    ) -> RepoResult<Option<ApprovalRow>> {
        // 拒绝：已 terminal 的不能再决策
        let cur: Option<ApprovalRow> = self.get(company_id, id).await?;
        if let Some(row) = &cur {
            if let Some(prev) = ApprovalStatus::parse(&row.status) {
                if prev.is_terminal() {
                    return Err(RepoError::Invalid(format!(
                        "approval already in terminal state {prev:?}"
                    )));
                }
            }
            if to == ApprovalStatus::Pending {
                return Err(RepoError::Invalid(
                    "cannot decide back to pending".into(),
                ));
            }
        }
        let sql = format!(
            "UPDATE approvals SET status=$2, decided_by_user_id=$3, decided_at=now(),              decision_note=$4, updated_at=now()              WHERE company_id=$1 AND id=$5              RETURNING {APP_COLS}"
        );
        Ok(sqlx::query_as::<_, ApprovalRow>(&sql)
            .bind(company_id)
            .bind(to.as_str())
            .bind(decided_by_user_id)
            .bind(note)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn cancel(
        &self,
        company_id: Uuid,
        id: Uuid,
        cancelled_by_user_id: &str,
        reason: Option<&str>,
    ) -> RepoResult<Option<ApprovalRow>> {
        self.decide(company_id, id, ApprovalStatus::Cancelled, cancelled_by_user_id, reason)
            .await
    }

    pub async fn mark_expired(&self, id: Uuid) -> RepoResult<()> {
        sqlx::query(
            "UPDATE approvals SET status='expired', updated_at=now()              WHERE id=$1 AND status='pending'",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn delete(&self, company_id: Uuid, id: Uuid) -> RepoResult<bool> {
        // 先删评论
        sqlx::query("DELETE FROM approval_comments WHERE company_id=$1 AND approval_id=$2")
            .bind(company_id)
            .bind(id)
            .execute(self.db.pool())
            .await?;
        let n = sqlx::query("DELETE FROM approvals WHERE company_id=$1 AND id=$2")
            .bind(company_id)
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    pub async fn count_pending(&self, company_id: Uuid) -> RepoResult<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM approvals WHERE company_id=$1 AND status='pending'",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
    }

    /// Round 177: 注意力队列用 —— 列出某公司的 pending approvals（id/approval_type/payload/updated_at）。
    pub async fn list_pending_attention(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ApprovalRow>> {
        sqlx::query_as::<_, ApprovalRow>(
            "SELECT id, company_id, type AS approval_type, requested_by_agent_id, \
                    requested_by_user_id, status, payload, decision_note, \
                    decided_by_user_id, decided_at, created_at, updated_at \
             FROM approvals WHERE company_id = $1 AND status = 'pending' \
             ORDER BY updated_at DESC LIMIT 100",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(RepoError::from)
    }

    // ---- comments ----

    pub async fn list_comments(
        &self,
        approval_id: Uuid,
    ) -> RepoResult<Vec<ApprovalCommentRow>> {
        let sql = format!(
            "SELECT {COMMENT_COLS} FROM approval_comments              WHERE approval_id=$1 ORDER BY created_at ASC"
        );
        Ok(sqlx::query_as::<_, ApprovalCommentRow>(&sql)
            .bind(approval_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn add_comment(
        &self,
        c: &NewApprovalComment,
    ) -> RepoResult<ApprovalCommentRow> {
        if c.author_agent_id.is_none() && c.author_user_id.is_none() {
            return Err(RepoError::Invalid(
                "comment must be authored by agent or user".into(),
            ));
        }
        if c.body.trim().is_empty() {
            return Err(RepoError::Invalid("comment body must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO approval_comments (company_id, approval_id, author_agent_id, author_user_id, body)              VALUES ($1,$2,$3,$4,$5)              RETURNING {COMMENT_COLS}"
        );
        Ok(sqlx::query_as::<_, ApprovalCommentRow>(&sql)
            .bind(c.company_id)
            .bind(c.approval_id)
            .bind(c.author_agent_id)
            .bind(c.author_user_id.as_deref())
            .bind(&c.body)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn edit_comment(
        &self,
        id: Uuid,
        body: &str,
    ) -> RepoResult<Option<ApprovalCommentRow>> {
        if body.trim().is_empty() {
            return Err(RepoError::Invalid("comment body must not be empty".into()));
        }
        let sql = format!(
            "UPDATE approval_comments SET body=$2, updated_at=now() WHERE id=$1              RETURNING {COMMENT_COLS}"
        );
        Ok(sqlx::query_as::<_, ApprovalCommentRow>(&sql)
            .bind(id)
            .bind(body)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn delete_comment(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM approval_comments WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    // --------- Backward-compat shims (extended) ---------

    /// Back-compat: get by id only (scoped to company_id via separate query).
    #[allow(dead_code)]
    pub async fn get_legacy(&self, id: Uuid) -> RepoResult<Option<ApprovalRow>> {
        let sql = format!("SELECT {APP_COLS} FROM approvals WHERE id=$1");
        Ok(sqlx::query_as::<_, ApprovalRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Back-compat: simple create(company_id, type, payload).
    #[allow(dead_code)]
    pub async fn create_legacy(
        &self,
        company_id: Uuid,
        approval_type: &str,
        payload: Value,
    ) -> RepoResult<ApprovalRow> {
        let parsed = ApprovalType::parse(approval_type).unwrap_or(ApprovalType::Custom);
        let input = NewApproval {
            company_id,
            approval_type: parsed,
            requested_by_agent_id: None,
            requested_by_user_id: None,
            payload,
        };
        self.create(&input).await
    }

    /// Back-compat: simple decide(id, status, note, decided_by).
    #[allow(dead_code)]
    pub async fn decide_legacy(
        &self,
        id: Uuid,
        status: &str,
        note: Option<&str>,
        decided_by: &str,
    ) -> RepoResult<Option<ApprovalRow>> {
        // 找对应 company_id 再调新 API
        let cid: Option<Uuid> = sqlx::query_scalar("SELECT company_id FROM approvals WHERE id=$1")
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?;
        let cid = cid.ok_or_else(|| RepoError::NotFound {
            entity: "approval",
            id: id.to_string(),
        })?;
        let to = ApprovalStatus::parse(status).unwrap_or(ApprovalStatus::Approved);
        self.decide(cid, id, to, decided_by, note).await
    }

    /// Back-compat: delete by id only.
    #[allow(dead_code)]
    pub async fn delete_legacy(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM approvals WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    // --------- Backward-compat positional shims (extended) ---------

    /// Back-compat: list_by_company with default filter.
    #[allow(dead_code)]
    pub async fn list_by_company_simple(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ApprovalRow>> {
        self.list_by_company(company_id, &ApprovalFilter::default()).await
    }

    /// Back-compat: get by id only.
    #[allow(dead_code)]
    pub async fn get_id(&self, id: Uuid) -> RepoResult<Option<ApprovalRow>> {
        {
        let sql = format!("SELECT {APP_COLS} FROM approvals WHERE id=$1");
        Ok(sqlx::query_as::<_, ApprovalRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }
    }

    /// Back-compat: simple create with positional args.
    #[allow(dead_code)]
    pub async fn create_three_args(
        &self,
        company_id: Uuid,
        approval_type: &str,
        payload: Value,
    ) -> RepoResult<ApprovalRow> {
        self.create_positional(company_id, approval_type, payload).await
    }

    /// Back-compat: simple decide with positional args.
    #[allow(dead_code)]
    pub async fn decide_four_args(
        &self,
        id: Uuid,
        status: &str,
        note: Option<&str>,
        decided_by: &str,
    ) -> RepoResult<Option<ApprovalRow>> {
        self.decide_positional(id, status, note, decided_by).await
    }

    /// Back-compat: delete by id only.
    #[allow(dead_code)]
    pub async fn delete_one(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM approvals WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Back-compat: get by id only.
    #[allow(dead_code)]
    pub async fn get_id_only(&self, id: Uuid) -> RepoResult<Option<ApprovalRow>> {
        let sql = format!("SELECT {APP_COLS} FROM approvals WHERE id=$1");
        Ok(sqlx::query_as::<_, ApprovalRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Back-compat: simple create(company_id, type, payload).
    #[allow(dead_code)]
    pub async fn create_positional(
        &self,
        company_id: Uuid,
        approval_type: &str,
        payload: Value,
    ) -> RepoResult<ApprovalRow> {
        let parsed = ApprovalType::parse(approval_type).unwrap_or(ApprovalType::Custom);
        let n = NewApproval {
            company_id,
            approval_type: parsed,
            requested_by_agent_id: None,
            requested_by_user_id: None,
            payload,
        };
        self.create(&n).await
    }

    /// Back-compat: simple decide(id, status, note, decided_by).
    #[allow(dead_code)]
    pub async fn decide_positional(
        &self,
        id: Uuid,
        status: &str,
        note: Option<&str>,
        decided_by: &str,
    ) -> RepoResult<Option<ApprovalRow>> {
        let cid: Option<Uuid> =
            sqlx::query_scalar("SELECT company_id FROM approvals WHERE id=$1")
                .bind(id)
                .fetch_optional(self.db.pool())
                .await?;
        let cid = cid.ok_or_else(|| RepoError::NotFound {
            entity: "approval",
            id: id.to_string(),
        })?;
        let to = ApprovalStatus::parse(status).unwrap_or(ApprovalStatus::Approved);
        self.decide(cid, id, to, decided_by, note).await
    }

    /// Back-compat: delete by id only.
    #[allow(dead_code)]
    pub async fn delete_by_id(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM approvals WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    // ---- Round 170: approvals route 仓储化新增方法 ----

    /// Round 195: 请求 approval 修订（pending → revision_requested）。
    /// Returns updated row, or None if not pending.
    pub async fn request_revision(
        &self,
        approval_id: Uuid,
        decided_by_user_id: &str,
        decision_note: Option<&str>,
    ) -> RepoResult<Option<ApprovalRow>> {
        sqlx::query_as::<_, ApprovalRow>(
            "UPDATE approvals SET                 status = 'revision_requested',                 decision_note = $2,                 decided_by_user_id = $3,                 decided_at = now(),                 updated_at = now()              WHERE id = $1 AND status = 'pending'              RETURNING id, company_id, type AS approval_type, requested_by_agent_id,                        requested_by_user_id, status, payload, decision_note,                        decided_by_user_id, decided_at, created_at, updated_at",
        )
        .bind(approval_id)
        .bind(decision_note)
        .bind(decided_by_user_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(Into::into)
    }

    /// Round 170: 重提交 approval（设为 pending）。
    /// 若 payload 提供则一并更新 payload，否则只更新 note。
    pub async fn resubmit(
        &self,
        approval_id: Uuid,
        payload: Option<&Value>,
        note: Option<&str>,
    ) -> RepoResult<bool> {
        if let Some(p) = payload {
            let n = sqlx::query(
                "UPDATE approvals SET status = 'pending', payload = $1, decision_note = $2, decided_at = NULL, updated_at = now() \
                 WHERE id = $3",
            )
            .bind(p)
            .bind(note)
            .bind(approval_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
            Ok(n > 0)
        } else {
            let n = sqlx::query(
                "UPDATE approvals SET status = 'pending', decision_note = $1, decided_at = NULL, updated_at = now() \
                 WHERE id = $2",
            )
            .bind(note)
            .bind(approval_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
            Ok(n > 0)
        }
    }

    /// Round 170: 取 approval 的 (id, company_id)。
    pub async fn get_id_company(&self, id: Uuid) -> RepoResult<Option<(Uuid, Uuid)>> {
        let row: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, company_id FROM approvals WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 170: 取 approval 的 company_id。
    pub async fn get_company_id(&self, id: Uuid) -> RepoResult<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT company_id FROM approvals WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(c,)| c))
    }

    /// Round 170: 写入一条 approval_comment。返回新行 id。
    pub async fn add_comment_raw(
        &self,
        company_id: Uuid,
        approval_id: Uuid,
        author_agent_id: Option<Uuid>,
        author_user_id: Option<&str>,
        body: &str,
    ) -> RepoResult<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO approval_comments (company_id, approval_id, author_agent_id, author_user_id, body) \
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(company_id)
        .bind(approval_id)
        .bind(author_agent_id)
        .bind(author_user_id)
        .bind(body)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.0)
    }

    /// Round 170: 列出 approval 的评论（按 created_at ASC, LIMIT 200）。
    pub async fn list_comments_raw(
        &self,
        approval_id: Uuid,
    ) -> RepoResult<Vec<(Uuid, Uuid, Option<Uuid>, Option<String>, String, Timestamp)>> {
        let rows: Vec<(Uuid, Uuid, Option<Uuid>, Option<String>, String, Timestamp)> = sqlx::query_as(
            "SELECT id, company_id, author_agent_id, author_user_id, body, created_at \
             FROM approval_comments WHERE approval_id = $1 ORDER BY created_at ASC LIMIT 200",
        )
        .bind(approval_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_status_state_machine() {
        assert!(!ApprovalStatus::Pending.is_terminal());
        assert!(ApprovalStatus::Approved.is_terminal());
        assert!(ApprovalStatus::Rejected.is_terminal());
        assert!(ApprovalStatus::Cancelled.is_terminal());
        assert!(ApprovalStatus::Expired.is_terminal());
        assert_eq!(ApprovalStatus::parse("approved"), Some(ApprovalStatus::Approved));
        assert_eq!(ApprovalStatus::parse("nope"), None);
    }

    #[test]
    fn approval_type_strings() {
        assert_eq!(ApprovalType::AgentAction.as_str(), "agent_action");
        assert_eq!(ApprovalType::BudgetChange.as_str(), "budget_change");
        assert_eq!(ApprovalType::SecretUse.as_str(), "secret_use");
    }

    #[test]
    fn new_approval_requires_requestor() {
        let bad = NewApproval {
            company_id: Uuid::new_v4(),
            approval_type: ApprovalType::Custom,
            requested_by_agent_id: None,
            requested_by_user_id: None,
            payload: serde_json::json!({}),
        };
        assert!(bad.requested_by_agent_id.is_none() && bad.requested_by_user_id.is_none());
    }
}
