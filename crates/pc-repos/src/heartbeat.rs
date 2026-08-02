//! `heartbeat_runs` 与 `heartbeat_run_events` 数据访问。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

const RUN_COLUMNS: &str = "id, company_id, agent_id, invocation_source, trigger_detail, status, \
responsible_user_id, started_at, finished_at, error, wakeup_request_id, exit_code, signal, \
usage_json, result_json, external_run_id, process_pid, process_group_id, process_started_at, \
last_output_at, last_output_seq, retry_of_run_id, scheduled_retry_at, scheduled_retry_attempt, \
context_snapshot, created_at, updated_at";

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
    pub external_run_id: Option<String>,
    pub process_pid: Option<i32>,
    pub process_group_id: Option<i32>,
    pub process_started_at: Option<Timestamp>,
    pub last_output_at: Option<Timestamp>,
    pub last_output_seq: i32,
    pub retry_of_run_id: Option<Uuid>,
    pub scheduled_retry_at: Option<Timestamp>,
    pub scheduled_retry_attempt: i32,
    pub context_snapshot: Option<serde_json::Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
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

pub struct CreateHeartbeat<'a> {
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub invocation_source: &'a str,
    pub trigger_detail: Option<&'a str>,
    pub responsible_user_id: Option<&'a str>,
    pub wakeup_request_id: Option<Uuid>,
    pub context_snapshot: Option<serde_json::Value>,
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

    pub async fn list_for_agent(&self, agent_id: Uuid) -> sqlx::Result<Vec<HeartbeatRow>> {
        let query = format!(
            "SELECT {RUN_COLUMNS} FROM heartbeat_runs WHERE agent_id=$1 ORDER BY created_at DESC"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(agent_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn list_recoverable(&self, limit: i64) -> sqlx::Result<Vec<HeartbeatRow>> {
        let query = format!(
            "SELECT {RUN_COLUMNS} FROM heartbeat_runs \
             WHERE status IN ('queued','running') ORDER BY created_at ASC LIMIT $1"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(limit.clamp(1, 10_000))
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        agent_id: Option<Uuid>,
        limit: i64,
    ) -> sqlx::Result<Vec<HeartbeatRow>> {
        let query = format!(
            "SELECT {RUN_COLUMNS} FROM heartbeat_runs \
             WHERE company_id=$1 AND ($2::uuid IS NULL OR agent_id=$2) \
             ORDER BY created_at DESC LIMIT $3"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(company_id)
            .bind(agent_id)
            .bind(limit.clamp(1, 1000))
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn mark_running(&self, run_id: Uuid) -> sqlx::Result<Option<HeartbeatRow>> {
        let query = format!(
            "UPDATE heartbeat_runs SET status='running', started_at=COALESCE(started_at, now()), \
             updated_at=now() WHERE id=$1 AND status='queued' RETURNING {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(run_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn finish(
        &self,
        run_id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> sqlx::Result<Option<HeartbeatRow>> {
        let query = format!(
            "UPDATE heartbeat_runs SET status=$2, error=$3, finished_at=now(), updated_at=now() \
             WHERE id=$1 AND status IN ('queued','running') RETURNING {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(run_id)
            .bind(status)
            .bind(error)
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
        sqlx::query_as::<_, HeartbeatEventRow>(
            "INSERT INTO heartbeat_run_events \
             (company_id, run_id, agent_id, seq, event_type, message, payload) \
             VALUES ($1,$2,$3, \
               COALESCE((SELECT MAX(seq)+1 FROM heartbeat_run_events WHERE run_id=$2), 1), \
               $4,$5,$6) \
             RETURNING id, company_id, run_id, agent_id, seq, event_type, stream, level, color, \
                       message, payload, created_at",
        )
        .bind(run.company_id)
        .bind(run.id)
        .bind(run.agent_id)
        .bind(event_type)
        .bind(message)
        .bind(payload)
        .fetch_one(self.db.pool())
        .await
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
        let mut transaction = self.db.pool().begin().await?;
        let message_bytes =
            message.map_or(0_i64, |text| i64::try_from(text.len()).unwrap_or(i64::MAX));
        let event = sqlx::query_as::<_, HeartbeatEventRow>(
            "INSERT INTO heartbeat_run_events \
             (company_id, run_id, agent_id, seq, event_type, stream, message, payload) \
             VALUES ($1,$2,$3, \
               COALESCE((SELECT MAX(seq)+1 FROM heartbeat_run_events WHERE run_id=$2), 1), \
               $4,$5,$6,$7) \
             RETURNING id, company_id, run_id, agent_id, seq, event_type, stream, level, color, \
                       message, payload, created_at",
        )
        .bind(run.company_id)
        .bind(run.id)
        .bind(run.agent_id)
        .bind(event_type)
        .bind(stream)
        .bind(message)
        .bind(payload)
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE heartbeat_runs SET last_output_at=now(), last_output_seq=$2, \
             last_output_stream=$3, last_output_bytes=$4, updated_at=now() WHERE id=$1",
        )
        .bind(run.id)
        .bind(event.seq)
        .bind(stream)
        .bind(message_bytes)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(event)
    }

    pub async fn finish_execution(
        &self,
        run_id: Uuid,
        status: &str,
        error: Option<&str>,
        result: Option<&pc_adapter_api::AdapterExecutionResult>,
    ) -> sqlx::Result<Option<HeartbeatRow>> {
        let usage = result
            .and_then(|result| result.usage.as_ref())
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
        let result_json = result.and_then(|result| result.result_json.clone());
        let query = format!(
            "UPDATE heartbeat_runs SET status=$2, error=$3, exit_code=$4, signal=$5, \
             usage_json=$6, result_json=$7, session_id_after=$8, error_code=$9, \
             finished_at=now(), updated_at=now() WHERE id=$1 RETURNING {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, HeartbeatRow>(&query)
            .bind(run_id)
            .bind(status)
            .bind(error)
            .bind(result.and_then(|result| result.exit_code))
            .bind(result.and_then(|result| result.signal.as_deref()))
            .bind(usage)
            .bind(result_json)
            .bind(result.and_then(|result| result.session_id.as_deref()))
            .bind(result.and_then(|result| result.error_code.as_deref()))
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn list_events(
        &self,
        run_id: Uuid,
        after_seq: i32,
        limit: i64,
    ) -> sqlx::Result<Vec<HeartbeatEventRow>> {
        sqlx::query_as::<_, HeartbeatEventRow>(
            "SELECT id, company_id, run_id, agent_id, seq, event_type, stream, level, color, \
                    message, payload, created_at FROM heartbeat_run_events \
             WHERE run_id=$1 AND seq>$2 ORDER BY seq ASC LIMIT $3",
        )
        .bind(run_id)
        .bind(after_seq.max(0))
        .bind(limit.clamp(1, 1000))
        .fetch_all(self.db.pool())
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            external_run_id: None,
            process_pid: None,
            process_group_id: None,
            process_started_at: None,
            last_output_at: None,
            last_output_seq: 0,
            retry_of_run_id: None,
            scheduled_retry_at: None,
            scheduled_retry_attempt: 0,
            context_snapshot: None,
            created_at: now,
            updated_at: now,
        };

        let value = serde_json::to_value(row).unwrap();
        assert_eq!(value["status"], "queued");
        assert!(value["started_at"].is_null());
        assert!(value["finished_at"].is_null());
    }
}
