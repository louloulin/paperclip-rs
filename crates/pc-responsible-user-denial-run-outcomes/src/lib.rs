//! 记录 responsible-user denial 编码到 active heartbeat run + 发布 live event。
//!
//! 对齐 Node `services/responsible-user-denial-run-outcomes.ts`：
//! - `normalizeResponsibleUserDenialCode` 复用 `pc-responsible-user-denial`
//! - `recordResponsibleUserDenialOnActiveRun`:
//!   1. 校验 runId 非空 + code 是已知 denial code
//!   2. `UPDATE heartbeat_runs SET error_code = $code WHERE id = runId AND status IN ('queued','running')`
//!      + 可选 `agent_id` / `company_id` 过滤（避免误更新其他 tenant）
//!   3. UPDATE 成功后发布 `heartbeat.run.status` live event 并返回更新行

use chrono::{DateTime, Utc};
use pc_live_events::{publish_live_event, LiveEventPayload, LiveEventType};
use pc_repos::Db;
use pc_responsible_user_denial::{
    is_valid_code, normalize_responsible_user_denial_code_value, ResponsibleUserDenialCode,
};
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;

/// `recordResponsibleUserDenialOnActiveRun` 输入。
#[derive(Debug, Clone, Default)]
pub struct RecordDenialInput {
    pub run_id: Option<String>,
    pub agent_id: Option<String>,
    pub company_id: Option<String>,
    /// `serde_json::Value` 便于上游传入 `code`（接受 string 或其他）。
    pub code: Option<Value>,
}

/// `heartbeat_runs` 更新后返回的列。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRunOutcome {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub status: String,
    pub invocation_source: String,
    pub trigger_detail: Option<String>,
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum DenialRecordError {
    #[error("postgres error: {0}")]
    Postgres(#[from] sqlx::Error),
}

/// `normalizeResponsibleUserDenialCode` — 与 Node 1:1 对齐。
pub fn normalize_responsible_user_denial_code(
    value: &Value,
) -> Option<ResponsibleUserDenialCode> {
    normalize_responsible_user_denial_code_value(value)
}

/// 复用 `pc-responsible-user-denial::is_valid_code`（re-export）。
pub fn is_responsible_user_denial_code(code: &str) -> bool {
    is_valid_code(code)
}

/// 记录 denial code 到 active heartbeat run。
pub async fn record_responsible_user_denial_on_active_run(
    db: &Db,
    input: RecordDenialInput,
) -> Result<Option<ActiveRunOutcome>, DenialRecordError> {
    let run_id_str = input.run_id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let code = input
        .code
        .as_ref()
        .and_then(normalize_responsible_user_denial_code_value);
    let (Some(run_id), Some(code)) = (run_id_str, code) else {
        return Ok(None);
    };
    let run_uuid = match Uuid::parse_str(run_id) {
        Ok(u) => u,
        Err(_) => return Ok(None),
    };
    let agent_uuid = input
        .agent_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s.trim()).ok());
    let company_uuid = input
        .company_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s.trim()).ok());

    // 固定参数顺序: $1=run_id, $2=status[], $3=error_code, $4=agent_id?, $5=company_id?
    let mut sql = String::from(
        "UPDATE heartbeat_runs SET error_code = $3, updated_at = now() \
         WHERE id = $1 AND status = ANY($2)",
    );
    if agent_uuid.is_some() {
        sql.push_str(" AND agent_id = $4");
    }
    if company_uuid.is_some() {
        sql.push_str(" AND company_id = $5");
    }
    sql.push_str(
        " RETURNING id, company_id, agent_id, status, invocation_source, trigger_detail, \
         error, error_code, started_at, finished_at, updated_at",
    );

    let mut q = sqlx::query(&sql)
        .bind(run_uuid)
        .bind(Vec::<&str>::from(["queued", "running"]))
        .bind(code.as_str().to_string());
    if let Some(a) = agent_uuid {
        q = q.bind(a);
    }
    if let Some(c) = company_uuid {
        q = q.bind(c);
    }
    let row = q.fetch_optional(db.pool()).await?;
    let Some(row) = row else { return Ok(None) };

    let outcome = ActiveRunOutcome {
        id: row.try_get("id")?,
        company_id: row.try_get("company_id")?,
        agent_id: row.try_get("agent_id")?,
        status: row.try_get("status")?,
        invocation_source: row.try_get("invocation_source")?,
        trigger_detail: row.try_get("trigger_detail")?,
        error: row.try_get("error")?,
        error_code: row.try_get("error_code")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        updated_at: row.try_get("updated_at")?,
    };
    publish_run_status_event(&outcome);
    tracing::info!(
        run_id = %outcome.id,
        agent_id = %outcome.agent_id,
        company_id = %outcome.company_id,
        error_code = %code.as_str(),
        "recorded responsible-user denial code on active heartbeat run"
    );
    Ok(Some(outcome))
}

fn publish_run_status_event(outcome: &ActiveRunOutcome) {
    let payload_value = serde_json::json!({
        "runId": outcome.id.to_string(),
        "agentId": outcome.agent_id.to_string(),
        "status": outcome.status,
        "invocationSource": outcome.invocation_source,
        "triggerDetail": outcome.trigger_detail,
        "error": outcome.error,
        "errorCode": outcome.error_code,
        "startedAt": outcome.started_at.map(|t| t.to_rfc3339()),
        "finishedAt": outcome.finished_at.map(|t| t.to_rfc3339()),
    });
    let payload: LiveEventPayload = payload_value
        .as_object()
        .cloned()
        .unwrap_or_default();
    publish_live_event(
        &outcome.company_id.to_string(),
        LiveEventType("heartbeat.run.status".into()),
        Some(payload),
    );
}
