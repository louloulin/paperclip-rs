//! `smoke_runs` + `smoke_run_steps` 域 — 冒烟测试运行与步骤。
//!
//! 设计：
//! - 与原 paperclip schema (`smoke_runs` / `smoke_run_steps`) 完全等价
//! - `RunRow::from_status` 帮助把字符串状态（"running"/"passed"/"failed"/"cancelled"）
//!   映射到 `SmokeRunStatus` 枚举
//! - `StepsRepo` 提供按 run 聚合查询与按状态过滤

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeRunStatus {
    Running,
    Passed,
    Failed,
    Cancelled,
}
impl SmokeRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "passed" => Some(Self::Passed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeRunTrigger {
    Manual,
    Scheduled,
    Webhook,
    OAuthTest,
}
impl SmokeRunTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
            Self::Webhook => "webhook",
            Self::OAuthTest => "oauth_test",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeStepPath {
    OauthAuthorize,
    OauthToken,
    OauthUserinfo,
    OauthRevoke,
    ServiceStart,
    ServiceStop,
    FixtureInstall,
    Reset,
    Custom,
}
impl SmokeStepPath {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OauthAuthorize => "oauth/authorize",
            Self::OauthToken => "oauth/token",
            Self::OauthUserinfo => "oauth/userinfo",
            Self::OauthRevoke => "oauth/revoke",
            Self::ServiceStart => "services/start",
            Self::ServiceStop => "services/stop",
            Self::FixtureInstall => "fixtures/install",
            Self::Reset => "reset",
            Self::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeStepStatus {
    Passed,
    Failed,
    Skipped,
    Running,
}
impl SmokeStepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Running => "running",
        }
    }
}

const RUN_COLS: &str = "id, company_id, trigger, status, started_at, finished_at, summary,      created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub trigger: String,
    pub status: String,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub summary: Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl RunRow {
    pub fn run_status(&self) -> Option<SmokeRunStatus> {
        SmokeRunStatus::parse(&self.status)
    }
}

const STEP_COLS: &str = "id, company_id, run_id, path, scenario_step, status,      detail, screenshot_artifact_ref, duration_ms, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub run_id: Uuid,
    pub path: String,
    pub scenario_step: String,
    pub status: String,
    pub detail: Option<String>,
    pub screenshot_artifact_ref: Option<Value>,
    pub duration_ms: Option<i32>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewRun {
    pub company_id: Uuid,
    pub trigger: SmokeRunTrigger,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewStep {
    pub company_id: Uuid,
    pub run_id: Uuid,
    pub path: SmokeStepPath,
    pub scenario_step: String,
    pub status: SmokeStepStatus,
    pub detail: Option<String>,
    pub duration_ms: Option<i32>,
    pub screenshot_artifact_ref: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct RunFilter {
    pub status: Option<SmokeRunStatus>,
    pub trigger: Option<SmokeRunTrigger>,
    pub limit: Option<i64>,
}

pub struct SmokeRepo<'a> {
    pub db: &'a Db,
}

impl<'a> SmokeRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    // ---- run ----

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        filter: Option<RunFilter>,
    ) -> RepoResult<Vec<RunRow>> {
        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT id, company_id, trigger, status, started_at, finished_at, summary,              created_at, updated_at FROM smoke_runs WHERE company_id = ",
        );
        qb.push_bind(company_id);
        let filter = filter.unwrap_or_default();
        if let Some(s) = filter.status {
            qb.push(" AND status = ").push_bind(s.as_str());
        }
        if let Some(t) = filter.trigger {
            qb.push(" AND trigger = ").push_bind(t.as_str());
        }
        qb.push(" ORDER BY started_at DESC LIMIT ");
        qb.push_bind(filter.limit.unwrap_or(50));
        let rows = qb.build_query_as::<RunRow>().fetch_all(self.db.pool()).await?;
        Ok(rows)
    }

    pub async fn get(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<RunRow>> {
        let sql = format!(
            "SELECT {RUN_COLS} FROM smoke_runs WHERE company_id=$1 AND id=$2"
        );
        Ok(sqlx::query_as::<_, RunRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create_run(&self, n: &NewRun) -> RepoResult<RunRow> {
        let sql = format!(
            "INSERT INTO smoke_runs (company_id, trigger, status)              VALUES ($1, $2, 'running')              RETURNING {RUN_COLS}",
        );
        Ok(sqlx::query_as::<_, RunRow>(&sql)
            .bind(n.company_id)
            .bind(n.trigger.as_str())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn finish_run(
        &self,
        id: Uuid,
        status: SmokeRunStatus,
        summary: Option<Value>,
    ) -> RepoResult<()> {
        sqlx::query(
            "UPDATE smoke_runs SET status=$2, finished_at=now(), summary=COALESCE($3, summary),              updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .bind(status.as_str())
        .bind(summary)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn delete_run(&self, id: Uuid) -> RepoResult<bool> {
        // 先删步骤
        sqlx::query("DELETE FROM smoke_run_steps WHERE run_id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        let n = sqlx::query("DELETE FROM smoke_runs WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    // ---- step ----

    pub async fn list_steps(&self, run_id: Uuid) -> RepoResult<Vec<StepRow>> {
        let sql = format!(
            "SELECT {STEP_COLS} FROM smoke_run_steps WHERE run_id=$1 ORDER BY created_at ASC"
        );
        Ok(sqlx::query_as::<_, StepRow>(&sql)
            .bind(run_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn add_step(&self, n: &NewStep) -> RepoResult<StepRow> {
        let sql = format!(
            "INSERT INTO smoke_run_steps (company_id, run_id, path, scenario_step, status,                 detail, screenshot_artifact_ref, duration_ms)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8)              RETURNING {STEP_COLS}",
        );
        let row = sqlx::query_as::<_, StepRow>(&sql)
            .bind(n.company_id)
            .bind(n.run_id)
            .bind(n.path.as_str())
            .bind(&n.scenario_step)
            .bind(n.status.as_str())
            .bind(n.detail.as_deref())
            .bind(n.screenshot_artifact_ref.clone())
            .bind(n.duration_ms)
            .fetch_one(self.db.pool())
            .await?;
        // 自动把 run 标记 failed 如果有一步 failed
        if matches!(n.status, SmokeStepStatus::Failed) {
            sqlx::query(
                "UPDATE smoke_runs SET status='failed', updated_at=now() WHERE id=$1 AND status='running'",
            )
            .bind(n.run_id)
            .execute(self.db.pool())
            .await?;
        }
        Ok(row)
    }

    pub async fn latest_step(
        &self,
        run_id: Uuid,
        path: SmokeStepPath,
    ) -> RepoResult<Option<StepRow>> {
        let sql = format!(
            "SELECT {STEP_COLS} FROM smoke_run_steps              WHERE run_id=$1 AND path=$2 ORDER BY created_at DESC LIMIT 1"
        );
        Ok(sqlx::query_as::<_, StepRow>(&sql)
            .bind(run_id)
            .bind(path.as_str())
            .fetch_optional(self.db.pool())
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_status_round_trip() {
        for s in [SmokeRunStatus::Running, SmokeRunStatus::Passed, SmokeRunStatus::Failed, SmokeRunStatus::Cancelled] {
            assert_eq!(SmokeRunStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(SmokeRunStatus::parse("unknown"), None);
    }
    #[test]
    fn trigger_strings() {
        assert_eq!(SmokeRunTrigger::Manual.as_str(), "manual");
        assert_eq!(SmokeRunTrigger::Scheduled.as_str(), "scheduled");
    }
    #[test]
    fn step_path_strings() {
        assert_eq!(SmokeStepPath::OauthAuthorize.as_str(), "oauth/authorize");
        assert_eq!(SmokeStepPath::ServiceStart.as_str(), "services/start");
    }
}
