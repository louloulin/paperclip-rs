//! `heartbeat_runs`、`heartbeat_run_events` 与 watchdog 决策数据访问。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatRunStatus {
    Queued,
    ScheduledRetry,
    Running,
    Succeeded,
    Interrupted,
    Failed,
    Cancelled,
    TimedOut,
}

impl HeartbeatRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::ScheduledRetry => "scheduled_retry",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Interrupted | Self::Failed | Self::Cancelled | Self::TimedOut
        )
    }

    pub fn can_transition_to(self, target: Self) -> bool {
        if self == target {
            return true;
        }
        match self {
            Self::Queued => matches!(
                target,
                Self::ScheduledRetry
                    | Self::Running
                    | Self::Failed
                    | Self::Cancelled
                    | Self::TimedOut
            ),
            Self::ScheduledRetry => matches!(target, Self::Queued | Self::Running | Self::Cancelled),
            Self::Running => matches!(
                target,
                Self::ScheduledRetry
                    | Self::Succeeded
                    | Self::Interrupted
                    | Self::Failed
                    | Self::Cancelled
                    | Self::TimedOut
            ),
            Self::Succeeded
            | Self::Interrupted
            | Self::Failed
            | Self::Cancelled
            | Self::TimedOut => false,
        }
    }

    fn allowed_predecessors(self) -> &'static [&'static str] {
        match self {
            Self::Queued => &["queued", "scheduled_retry"],
            Self::ScheduledRetry => &["queued", "running", "scheduled_retry"],
            Self::Running => &["queued", "scheduled_retry", "running"],
            Self::Succeeded => &["running", "succeeded"],
            Self::Interrupted => &["running", "interrupted"],
            Self::Failed => &["queued", "running", "failed"],
            Self::Cancelled => &["queued", "scheduled_retry", "running", "cancelled"],
            Self::TimedOut => &["queued", "running", "timed_out"],
        }
    }
}

impl std::str::FromStr for HeartbeatRunStatus {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "scheduled_retry" => Ok(Self::ScheduledRetry),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "interrupted" => Ok(Self::Interrupted),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "timed_out" => Ok(Self::TimedOut),
            _ => Err("invalid heartbeat run status"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunLivenessState {
    Completed,
    Advanced,
    PlanOnly,
    EmptyResponse,
    Blocked,
    Failed,
    NeedsFollowup,
}

impl RunLivenessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Advanced => "advanced",
            Self::PlanOnly => "plan_only",
            Self::EmptyResponse => "empty_response",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::NeedsFollowup => "needs_followup",
        }
    }
}

impl std::str::FromStr for RunLivenessState {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "completed" => Ok(Self::Completed),
            "advanced" => Ok(Self::Advanced),
            "plan_only" => Ok(Self::PlanOnly),
            "empty_response" => Ok(Self::EmptyResponse),
            "blocked" => Ok(Self::Blocked),
            "failed" => Ok(Self::Failed),
            "needs_followup" => Ok(Self::NeedsFollowup),
            _ => Err("invalid run liveness state"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchdogDecision {
    Snooze,
    Continue,
    DismissedFalsePositive,
}

impl WatchdogDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snooze => "snooze",
            Self::Continue => "continue",
            Self::DismissedFalsePositive => "dismissed_false_positive",
        }
    }
}

impl std::str::FromStr for WatchdogDecision {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "snooze" => Ok(Self::Snooze),
            "continue" => Ok(Self::Continue),
            "dismissed_false_positive" => Ok(Self::DismissedFalsePositive),
            _ => Err("invalid watchdog decision"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatEventStream {
    System,
    Stdout,
    Stderr,
}

impl HeartbeatEventStream {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatEventLevel {
    Info,
    Warn,
    Error,
}

impl HeartbeatEventLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

const RUN_COLUMNS: &str = "id, company_id, agent_id, invocation_source, trigger_detail, status, \
responsible_user_id, started_at, finished_at, error, wakeup_request_id, exit_code, signal, \
usage_json, result_json, session_id_before, session_id_after, log_store, log_ref, log_bytes, \
log_sha256, log_compressed, stdout_excerpt, stderr_excerpt, error_code, external_run_id, \
process_pid, process_group_id, process_started_at, last_output_at, last_output_seq, \
last_output_stream, last_output_bytes, retry_of_run_id, process_loss_retry_count, \
scheduled_retry_at, scheduled_retry_attempt, scheduled_retry_reason, issue_comment_status, \
issue_comment_satisfied_by_comment_id, issue_comment_retry_queued_at, liveness_state, \
liveness_reason, continuation_attempt, last_useful_action_at, next_action, context_snapshot, \
created_at, updated_at";

const EVENT_COLUMNS: &str = "id, company_id, run_id, agent_id, seq, event_type, stream, level, \
color, message, payload, created_at";

const WATCHDOG_COLUMNS: &str = "id, company_id, run_id, evaluation_issue_id, decision, \
snoozed_until, reason, created_by_agent_id, created_by_user_id, created_by_run_id, created_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq)]
pub struct HeartbeatRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub invocation_source: String,
    pub trigger_detail: Option<String>,
    pub status: String,
    pub responsible_user_id: Option<String>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub error: Option<String>,
    pub wakeup_request_id: Option<Uuid>,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub usage_json: Option<serde_json::Value>,
    pub result_json: Option<serde_json::Value>,
    pub session_id_before: Option<String>,
    pub session_id_after: Option<String>,
    pub log_store: Option<String>,
    pub log_ref: Option<String>,
    pub log_bytes: Option<i64>,
    pub log_sha256: Option<String>,
    pub log_compressed: bool,
    pub stdout_excerpt: Option<String>,
    pub stderr_excerpt: Option<String>,
    pub error_code: Option<String>,
    pub external_run_id: Option<String>,
    pub process_pid: Option<i32>,
    pub process_group_id: Option<i32>,
    pub process_started_at: Option<Timestamp>,
    pub last_output_at: Option<Timestamp>,
    pub last_output_seq: i32,
    pub last_output_stream: Option<String>,
    pub last_output_bytes: Option<i64>,
    pub retry_of_run_id: Option<Uuid>,
    pub process_loss_retry_count: i32,
    pub scheduled_retry_at: Option<Timestamp>,
    pub scheduled_retry_attempt: i32,
    pub scheduled_retry_reason: Option<String>,
    pub issue_comment_status: String,
    pub issue_comment_satisfied_by_comment_id: Option<Uuid>,
    pub issue_comment_retry_queued_at: Option<Timestamp>,
    pub liveness_state: Option<String>,
    pub liveness_reason: Option<String>,
    pub continuation_attempt: i32,
    pub last_useful_action_at: Option<Timestamp>,
    pub next_action: Option<String>,
    pub context_snapshot: Option<serde_json::Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl HeartbeatRow {
    pub fn run_status(&self) -> Option<HeartbeatRunStatus> {
        self.status.parse().ok()
    }

    pub fn liveness(&self) -> Option<RunLivenessState> {
        self.liveness_state
            .as_deref()
            .and_then(|value| value.parse().ok())
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq)]
pub struct HeartbeatEventRow {
    pub id: i64,
    pub company_id: Uuid,
    pub run_id: Uuid,
    pub agent_id: Uuid,
    pub seq: i32,
    pub event_type: String,
    pub stream: Option<String>,
    pub level: Option<String>,
    pub color: Option<String>,
    pub message: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatWatchdogDecisionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub run_id: Uuid,
    pub evaluation_issue_id: Option<Uuid>,
    pub decision: String,
    pub snoozed_until: Option<Timestamp>,
    pub reason: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
    pub created_at: Timestamp,
}

pub struct CreateHeartbeat<'a> {
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub invocation_source: &'a str,
    pub trigger_detail: Option<&'a str>,
    pub responsible_user_id: Option<&'a str>,
    pub wakeup_request_id: Option<Uuid>,
    pub context_snapshot: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct NewHeartbeatEvent {
    pub event_type: String,
    pub stream: Option<HeartbeatEventStream>,
    pub level: Option<HeartbeatEventLevel>,
    pub color: Option<String>,
    pub message: Option<String>,
    pub payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct NewWatchdogDecision {
    pub company_id: Uuid,
    pub run_id: Uuid,
    pub evaluation_issue_id: Option<Uuid>,
    pub decision: WatchdogDecision,
    pub snoozed_until: Option<Timestamp>,
    pub reason: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct HeartbeatRunFilter {
    pub agent_id: Option<Uuid>,
    pub statuses: Vec<HeartbeatRunStatus>,
    pub responsible_user_id: Option<String>,
    pub limit: Option<i64>,
}

pub struct HeartbeatRepo<'a> {
    pub db: &'a Db,
}

impl<'a> HeartbeatRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn create(&self, input: CreateHeartbeat<'_>) -> sqlx::Result<HeartbeatRow> {
        let query = format!(
            "INSERT INTO heartbeat_runs \
             (company_id, agent_id, invocation_source, trigger_detail, responsible_user_id, \
              wakeup_request_id, context_snapshot) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(input.company_id)
            .bind(input.agent_id)
            .bind(input.invocation_source)
            .bind(input.trigger_detail)
            .bind(input.responsible_user_id)
            .bind(input.wakeup_request_id)
            .bind(input.context_snapshot)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn get(&self, run_id: Uuid) -> sqlx::Result<Option<HeartbeatRow>> {
        let query = format!("SELECT {RUN_COLUMNS} FROM heartbeat_runs WHERE id=$1");
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(run_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn get_for_company(
        &self,
        company_id: Uuid,
        run_id: Uuid,
    ) -> sqlx::Result<Option<HeartbeatRow>> {
        let query = format!(
            "SELECT {RUN_COLUMNS} FROM heartbeat_runs WHERE company_id=$1 AND id=$2"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(company_id)
            .bind(run_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn list_for_agent(&self, agent_id: Uuid) -> sqlx::Result<Vec<HeartbeatRow>> {
        let query = format!(
            "SELECT {RUN_COLUMNS} FROM heartbeat_runs WHERE agent_id=$1 ORDER BY created_at DESC"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(agent_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn list_for_company(
        &self,
        company_id: Uuid,
        filter: &HeartbeatRunFilter,
    ) -> sqlx::Result<Vec<HeartbeatRow>> {
        let mut query = sqlx::QueryBuilder::<sqlx::Postgres>::new(format!(
            "SELECT {RUN_COLUMNS} FROM heartbeat_runs WHERE company_id="
        ));
        query.push_bind(company_id);
        if let Some(agent_id) = filter.agent_id {
            query.push(" AND agent_id=").push_bind(agent_id);
        }
        if !filter.statuses.is_empty() {
            let statuses: Vec<String> = filter
                .statuses
                .iter()
                .map(|status| status.as_str().to_owned())
                .collect();
            query
                .push(" AND status=ANY(")
                .push_bind(statuses)
                .push("::text[])");
        }
        if let Some(responsible_user_id) = filter.responsible_user_id.as_deref() {
            query
                .push(" AND responsible_user_id=")
                .push_bind(responsible_user_id);
        }
        query
            .push(" ORDER BY created_at DESC, id DESC LIMIT ")
            .push_bind(filter.limit.unwrap_or(200).clamp(1, 1_000));
        query
            .build_query_as::<HeartbeatRow>()
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn list_recoverable(&self, limit: i64) -> sqlx::Result<Vec<HeartbeatRow>> {
        let query = format!(
            "SELECT {RUN_COLUMNS} FROM heartbeat_runs \
             WHERE status IN ('queued','scheduled_retry','running') \
             ORDER BY created_at ASC LIMIT $1"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(limit.clamp(1, 10_000))
            .fetch_all(self.db.pool())
            .await
    }

/// Round 107: 查某个 issue 当前是否还有活跃 heartbeat run
    /// (status in queued/claimed/running/paused)。通常用于前端 polling。
    pub async fn find_active_run_by_issue(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM heartbeat_runs              WHERE context_snapshot->>'issueId' = $1              AND status::text IN ('queued','claimed','running','paused')              ORDER BY started_at DESC NULLS LAST LIMIT 1",
        )
        .bind(issue_id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(id,)| id))
    }

    /// Round 107: 列出某个 issue 的所有 heartbeat_runs（按 started_at DESC）。
    pub async fn list_runs_by_issue(
        &self,
        issue_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<HeartbeatRow>> {
        let query = format!(
            "SELECT {RUN_COLUMNS} FROM heartbeat_runs              WHERE context_snapshot->>'issueId' = $1              ORDER BY started_at DESC NULLS LAST LIMIT $2"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(issue_id.to_string())
            .bind(limit.clamp(1, 500))
            .fetch_all(self.db.pool())
            .await
    }

    /// Round 137: 按 id 取单条 run（含 context_snapshot）。
    /// 返回完整 10 列元组供 get_issue_run 路由使用。
    pub async fn get_run_with_context(
        &self,
        run_id: Uuid,
    ) -> sqlx::Result<
        Option<(
            Uuid,
            Uuid,
            Uuid,
            String,
            String,
            Option<pc_core::Timestamp>,
            Option<pc_core::Timestamp>,
            Option<pc_core::Timestamp>,
            Option<String>,
            serde_json::Value,
        )>,
    > {
        sqlx::query_as(
            "SELECT id, company_id, agent_id, status, invocation_source,                 started_at, finished_at, created_at, error, context_snapshot              FROM heartbeat_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 137: 取消指定 run（幂等：仅当 status IN queued/running 时更新）。
    /// 返回 rows_affected > 0 表示实际取消。
    pub async fn cancel_run_for_issue(
        &self,
        run_id: Uuid,
        issue_id: Uuid,
    ) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "UPDATE heartbeat_runs              SET status = 'cancelled', finished_at = now(), updated_at = now()              WHERE id = $1                AND context_snapshot ->> 'issueId' = $2::text                AND status IN ('queued','running')",
        )
        .bind(run_id)
        .bind(issue_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 137: 取 run 的 agent_id + context_snapshot（供 restart 用）。
    pub async fn get_agent_and_context(
        &self,
        run_id: Uuid,
    ) -> sqlx::Result<Option<(Uuid, serde_json::Value)>> {
        sqlx::query_as(
            "SELECT agent_id, context_snapshot FROM heartbeat_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 137: 插入新 run（INSERT 复合）。
    pub async fn insert_queued_run(
        &self,
        run_id: Uuid,
        company_id: Uuid,
        agent_id: Uuid,
        context_snapshot: &serde_json::Value,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, context_snapshot)              VALUES ($1, $2, $3, 'on_demand', 'queued', $4)",
        )
        .bind(run_id)
        .bind(company_id)
        .bind(agent_id)
        .bind(context_snapshot)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        agent_id: Option<Uuid>,
        limit: i64,
    ) -> sqlx::Result<Vec<HeartbeatRow>> {
        self.list_for_company(
            company_id,
            &HeartbeatRunFilter {
                agent_id,
                limit: Some(limit),
                ..Default::default()
            },
        )
        .await
    }

    pub async fn count_running_for_agent(&self, agent_id: Uuid) -> sqlx::Result<i64> {
        sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM heartbeat_runs \
             WHERE agent_id=$1 AND status='running'",
        )
        .bind(agent_id)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn count_started_today_for_agent(&self, agent_id: Uuid) -> sqlx::Result<i64> {
        sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM heartbeat_runs \
             WHERE agent_id=$1 AND started_at >= (date_trunc('day', timezone('UTC', now())) AT TIME ZONE 'UTC') \
               AND started_at < ((date_trunc('day', timezone('UTC', now())) + interval '1 day') AT TIME ZONE 'UTC') \
               AND status NOT IN ('queued','scheduled_retry')",
        )
        .bind(agent_id)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn claim_for_agent_with_limit(
        &self,
        run: &HeartbeatRow,
        max_concurrent: i64,
    ) -> sqlx::Result<Option<HeartbeatRow>> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(run.agent_id.to_string())
            .execute(&mut *tx)
            .await?;
        let running: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM heartbeat_runs \
             WHERE agent_id=$1 AND status='running'",
        )
        .bind(run.agent_id)
        .fetch_one(&mut *tx)
        .await?;
        if running >= max_concurrent {
            tx.rollback().await?;
            return Ok(None);
        }
        let query = format!(
            "UPDATE heartbeat_runs SET status='running', \
             responsible_user_id=COALESCE($3,responsible_user_id), process_pid=$4, process_group_id=$5, \
             process_started_at=CASE WHEN $4 IS NOT NULL THEN COALESCE(process_started_at,now()) ELSE process_started_at END, \
             started_at=COALESCE(started_at,now()), scheduled_retry_at=NULL, updated_at=now() \
             WHERE company_id=$1 AND id=$2 AND status IN ('queued','scheduled_retry') \
             RETURNING {RUN_COLUMNS}"
        );
        let claimed = sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(run.company_id)
            .bind(run.id)
            .bind(run.responsible_user_id.as_deref())
            .bind(run.process_pid)
            .bind(run.process_group_id)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(claimed)
    }

    pub async fn promote_due_scheduled_retry(
        &self,
        run_id: Uuid,
    ) -> sqlx::Result<Option<HeartbeatRow>> {
        let query = format!(
            "UPDATE heartbeat_runs SET status='queued', updated_at=now() \
             WHERE id=$1 AND status='scheduled_retry' \
               AND scheduled_retry_at IS NOT NULL AND scheduled_retry_at <= now() \
             RETURNING {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(run_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn create_scheduled_retry(
        &self,
        run: &HeartbeatRow,
        due_at: Timestamp,
        attempt: i32,
        reason: &str,
    ) -> sqlx::Result<HeartbeatRow> {
        let query = format!(
            "INSERT INTO heartbeat_runs (company_id, agent_id, invocation_source, trigger_detail, \
             status, responsible_user_id, context_snapshot, retry_of_run_id, scheduled_retry_at, \
             scheduled_retry_attempt, scheduled_retry_reason, issue_comment_status, continuation_attempt) \
             VALUES ($1,$2,$3,$4,'scheduled_retry',$5,$6,$7,$8,$9,$10,'not_applicable',$11) \
             RETURNING {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(run.company_id)
            .bind(run.agent_id)
            .bind(&run.invocation_source)
            .bind(run.trigger_detail.as_deref())
            .bind(run.responsible_user_id.as_deref())
            .bind(run.context_snapshot.clone())
            .bind(run.id)
            .bind(due_at)
            .bind(attempt)
            .bind(reason)
            .bind(run.continuation_attempt)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn mark_running(&self, run_id: Uuid) -> sqlx::Result<Option<HeartbeatRow>> {
        let query = format!(
            "UPDATE heartbeat_runs SET status='running', started_at=COALESCE(started_at,now()), \
             scheduled_retry_at=NULL, updated_at=now() WHERE id=$1 \
             AND status IN ('queued','scheduled_retry') RETURNING {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(run_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn claim_for_company(
        &self,
        company_id: Uuid,
        run_id: Uuid,
        responsible_user_id: Option<&str>,
        process_pid: Option<i32>,
        process_group_id: Option<i32>,
    ) -> sqlx::Result<Option<HeartbeatRow>> {
        let query = format!(
            "UPDATE heartbeat_runs SET status='running', responsible_user_id=COALESCE($3,responsible_user_id), \
             process_pid=$4, process_group_id=$5, process_started_at=CASE WHEN $4 IS NOT NULL \
             THEN COALESCE(process_started_at,now()) ELSE process_started_at END, \
             started_at=COALESCE(started_at,now()), scheduled_retry_at=NULL, updated_at=now() \
             WHERE company_id=$1 AND id=$2 AND status IN ('queued','scheduled_retry') \
             RETURNING {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(company_id)
            .bind(run_id)
            .bind(responsible_user_id)
            .bind(process_pid)
            .bind(process_group_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn transition_status(
        &self,
        company_id: Uuid,
        run_id: Uuid,
        target: HeartbeatRunStatus,
        error: Option<&str>,
        error_code: Option<&str>,
    ) -> sqlx::Result<Option<HeartbeatRow>> {
        let predecessors: Vec<String> = target
            .allowed_predecessors()
            .iter()
            .map(|status| (*status).to_owned())
            .collect();
        let query = format!(
            "UPDATE heartbeat_runs SET status=$3, error=$4, error_code=$5, \
             started_at=CASE WHEN $3='running' THEN COALESCE(started_at,now()) ELSE started_at END, \
             finished_at=CASE WHEN $3 IN ('succeeded','interrupted','failed','cancelled','timed_out') \
                THEN COALESCE(finished_at,now()) ELSE NULL END, updated_at=now() \
             WHERE company_id=$1 AND id=$2 AND status=ANY($6::text[]) RETURNING {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(company_id)
            .bind(run_id)
            .bind(target.as_str())
            .bind(error)
            .bind(error_code)
            .bind(predecessors)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn finish(
        &self,
        run_id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> sqlx::Result<Option<HeartbeatRow>> {
        let target: HeartbeatRunStatus = status
            .parse()
            .map_err(|message: &'static str| sqlx::Error::Protocol(message.into()))?;
        if !target.is_terminal() {
            return Err(sqlx::Error::Protocol(
                "heartbeat finish status must be terminal".into(),
            ));
        }
        let predecessors: Vec<String> = target
            .allowed_predecessors()
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        let query = format!(
            "UPDATE heartbeat_runs SET status=$2, error=$3, finished_at=COALESCE(finished_at,now()), \
             updated_at=now() WHERE id=$1 AND status=ANY($4::text[]) RETURNING {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(run_id)
            .bind(target.as_str())
            .bind(error)
            .bind(predecessors)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn append_event(
        &self,
        run: &HeartbeatRow,
        event_type: &str,
        message: Option<&str>,
        payload: Option<serde_json::Value>,
    ) -> sqlx::Result<HeartbeatEventRow> {
        self.append_event_full(
            run,
            NewHeartbeatEvent {
                event_type: event_type.to_owned(),
                stream: None,
                level: None,
                color: None,
                message: message.map(ToOwned::to_owned),
                payload,
            },
            false,
        )
        .await
    }

    pub async fn append_event_full(
        &self,
        run: &HeartbeatRow,
        event: NewHeartbeatEvent,
        update_last_output: bool,
    ) -> sqlx::Result<HeartbeatEventRow> {
        let mut transaction = self.db.pool().begin().await?;
        let locked: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM heartbeat_runs WHERE company_id=$1 AND id=$2 FOR UPDATE",
        )
        .bind(run.company_id)
        .bind(run.id)
        .fetch_optional(&mut *transaction)
        .await?;
        if locked.is_none() {
            return Err(sqlx::Error::RowNotFound);
        }
        let next_seq: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(seq),0)+1 FROM heartbeat_run_events WHERE run_id=$1",
        )
        .bind(run.id)
        .fetch_one(&mut *transaction)
        .await?;
        let query = format!(
            "INSERT INTO heartbeat_run_events \
             (company_id,run_id,agent_id,seq,event_type,stream,level,color,message,payload) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING {EVENT_COLUMNS}"
        );
        let row = sqlx::query_as::<_, HeartbeatEventRow>(&query)
            .bind(run.company_id)
            .bind(run.id)
            .bind(run.agent_id)
            .bind(next_seq)
            .bind(event.event_type)
            .bind(event.stream.map(HeartbeatEventStream::as_str))
            .bind(event.level.map(HeartbeatEventLevel::as_str))
            .bind(event.color)
            .bind(&event.message)
            .bind(event.payload)
            .fetch_one(&mut *transaction)
            .await?;
        if update_last_output {
            let message_bytes = event
                .message
                .as_deref()
                .map_or(0_i64, |text| i64::try_from(text.len()).unwrap_or(i64::MAX));
            sqlx::query(
                "UPDATE heartbeat_runs SET last_output_at=now(), last_output_seq=$2, \
                 last_output_stream=$3, last_output_bytes=$4, updated_at=now() \
                 WHERE company_id=$1 AND id=$5",
            )
            .bind(run.company_id)
            .bind(row.seq)
            .bind(&row.stream)
            .bind(message_bytes)
            .bind(run.id)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(row)
    }

    pub async fn record_execution_event(
        &self,
        run: &HeartbeatRow,
        _sequence: i32,
        event_type: &str,
        stream: Option<&str>,
        message: Option<&str>,
        payload: Option<serde_json::Value>,
    ) -> sqlx::Result<HeartbeatEventRow> {
        let parsed_stream = match stream {
            Some("system") => Some(HeartbeatEventStream::System),
            Some("stdout") => Some(HeartbeatEventStream::Stdout),
            Some("stderr") => Some(HeartbeatEventStream::Stderr),
            Some(_) => {
                return Err(sqlx::Error::Protocol(
                    "invalid heartbeat event stream".into(),
                ))
            }
            None => None,
        };
        self.append_event_full(
            run,
            NewHeartbeatEvent {
                event_type: event_type.to_owned(),
                stream: parsed_stream,
                level: None,
                color: None,
                message: message.map(ToOwned::to_owned),
                payload,
            },
            true,
        )
        .await
    }

    pub async fn finish_execution(
        &self,
        run_id: Uuid,
        status: &str,
        error: Option<&str>,
        result: Option<&pc_adapter_api::AdapterExecutionResult>,
    ) -> sqlx::Result<Option<HeartbeatRow>> {
        let target: HeartbeatRunStatus = status
            .parse()
            .map_err(|message: &'static str| sqlx::Error::Protocol(message.into()))?;
        if !target.is_terminal() {
            return Err(sqlx::Error::Protocol(
                "execution result status must be terminal".into(),
            ));
        }
        let usage = result
            .and_then(|result| result.usage.as_ref())
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
        let result_json = result.and_then(|result| result.result_json.clone());
        let predecessors: Vec<String> = target
            .allowed_predecessors()
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        let query = format!(
            "UPDATE heartbeat_runs SET status=$2, error=$3, exit_code=$4, signal=$5, \
             usage_json=$6, result_json=$7, session_id_after=$8, error_code=$9, \
             finished_at=COALESCE(finished_at,now()), updated_at=now() \
             WHERE id=$1 AND status=ANY($10::text[]) RETURNING {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(run_id)
            .bind(target.as_str())
            .bind(error)
            .bind(result.and_then(|result| result.exit_code))
            .bind(result.and_then(|result| result.signal.as_deref()))
            .bind(usage)
            .bind(result_json)
            .bind(result.and_then(|result| result.session_id.as_deref()))
            .bind(result.and_then(|result| result.error_code.as_deref()))
            .bind(predecessors)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn list_events(
        &self,
        run_id: Uuid,
        after_seq: i32,
        limit: i64,
    ) -> sqlx::Result<Vec<HeartbeatEventRow>> {
        let query = format!(
            "SELECT {EVENT_COLUMNS} FROM heartbeat_run_events WHERE run_id=$1 AND seq>$2 \
             ORDER BY seq ASC LIMIT $3"
        );
        sqlx::query_as::<_, HeartbeatEventRow>(&query)
            .bind(run_id)
            .bind(after_seq.max(0))
            .bind(limit.clamp(1, 1_000))
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn list_events_for_company(
        &self,
        company_id: Uuid,
        run_id: Uuid,
        after_seq: i32,
        limit: i64,
    ) -> sqlx::Result<Vec<HeartbeatEventRow>> {
        let query = format!(
            "SELECT {EVENT_COLUMNS} FROM heartbeat_run_events \
             WHERE company_id=$1 AND run_id=$2 AND seq>$3 ORDER BY seq ASC LIMIT $4"
        );
        sqlx::query_as::<_, HeartbeatEventRow>(&query)
            .bind(company_id)
            .bind(run_id)
            .bind(after_seq.max(0))
            .bind(limit.clamp(1, 1_000))
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn record_watchdog_decision(
        &self,
        input: NewWatchdogDecision,
    ) -> sqlx::Result<HeartbeatWatchdogDecisionRow> {
        if input.decision == WatchdogDecision::Snooze && input.snoozed_until.is_none() {
            return Err(sqlx::Error::Protocol(
                "snooze watchdog decision requires snoozed_until".into(),
            ));
        }
        let query = format!(
            "INSERT INTO heartbeat_run_watchdog_decisions \
             (company_id,run_id,evaluation_issue_id,decision,snoozed_until,reason, \
              created_by_agent_id,created_by_user_id,created_by_run_id) \
             SELECT hr.company_id,hr.id,$3,$4, \
                CASE WHEN $4='continue' THEN COALESCE($5,now()+interval '30 minutes') \
                     WHEN $4='snooze' THEN $5 ELSE NULL END, $6,$7,$8,$9 \
             FROM heartbeat_runs hr WHERE hr.company_id=$1 AND hr.id=$2 \
             RETURNING {WATCHDOG_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatWatchdogDecisionRow>(&query)
            .bind(input.company_id)
            .bind(input.run_id)
            .bind(input.evaluation_issue_id)
            .bind(input.decision.as_str())
            .bind(input.snoozed_until)
            .bind(input.reason)
            .bind(input.created_by_agent_id)
            .bind(input.created_by_user_id)
            .bind(input.created_by_run_id)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn list_watchdog_decisions(
        &self,
        company_id: Uuid,
        run_id: Uuid,
    ) -> sqlx::Result<Vec<HeartbeatWatchdogDecisionRow>> {
        let query = format!(
            "SELECT {WATCHDOG_COLUMNS} FROM heartbeat_run_watchdog_decisions \
             WHERE company_id=$1 AND run_id=$2 ORDER BY created_at DESC, id DESC"
        );
        sqlx::query_as::<_, HeartbeatWatchdogDecisionRow>(&query)
            .bind(company_id)
            .bind(run_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn active_watchdog_snooze(
        &self,
        company_id: Uuid,
        run_id: Uuid,
    ) -> sqlx::Result<Option<HeartbeatWatchdogDecisionRow>> {
        let query = format!(
            "SELECT {WATCHDOG_COLUMNS} FROM heartbeat_run_watchdog_decisions \
             WHERE company_id=$1 AND run_id=$2 AND decision IN ('snooze','continue') \
             AND snoozed_until>now() ORDER BY created_at DESC, id DESC LIMIT 1"
        );
        sqlx::query_as::<_, HeartbeatWatchdogDecisionRow>(&query)
            .bind(company_id)
            .bind(run_id)
            .fetch_optional(self.db.pool())
            .await
    }

    // =========================================================================
    // Round 161: issues.rs 仓储化新增方法
    // =========================================================================

    /// Round 161: issue_heartbeat_context — issue 最近 N 次 heartbeat_runs（started_at DESC NULLS LAST）。
    pub async fn recent_runs_for_issue(
        &self,
        issue_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<(Uuid, String, Option<pc_core::Timestamp>)>> {
        let rows: Vec<(Uuid, String, Option<pc_core::Timestamp>)> = sqlx::query_as(
            "SELECT id, status::text, started_at FROM heartbeat_runs \
             WHERE context_snapshot->>'issueId' = $1 \
             ORDER BY started_at DESC NULLS LAST LIMIT $2",
        )
        .bind(issue_id.to_string())
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
        .unwrap_or_default();
        Ok(rows)
    }

    /// Round 161: preview_tree_control — count active heartbeat_runs for issue。
    pub async fn count_active_runs_for_issue(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<i64> {
        let v: Option<(i64,)> = sqlx::query_as(
            "SELECT COUNT(*) FROM heartbeat_runs \
             WHERE issue_id = $1 AND status IN ('pending','in_progress')",
        )
        .bind(issue_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(v.map(|(c,)| c).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_contract_values_round_trip() {
        for status in [
            HeartbeatRunStatus::Queued,
            HeartbeatRunStatus::ScheduledRetry,
            HeartbeatRunStatus::Running,
            HeartbeatRunStatus::Succeeded,
            HeartbeatRunStatus::Interrupted,
            HeartbeatRunStatus::Failed,
            HeartbeatRunStatus::Cancelled,
            HeartbeatRunStatus::TimedOut,
        ] {
            assert_eq!(status.as_str().parse(), Ok(status));
        }
        for state in [
            RunLivenessState::Completed,
            RunLivenessState::Advanced,
            RunLivenessState::PlanOnly,
            RunLivenessState::EmptyResponse,
            RunLivenessState::Blocked,
            RunLivenessState::Failed,
            RunLivenessState::NeedsFollowup,
        ] {
            assert_eq!(state.as_str().parse(), Ok(state));
        }
        for decision in [
            WatchdogDecision::Snooze,
            WatchdogDecision::Continue,
            WatchdogDecision::DismissedFalsePositive,
        ] {
            assert_eq!(decision.as_str().parse(), Ok(decision));
        }
    }

    #[test]
    fn heartbeat_terminal_states_cannot_restart() {
        assert!(HeartbeatRunStatus::Queued.can_transition_to(HeartbeatRunStatus::Running));
        assert!(HeartbeatRunStatus::ScheduledRetry.can_transition_to(HeartbeatRunStatus::Queued));
        assert!(HeartbeatRunStatus::Running.can_transition_to(HeartbeatRunStatus::Succeeded));
        for terminal in [
            HeartbeatRunStatus::Succeeded,
            HeartbeatRunStatus::Interrupted,
            HeartbeatRunStatus::Failed,
            HeartbeatRunStatus::Cancelled,
            HeartbeatRunStatus::TimedOut,
        ] {
            assert!(terminal.is_terminal());
            assert!(!terminal.can_transition_to(HeartbeatRunStatus::Running));
            assert!(terminal.can_transition_to(terminal));
        }
    }

    #[test]
    fn queued_run_serializes_nullable_runtime_fields() {
        let now = Timestamp::now();
        let row = HeartbeatRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            invocation_source: "on_demand".into(),
            trigger_detail: Some("manual".into()),
            status: "queued".into(),
            responsible_user_id: None,
            started_at: None,
            finished_at: None,
            error: None,
            wakeup_request_id: None,
            exit_code: None,
            signal: None,
            usage_json: None,
            result_json: None,
            session_id_before: None,
            session_id_after: None,
            log_store: None,
            log_ref: None,
            log_bytes: None,
            log_sha256: None,
            log_compressed: false,
            stdout_excerpt: None,
            stderr_excerpt: None,
            error_code: None,
            external_run_id: None,
            process_pid: None,
            process_group_id: None,
            process_started_at: None,
            last_output_at: None,
            last_output_seq: 0,
            last_output_stream: None,
            last_output_bytes: None,
            retry_of_run_id: None,
            process_loss_retry_count: 0,
            scheduled_retry_at: None,
            scheduled_retry_attempt: 0,
            scheduled_retry_reason: None,
            issue_comment_status: "not_applicable".into(),
            issue_comment_satisfied_by_comment_id: None,
            issue_comment_retry_queued_at: None,
            liveness_state: None,
            liveness_reason: None,
            continuation_attempt: 0,
            last_useful_action_at: None,
            next_action: None,
            context_snapshot: None,
            created_at: now,
            updated_at: now,
        };

        let value = serde_json::to_value(row).unwrap();
        assert_eq!(value["status"], "queued");
        assert!(value["started_at"].is_null());
        assert!(value["finished_at"].is_null());
        assert!(value["session_id_before"].is_null());
        assert_eq!(value["issue_comment_status"], "not_applicable");
    }
}
