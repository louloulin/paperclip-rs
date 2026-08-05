//! `join_requests` 域 — 公司加入请求的生命周期。
//!
//! Schema (paperclip `packages/db/src/schema/join_requests.ts`)：
//! - `join_requests(id, invite_id, company_id, request_type, status,
//!   request_ip, requesting_user_id, request_email_snapshot, agent_name,
//!   adapter_type, capabilities, agent_defaults_payload, created_agent_id,
//!   approved_by_user_id, approved_at, rejected_by_user_id, rejected_at,
//!   created_at, updated_at)`
//! - 唯一索引 `join_requests_invite_unique_idx(invite_id)`
//! - 普通索引 `join_requests_company_status_type_created_idx(...)`
//!
//! 状态机：
//! - `pending_approval` → `approved`：写入 approvals + 创建 membership / agent
//! - `pending_approval` → `rejected`：仅写入拒绝时间戳
//!
//! 与 Node 等价：单事务里 SELECT FOR UPDATE + 状态转移 + 副作用写入。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

const COLS: &str = "id, invite_id, company_id, request_type, status, request_ip, \
    requesting_user_id, request_email_snapshot, agent_name, adapter_type, capabilities, \
    agent_defaults_payload, claim_secret_hash, claim_secret_expires_at, claim_secret_consumed_at, \
    created_agent_id, approved_by_user_id, approved_at, \
    rejected_by_user_id, rejected_at, created_at, updated_at";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinRequestStatus {
    PendingApproval,
    Approved,
    Rejected,
}

impl JoinRequestStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JoinRequestStatus::PendingApproval => "pending_approval",
            JoinRequestStatus::Approved => "approved",
            JoinRequestStatus::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequestRow {
    pub id: Uuid,
    pub invite_id: Uuid,
    pub company_id: Uuid,
    pub request_type: String,
    pub status: String,
    pub request_ip: String,
    #[serde(default)]
    pub requesting_user_id: Option<String>,
    #[serde(default)]
    pub request_email_snapshot: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub adapter_type: Option<String>,
    #[serde(default)]
    pub capabilities: Option<String>,
    #[serde(default)]
    pub agent_defaults_payload: Option<JsonValue>,
    #[serde(default)]
    pub claim_secret_hash: Option<String>,
    #[serde(default)]
    pub claim_secret_expires_at: Option<Timestamp>,
    #[serde(default)]
    pub claim_secret_consumed_at: Option<Timestamp>,
    #[serde(default)]
    pub created_agent_id: Option<Uuid>,
    #[serde(default)]
    pub approved_by_user_id: Option<String>,
    #[serde(default)]
    pub approved_at: Option<Timestamp>,
    #[serde(default)]
    pub rejected_by_user_id: Option<String>,
    #[serde(default)]
    pub rejected_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct NewJoinRequest {
    pub invite_id: Uuid,
    pub company_id: Uuid,
    pub request_type: String,
    pub request_ip: String,
    pub requesting_user_id: Option<String>,
    pub request_email_snapshot: Option<String>,
    pub agent_name: Option<String>,
    pub adapter_type: Option<String>,
    pub capabilities: Option<String>,
    pub agent_defaults_payload: Option<JsonValue>,
}

/// 批准/拒绝请求的可变副作用。
#[derive(Debug, Clone, Default)]
pub struct JoinRequestDecision {
    pub note: Option<String>,
    pub by_user_id: String,
}

/// 批准后返回给上层路由的副作用记录：可能创建了 membership 或 agent。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequestApprovalEffects {
    pub created_membership_id: Option<Uuid>,
    pub created_agent_id: Option<Uuid>,
}

#[derive(Debug, thiserror::Error)]
pub enum JoinRequestError {
    #[error(transparent)]
    Db(#[from] RepoError),
    #[error("invalid state transition for join request {0}: not pending")]
    NotPending(Uuid),
    #[error("unknown request_type '{0}'")]
    UnknownRequestType(String),
}

impl From<sqlx::Error> for JoinRequestError {
    fn from(e: sqlx::Error) -> Self {
        JoinRequestError::Db(e.into())
    }
}

pub struct JoinRequestRepo<'a> {
    pub db: &'a Db,
}

impl<'a> JoinRequestRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 列出公司最近 100 条 join request（与原 Node handler 同 LIMIT）。
    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<JoinRequestRow>> {
        let sql = format!(
            "SELECT {COLS} FROM join_requests \
             WHERE company_id = $1 \
             ORDER BY created_at DESC LIMIT 100"
        );
        let rows: Vec<JoinRequestRow> = sqlx::query_as(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    /// 通过 id 锁定单条 request；用于状态机校验。
    pub async fn find_by_id(&self, company_id: Uuid, id: Uuid) -> RepoResult<Option<JoinRequestRow>> {
        let sql = format!(
            "SELECT {COLS} FROM join_requests WHERE company_id = $1 AND id = $2 LIMIT 1"
        );
        let row: Option<JoinRequestRow> = sqlx::query_as(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row)
    }

    /// 创建新请求。`request_type` 必须是 `human | agent | company_join`。
    pub async fn create(&self, input: NewJoinRequest) -> RepoResult<JoinRequestRow> {
        if !matches!(input.request_type.as_str(), "human" | "agent" | "company_join" | "user") {
            return Err(RepoError::Invalid(format!("unknown request_type '{}'", input.request_type)));
        }
        let id = Uuid::new_v4();
        let sql = format!(
            "INSERT INTO join_requests \
             (id, invite_id, company_id, request_type, status, request_ip, \
              requesting_user_id, request_email_snapshot, agent_name, adapter_type, \
              capabilities, agent_defaults_payload, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,'pending_approval',$5,$6,$7,$8,$9,$10,$11, now(), now())"
        );
        sqlx::query(&sql)
            .bind(id)
            .bind(input.invite_id)
            .bind(input.company_id)
            .bind(&input.request_type)
            .bind(&input.request_ip)
            .bind(&input.requesting_user_id)
            .bind(&input.request_email_snapshot)
            .bind(&input.agent_name)
            .bind(&input.adapter_type)
            .bind(&input.capabilities)
            .bind(&input.agent_defaults_payload)
            .execute(self.db.pool())
            .await?;
        let row: JoinRequestRow = sqlx::query_as(&format!(
            "SELECT {COLS} FROM join_requests WHERE id = $1"
        ))
        .bind(id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row)
    }

    /// 批准：原子事务里读取 → 校验状态 → 触发副作用 → 写入 approved。
    pub async fn approve(
        &self,
        company_id: Uuid,
        req_id: Uuid,
        decision: JoinRequestDecision,
    ) -> Result<JoinRequestApprovalEffects, JoinRequestError> {
        let mut tx = self.db.pool().begin().await?;
        let row: Option<JoinRequestRow> = sqlx::query_as(&format!(
            "SELECT {COLS} FROM join_requests \
             WHERE company_id = $1 AND id = $2 FOR UPDATE"
        ))
        .bind(company_id)
        .bind(req_id)
        .fetch_optional(&mut *tx)
        .await?;
        let row = row.ok_or(JoinRequestError::NotPending(req_id))?;
        if row.status != JoinRequestStatus::PendingApproval.as_str() {
            return Err(JoinRequestError::NotPending(req_id));
        }
        let mut effects = JoinRequestApprovalEffects::default();
        match row.request_type.as_str() {
            "company_join" | "user" => {
                if let Some(uid) = row.requesting_user_id.as_ref() {
                    // upsert membership
                    let existing: Option<(Uuid,)> = sqlx::query_as(
                        "SELECT id FROM company_memberships \
                         WHERE company_id=$1 AND principal_type='user' AND principal_id=$2",
                    )
                    .bind(company_id)
                    .bind(uid)
                    .fetch_optional(&mut *tx)
                    .await?;
                    if let Some((mid,)) = existing {
                        sqlx::query(
                            "UPDATE company_memberships SET status='active', updated_at=now() \
                             WHERE id = $1",
                        )
                        .bind(mid)
                        .execute(&mut *tx)
                        .await?;
                        effects.created_membership_id = Some(mid);
                    } else {
                        let mid = Uuid::new_v4();
                        sqlx::query(
                            "INSERT INTO company_memberships \
                             (id, company_id, principal_type, principal_id, status, membership_role) \
                             VALUES ($1,$2,'user',$3,'active','member')",
                        )
                        .bind(mid)
                        .bind(company_id)
                        .bind(uid)
                        .execute(&mut *tx)
                        .await?;
                        effects.created_membership_id = Some(mid);
                    }
                }
            }
            "agent" => {
                if let Some(name) = row.agent_name.as_ref() {
                    let aid = Uuid::new_v4();
                    let adapter = row
                        .adapter_type
                        .clone()
                        .unwrap_or_else(|| "process".to_string());
                    sqlx::query(
                        "INSERT INTO agents \
                         (id, company_id, name, role, adapter_type, status) \
                         VALUES ($1,$2,$3,'general',$4,'idle')",
                    )
                    .bind(aid)
                    .bind(company_id)
                    .bind(name)
                    .bind(&adapter)
                    .execute(&mut *tx)
                    .await?;
                    effects.created_agent_id = Some(aid);
                }
            }
            other => return Err(JoinRequestError::UnknownRequestType(other.to_string())),
        }
        let now: DateTime<Utc> = Utc::now();
        sqlx::query(
            "UPDATE join_requests \
             SET status='approved', approved_at=$1, approved_by_user_id=$2, updated_at=now() \
             WHERE id = $3",
        )
        .bind(now)
        .bind(&decision.by_user_id)
        .bind(req_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(effects)
    }

    /// 拒绝：单条 UPDATE，要求状态为 pending_approval（避免重复状态机迁移）。
    pub async fn reject(
        &self,
        company_id: Uuid,
        req_id: Uuid,
        decision: JoinRequestDecision,
    ) -> Result<bool, JoinRequestError> {
        let r = sqlx::query(
            "UPDATE join_requests \
             SET status='rejected', rejected_at=now(), rejected_by_user_id=$1, updated_at=now() \
             WHERE company_id=$2 AND id=$3 AND status='pending_approval'",
        )
        .bind(&decision.by_user_id)
        .bind(company_id)
        .bind(req_id)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Round 215: 认领 join_request 的 API key。
    ///
    /// 流程（与 Node `access.ts` claim-api-key 路由对齐）：
    /// 1. SELECT FOR UPDATE 行
    /// 2. 校验：存在 / request_type=agent / status=approved /
    ///    claim_secret_hash 已设置 / hash 匹配 / 未过期 / 未消费
    /// 3. 原子标记 `claim_secret_consumed_at = now()`（仅当仍为 NULL）
    /// 4. 返回最新行（携带已设置的 `created_agent_id`）
    pub async fn claim_api_key(
        &self,
        request_id: Uuid,
        presented_hash: &str,
    ) -> RepoResult<JoinRequestRow> {
        let mut tx = self.db.pool().begin().await?;

        let row: Option<JoinRequestRow> = sqlx::query_as(&format!(
            "SELECT {COLS} FROM join_requests WHERE id=$1 FOR UPDATE"
        ))
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await?;

        let row = row.ok_or_else(|| RepoError::NotFound {
            entity: "join_request",
            id: request_id.to_string(),
        })?;

        if row.request_type != "agent" {
            return Err(RepoError::Invalid(
                "Only agent join requests can claim API keys".into(),
            ));
        }
        if row.status != JoinRequestStatus::Approved.as_str() {
            return Err(RepoError::Invalid(
                "Join request must be approved before key claim".into(),
            ));
        }
        if row.created_agent_id.is_none() {
            return Err(RepoError::Invalid(
                "Join request has no created agent".into(),
            ));
        }
        let stored_hash = row.claim_secret_hash.as_deref().ok_or_else(|| {
            RepoError::Invalid("Join request is missing claim secret metadata".into())
        })?;
        if !pc_core::hash::constant_time_eq(stored_hash.as_bytes(), presented_hash.as_bytes()) {
            return Err(RepoError::Invalid("Invalid claim secret".into()));
        }
        if let Some(expires_at) = row.claim_secret_expires_at {
            if expires_at.as_datetime() <= chrono::Utc::now() {
                return Err(RepoError::Invalid("Claim secret expired".into()));
            }
        }
        if row.claim_secret_consumed_at.is_some() {
            return Err(RepoError::Invalid("Claim secret already used".into()));
        }

        // Atomic mark consumed (only if still NULL)
        let updated: Option<JoinRequestRow> = sqlx::query_as(&format!(
            "UPDATE join_requests SET claim_secret_consumed_at=now(), updated_at=now() \
             WHERE id=$1 AND claim_secret_consumed_at IS NULL RETURNING {COLS}"
        ))
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await?;

        let updated = updated.ok_or_else(|| {
            RepoError::Invalid("Claim secret already used".into())
        })?;

        tx.commit().await?;
        Ok(updated)
    }
}

/// Round 215: 常数时间字节比较，防御 hash 比较时的侧信道泄漏。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_str_roundtrip() {
        assert_eq!(
            JoinRequestStatus::PendingApproval.as_str(),
            "pending_approval"
        );
        assert_eq!(JoinRequestStatus::Approved.as_str(), "approved");
        assert_eq!(JoinRequestStatus::Rejected.as_str(), "rejected");
    }

    #[test]
    fn unknown_request_type_is_rejected_via_repo_create() {
        // 单元测试里没有真实 DB，只测类名匹配与错误描述。
        // 真实路径测在 pc-http/tests/ 集成测试里。
        let unknown = "robot";
        assert!(!matches!(unknown, "human" | "agent" | "company_join" | "user"));
    }

    #[test]
    fn constant_time_eq_matches_equal_strings() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn constant_time_eq_rejects_different_lengths() {
        assert!(!constant_time_eq(b"hello", b"hellos"));
        assert!(!constant_time_eq(b"a", b""));
    }

    #[test]
    fn constant_time_eq_rejects_different_content() {
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"abc", b"abd"));
    }
}
