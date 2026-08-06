//! `summary_slots` 域 — 摘要槽位（公司/Agent/Document/Issue scope）。
//!
//! 设计：
//! - 单一 `(company, scope_kind, scope_id, slot_key)` 唯一约束
//! - 与 paperclip DB 中所有 `summary_slot_*` 视图/路由等价
//! - `ScopeKind` 枚举：`Company` / `Agent` / `Document` / `Issue`
//! - `Status` 枚举：`Idle` / `Generating` / `Ready` / `Stale` / `Failed`

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Company,
    Agent,
    Document,
    Issue,
}
impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Company => "company",
            Self::Agent => "agent",
            Self::Document => "document",
            Self::Issue => "issue",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "company" => Some(Self::Company),
            "agent" => Some(Self::Agent),
            "document" => Some(Self::Document),
            "issue" => Some(Self::Issue),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarySlotStatus {
    Idle,
    Generating,
    Ready,
    Stale,
    Failed,
}
impl SummarySlotStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Generating => "generating",
            Self::Ready => "ready",
            Self::Stale => "stale",
            Self::Failed => "failed",
        }
    }
}

const COLS: &str = "id, company_id, scope_kind, scope_id, slot_key, document_id,      status, failure_reason, generating_issue_id, last_generated_at,      last_generated_by_agent_id, last_model, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummarySlotRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub scope_kind: String,
    pub scope_id: Option<Uuid>,
    pub slot_key: String,
    pub document_id: Option<Uuid>,
    pub status: String,
    pub failure_reason: Option<String>,
    pub generating_issue_id: Option<Uuid>,
    pub last_generated_at: Option<Timestamp>,
    pub last_generated_by_agent_id: Option<Uuid>,
    pub last_model: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarySlotKey {
    WeeklyBoardUpdate,
    ProjectStatus,
    AgentPerformance,
    IssueTriage,
    Custom,
}
impl SummarySlotKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WeeklyBoardUpdate => "weekly_board_update",
            Self::ProjectStatus => "project_status",
            Self::AgentPerformance => "agent_performance",
            Self::IssueTriage => "issue_triage",
            Self::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSummarySlot {
    pub company_id: Uuid,
    pub scope_kind: ScopeKind,
    pub scope_id: Option<Uuid>,
    pub slot_key: String,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummarySlotPatch {
    pub status: Option<SummarySlotStatus>,
    pub failure_reason: Option<String>,
    pub generating_issue_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub last_model: Option<String>,
}

pub struct SummaryRepo<'a> {
    pub db: &'a Db,
}

impl<'a> SummaryRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<SummarySlotRow>> {
        let sql = format!(
            "SELECT {COLS} FROM summary_slots WHERE company_id=$1              ORDER BY updated_at DESC",
        );
        Ok(sqlx::query_as::<_, SummarySlotRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_for_scope(
        &self,
        company_id: Uuid,
        scope_kind: ScopeKind,
        scope_id: Uuid,
    ) -> RepoResult<Vec<SummarySlotRow>> {
        let sql = format!(
            "SELECT {COLS} FROM summary_slots              WHERE company_id=$1 AND scope_kind=$2 AND scope_id=$3              ORDER BY slot_key"
        );
        Ok(sqlx::query_as::<_, SummarySlotRow>(&sql)
            .bind(company_id)
            .bind(scope_kind.as_str())
            .bind(scope_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get_for_scope_slot(
        &self,
        company_id: Uuid,
        scope_kind: ScopeKind,
        scope_id: Uuid,
        slot_key: &str,
    ) -> RepoResult<Option<SummarySlotRow>> {
        let sql = format!(
            "SELECT {COLS} FROM summary_slots              WHERE company_id=$1 AND scope_kind=$2 AND scope_id=$3 AND slot_key=$4"
        );
        Ok(sqlx::query_as::<_, SummarySlotRow>(&sql)
            .bind(company_id)
            .bind(scope_kind.as_str())
            .bind(scope_id)
            .bind(slot_key)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn upsert(&self, n: &NewSummarySlot) -> RepoResult<SummarySlotRow> {
        if n.slot_key.trim().is_empty() {
            return Err(RepoError::Invalid("slot_key must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO summary_slots (company_id, scope_kind, scope_id, slot_key, document_id)              VALUES ($1,$2,$3,$4,$5)              ON CONFLICT (company_id, scope_kind, scope_id, slot_key)              DO UPDATE SET document_id=COALESCE(EXCLUDED.document_id, summary_slots.document_id),                            updated_at=now()              RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, SummarySlotRow>(&sql)
            .bind(n.company_id)
            .bind(n.scope_kind.as_str())
            .bind(n.scope_id)
            .bind(&n.slot_key)
            .bind(n.document_id)
            .fetch_one(self.db.pool())
            .await?)
    }

    /// 标记生成开始（status=generating, generating_issue_id 写入）
    pub async fn mark_generating(&self, id: Uuid, issue_id: Uuid) -> RepoResult<()> {
        sqlx::query(
            "UPDATE summary_slots SET status='generating', generating_issue_id=$2,              failure_reason=NULL, updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .bind(issue_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 标记生成完成（status=ready, last_generated_at=now()）
    pub async fn mark_ready(
        &self,
        id: Uuid,
        agent_id: Uuid,
        document_id: Uuid,
        model: Option<&str>,
    ) -> RepoResult<()> {
        sqlx::query(
            "UPDATE summary_slots SET status='ready', document_id=$2,              last_generated_at=now(), last_generated_by_agent_id=$3, last_model=$4,              generating_issue_id=NULL, failure_reason=NULL, updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .bind(document_id)
        .bind(agent_id)
        .bind(model)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 标记生成失败（保留 generating_issue_id 以便重试）
    pub async fn mark_failed(&self, id: Uuid, reason: &str) -> RepoResult<()> {
        sqlx::query(
            "UPDATE summary_slots SET status='failed', failure_reason=$2, updated_at=now()              WHERE id=$1",
        )
        .bind(id)
        .bind(reason)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn patch(
        &self,
        id: Uuid,
        p: &SummarySlotPatch,
    ) -> RepoResult<Option<SummarySlotRow>> {
        let sql = format!(
            "UPDATE summary_slots SET                 status = COALESCE($2, status),                 failure_reason = COALESCE($3, failure_reason),                 generating_issue_id = COALESCE($4, generating_issue_id),                 document_id = COALESCE($5, document_id),                 last_model = COALESCE($6, last_model),                 updated_at = now()              WHERE id=$1              RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, SummarySlotRow>(&sql)
            .bind(id)
            .bind(p.status.map(|s| s.as_str()))
            .bind(p.failure_reason.as_deref())
            .bind(p.generating_issue_id)
            .bind(p.document_id)
            .bind(p.last_model.as_deref())
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn delete(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM summary_slots WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// 查找一个 company 下所有 stale 状态的槽位（用于后台 sweep）
    pub async fn list_stale(&self, company_id: Uuid) -> RepoResult<Vec<SummarySlotRow>> {
        let sql =
            format!("SELECT {COLS} FROM summary_slots WHERE company_id=$1 AND status='stale'");
        Ok(sqlx::query_as::<_, SummarySlotRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    // =========================================================================
    // Round 158: summary_slots route 仓储化新增方法
    // =========================================================================

    /// Round 158: 字符串形式 scope_kind + Option<Uuid> scope_id 查找。
    pub async fn find_by_scope_str(
        &self,
        company_id: Uuid,
        scope_kind: &str,
        slot_key: &str,
        scope_id: Option<Uuid>,
    ) -> RepoResult<Option<SummarySlotRow>> {
        let sql = format!(
            "SELECT {COLS} FROM summary_slots \
             WHERE company_id = $1 AND scope_kind = $2 AND slot_key = $3 \
               AND scope_id IS NOT DISTINCT FROM $4"
        );
        Ok(sqlx::query_as::<_, SummarySlotRow>(&sql)
            .bind(company_id)
            .bind(scope_kind)
            .bind(slot_key)
            .bind(scope_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Round 158: INSERT 新的 idle 摘要槽位（ensure_summary_slot 用）。
    pub async fn insert_idle(
        &self,
        company_id: Uuid,
        scope_kind: &str,
        scope_id: Option<Uuid>,
        slot_key: &str,
    ) -> RepoResult<SummarySlotRow> {
        let sql = format!(
            "INSERT INTO summary_slots (company_id, scope_kind, scope_id, slot_key, status) \
             VALUES ($1, $2, $3, $4, 'idle') RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, SummarySlotRow>(&sql)
            .bind(company_id)
            .bind(scope_kind)
            .bind(scope_id)
            .bind(slot_key)
            .fetch_one(self.db.pool())
            .await?)
    }

    /// Round 158: UPDATE summary_slots to generated/idle state + RETURNING。
    pub async fn mark_slot_written(
        &self,
        slot_id: Uuid,
        document_id: Uuid,
        now: chrono::DateTime<chrono::Utc>,
        model: Option<&str>,
    ) -> RepoResult<SummarySlotRow> {
        let sql = format!(
            "UPDATE summary_slots SET document_id = $2, status = 'idle', failure_reason = NULL, \
             generating_issue_id = NULL, last_generated_at = $3, last_model = $4, \
             updated_at = $3 WHERE id = $1 RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, SummarySlotRow>(&sql)
            .bind(slot_id)
            .bind(document_id)
            .bind(now)
            .bind(model)
            .fetch_one(self.db.pool())
            .await?)
    }

    /// Round 158: UPDATE existing slot to generating + RETURNING row。
    pub async fn update_to_generating(
        &self,
        slot_id: Uuid,
        issue_id: Uuid,
    ) -> RepoResult<SummarySlotRow> {
        let sql = format!(
            "UPDATE summary_slots SET status = 'generating', generating_issue_id = $2, \
             updated_at = now() WHERE id = $1 RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, SummarySlotRow>(&sql)
            .bind(slot_id)
            .bind(issue_id)
            .fetch_one(self.db.pool())
            .await?)
    }

    /// Round 158: INSERT 新的 generating 摘要槽位 + RETURNING row。
    pub async fn insert_generating(
        &self,
        company_id: Uuid,
        scope_kind: &str,
        scope_id: Option<Uuid>,
        slot_key: &str,
        issue_id: Uuid,
    ) -> RepoResult<SummarySlotRow> {
        let sql = format!(
            "INSERT INTO summary_slots (company_id, scope_kind, scope_id, slot_key, status, generating_issue_id) \
             VALUES ($1, $2, $3, $4, 'generating', $5) RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, SummarySlotRow>(&sql)
            .bind(company_id)
            .bind(scope_kind)
            .bind(scope_id)
            .bind(slot_key)
            .bind(issue_id)
            .fetch_one(self.db.pool())
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_kind_strings_round_trip() {
        for k in [
            ScopeKind::Company,
            ScopeKind::Agent,
            ScopeKind::Document,
            ScopeKind::Issue,
        ] {
            assert_eq!(ScopeKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ScopeKind::parse("nope"), None);
    }
    #[test]
    fn status_strings() {
        assert_eq!(SummarySlotStatus::Idle.as_str(), "idle");
        assert_eq!(SummarySlotStatus::Generating.as_str(), "generating");
        assert_eq!(SummarySlotStatus::Ready.as_str(), "ready");
        assert_eq!(SummarySlotStatus::Stale.as_str(), "stale");
        assert_eq!(SummarySlotStatus::Failed.as_str(), "failed");
    }
    #[test]
    fn slot_key_strings() {
        assert_eq!(
            SummarySlotKey::WeeklyBoardUpdate.as_str(),
            "weekly_board_update"
        );
        assert_eq!(
            SummarySlotKey::AgentPerformance.as_str(),
            "agent_performance"
        );
    }
}
