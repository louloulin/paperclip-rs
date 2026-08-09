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
    /// Round 153: 从字符串 parse；未匹配返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(Self::Manual),
            "scheduled" => Some(Self::Scheduled),
            "webhook" => Some(Self::Webhook),
            "oauth_test" => Some(Self::OAuthTest),
            _ => None,
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
    /// Round 153: 从字符串 parse；未匹配返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "oauth/authorize" => Some(Self::OauthAuthorize),
            "oauth/token" => Some(Self::OauthToken),
            "oauth/userinfo" => Some(Self::OauthUserinfo),
            "oauth/revoke" => Some(Self::OauthRevoke),
            "services/start" => Some(Self::ServiceStart),
            "services/stop" => Some(Self::ServiceStop),
            "fixtures/install" => Some(Self::FixtureInstall),
            "reset" => Some(Self::Reset),
            "custom" => Some(Self::Custom),
            _ => None,
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
    /// Round 153: 从字符串 parse；未匹配返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "passed" => Some(Self::Passed),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            "running" => Some(Self::Running),
            _ => None,
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
        let rows = qb
            .build_query_as::<RunRow>()
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    pub async fn get(&self, company_id: Uuid, id: Uuid) -> RepoResult<Option<RunRow>> {
        let sql = format!("SELECT {RUN_COLS} FROM smoke_runs WHERE company_id=$1 AND id=$2");
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

    /// R510: PATCH /api/companies/:company_id/smoke-lab/runs/:run_id
    /// Mirrors Node `routes/smoke-lab.ts` patch_run — update status (free-form).
    /// Notes column does not exist in the current schema; if a future migration
    /// adds it, this repo method is the single place to wire it in.
    pub async fn patch_run(
        &self,
        company_id: Uuid,
        run_id: Uuid,
        status: Option<&str>,
    ) -> RepoResult<Option<RunRow>> {
        let sql = format!(
            "UPDATE smoke_runs SET status=COALESCE($3, status), updated_at=now()              WHERE company_id=$1 AND id=$2              RETURNING {RUN_COLS}",
        );
        Ok(sqlx::query_as::<_, RunRow>(&sql)
            .bind(company_id)
            .bind(run_id)
            .bind(status)
            .fetch_optional(self.db.pool())
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

    // ============================================================================
    // Round 153: smoke_lab 仓储扩展（oauth + services + fixtures + reset）
    // ============================================================================

    /// 插入一条 smoke lab oauth code（authorize 路径）。
    pub async fn insert_oauth_code(&self, code: &str, company_id: Uuid) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO smoke_lab_oauth_codes (code, company_id, used, created_at) \
             VALUES ($1, $2, false, now())",
        )
        .bind(code)
        .bind(company_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 消费（标记 used）一个 oauth code；返回 None 表示无效或已用。
    /// 返回 `Some(())` 即兑换成功。
    pub async fn claim_oauth_code(&self, code: &str, company_id: Uuid) -> sqlx::Result<bool> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "UPDATE smoke_lab_oauth_codes SET used = true, used_at = now() \
             WHERE code = $1 AND company_id = $2 AND used = false \
             RETURNING code::text::uuid",
        )
        .bind(code)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.is_some())
    }

    /// 插入一条 smoke lab oauth token（token 路径）。
    pub async fn insert_oauth_token(&self, token: &str, company_id: Uuid) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO smoke_lab_oauth_tokens (token, company_id, expires_at) \
             VALUES ($1, $2, now() + interval '1 hour')",
        )
        .bind(token)
        .bind(company_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 删除 oauth token（revoke 路径）。
    pub async fn delete_oauth_token(&self, token: &str) -> sqlx::Result<u64> {
        let r = sqlx::query("DELETE FROM smoke_lab_oauth_tokens WHERE token = $1")
            .bind(token)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected())
    }

    /// 列出某公司的 smoke lab services。
    pub async fn list_services(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<(String, String, serde_json::Value)>> {
        let rows: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
            "SELECT service_key, status, config FROM smoke_lab_services WHERE company_id = $1",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// 启动/标记 service 为 running（INSERT ON CONFLICT DO UPDATE）。
    pub async fn upsert_service_running(
        &self,
        company_id: Uuid,
        service_key: &str,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO smoke_lab_services (company_id, service_key, status, config, updated_at) \
             VALUES ($1, $2, 'running', '{}'::jsonb, now()) \
             ON CONFLICT (company_id, service_key) DO UPDATE SET status='running', updated_at=now()",
        )
        .bind(company_id)
        .bind(service_key)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 标记 service 为 stopped（按 company_id + service_key）。
    pub async fn stop_service(&self, company_id: Uuid, service_key: &str) -> sqlx::Result<u64> {
        let r = sqlx::query(
            "UPDATE smoke_lab_services SET status = 'stopped', updated_at = now() \
             WHERE company_id = $1 AND service_key = $2",
        )
        .bind(company_id)
        .bind(service_key)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected())
    }

    /// 探测某公司是否已存在。
    pub async fn company_exists(&self, company_id: Uuid) -> sqlx::Result<bool> {
        let v: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM companies WHERE id = $1)")
            .bind(company_id)
            .fetch_one(self.db.pool())
            .await?;
        Ok(v)
    }

    /// 探测某公司下某标题 issue 的数量。
    pub async fn count_issues_with_title(
        &self,
        company_id: Uuid,
        title: &str,
    ) -> sqlx::Result<i64> {
        let v: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM issues WHERE company_id = $1 AND title = $2",
        )
        .bind(company_id)
        .bind(title)
        .fetch_one(self.db.pool())
        .await?;
        Ok(v)
    }

    /// 探测某公司下某名称 agent 的数量。
    pub async fn count_agents_with_name(&self, company_id: Uuid, name: &str) -> sqlx::Result<i64> {
        let v: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM agents WHERE company_id = $1 AND name = $2",
        )
        .bind(company_id)
        .bind(name)
        .fetch_one(self.db.pool())
        .await?;
        Ok(v)
    }

    /// 探测某公司下项目数量。
    pub async fn count_projects(&self, company_id: Uuid) -> sqlx::Result<i64> {
        let v: i64 =
            sqlx::query_scalar("SELECT COUNT(*)::bigint FROM projects WHERE company_id = $1")
                .bind(company_id)
                .fetch_one(self.db.pool())
                .await?;
        Ok(v)
    }

    /// 插入一个 smoke project 占位。
    pub async fn insert_smoke_project(&self, company_id: Uuid, name: &str) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO projects (company_id, name, status) \
             VALUES ($1, $2, 'active')",
        )
        .bind(company_id)
        .bind(name)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 插入一个 smoke agent 占位。
    pub async fn insert_smoke_agent(
        &self,
        company_id: Uuid,
        name: &str,
        role: &str,
        status: &str,
        adapter_type: &str,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO agents (company_id, name, role, status, adapter_type) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(company_id)
        .bind(name)
        .bind(role)
        .bind(status)
        .bind(adapter_type)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 插入一个 smoke issue 占位。
    pub async fn insert_smoke_issue(
        &self,
        company_id: Uuid,
        title: &str,
        priority: &str,
        status: &str,
        origin_kind: &str,
        origin_fingerprint: &str,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO issues (company_id, title, priority, status, origin_kind, origin_fingerprint) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(company_id)
        .bind(title)
        .bind(priority)
        .bind(status)
        .bind(origin_kind)
        .bind(origin_fingerprint)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 插入占位 smoke service（INSERT ON CONFLICT DO NOTHING）。返回是否新插入。
    pub async fn insert_smoke_service_if_absent(
        &self,
        company_id: Uuid,
        service_key: &str,
        status: &str,
        config: serde_json::Value,
    ) -> sqlx::Result<bool> {
        let r = sqlx::query(
            "INSERT INTO smoke_lab_services (company_id, service_key, status, config) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (company_id, service_key) DO NOTHING",
        )
        .bind(company_id)
        .bind(service_key)
        .bind(status)
        .bind(config)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// 插入 fixture company（若 id 已存在则忽略）。
    pub async fn insert_fixture_company(
        &self,
        company_id: Uuid,
        name: &str,
        issue_prefix: &str,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3) \
             ON CONFLICT DO NOTHING",
        )
        .bind(company_id)
        .bind(name)
        .bind(issue_prefix)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 一次性清空某公司所有 smoke lab 数据（oauth + runs + steps + services）。
    /// 顺序：tokens → codes → steps → runs → services，避免 FK 约束失败。
    pub async fn reset_company(&self, company_id: Uuid) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM smoke_lab_oauth_tokens WHERE company_id = $1")
            .bind(company_id)
            .execute(self.db.pool())
            .await?;
        sqlx::query("DELETE FROM smoke_lab_oauth_codes WHERE company_id = $1")
            .bind(company_id)
            .execute(self.db.pool())
            .await?;
        sqlx::query("DELETE FROM smoke_run_steps WHERE company_id = $1")
            .bind(company_id)
            .execute(self.db.pool())
            .await?;
        sqlx::query("DELETE FROM smoke_runs WHERE company_id = $1")
            .bind(company_id)
            .execute(self.db.pool())
            .await?;
        sqlx::query("DELETE FROM smoke_lab_services WHERE company_id = $1")
            .bind(company_id)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_status_round_trip() {
        for s in [
            SmokeRunStatus::Running,
            SmokeRunStatus::Passed,
            SmokeRunStatus::Failed,
            SmokeRunStatus::Cancelled,
        ] {
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
