use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use super::rules::{expand_target_keys, normalize_target_keys, row_is_eligible};
use super::{AssertConsentInput, ChangeConsentError, ChangeConsentResult};
use crate::Db;

#[derive(Debug, FromRow)]
struct ConfirmationRow {
    id: Uuid,
    source_run_id: Option<Uuid>,
    payload: Value,
    result: Option<Value>,
}

pub struct ChangeConsentGateRepo<'a> {
    db: &'a Db,
}

impl<'a> ChangeConsentGateRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 返回 false 表示调用者不是 agent，不需要 Reflection Coach gate。
    pub async fn assert_consented(&self, input: AssertConsentInput) -> ChangeConsentResult<bool> {
        let Some(actor_agent_id) = input.actor_agent_id else {
            return Ok(false);
        };
        let actor_run_id = input
            .actor_run_id
            .ok_or(ChangeConsentError::RunIdRequired)?;
        let target_keys = normalize_target_keys(&input.target_keys);
        if target_keys.is_empty() {
            return Err(ChangeConsentError::TargetRequired);
        }
        let query_target_keys = expand_target_keys(&target_keys);

        let mut tx = self.db.pool().begin().await?;
        let rows: Vec<ConfirmationRow> = sqlx::query_as(
            "SELECT id, source_run_id, payload, result FROM issue_thread_interactions \
             WHERE company_id=$1 AND created_by_agent_id=$2 \
               AND kind='request_confirmation' AND status='accepted' \
               AND payload->'target'->>'key' = ANY($3::text[]) \
             ORDER BY resolved_at DESC NULLS LAST, created_at DESC LIMIT 10 FOR UPDATE",
        )
        .bind(input.company_id)
        .bind(actor_agent_id)
        .bind(&query_target_keys)
        .fetch_all(&mut *tx)
        .await?;

        let accepted = rows.into_iter().find(|row| {
            row.result.as_ref().is_some_and(|result| {
                row_is_eligible(
                    row.source_run_id,
                    &row.payload,
                    result,
                    actor_run_id,
                    &query_target_keys,
                )
            })
        });
        let Some(accepted) = accepted else {
            return Err(ChangeConsentError::GateRequired);
        };
        let mut result = accepted.result.ok_or(ChangeConsentError::GateRequired)?;
        let Some(object) = result.as_object_mut() else {
            return Err(ChangeConsentError::GateRequired);
        };
        object.insert(
            "consumedAt".into(),
            Value::String(pc_core::Timestamp::now().to_string()),
        );
        object.insert(
            "consumedByRunId".into(),
            Value::String(actor_run_id.to_string()),
        );

        let consumed = sqlx::query(
            "UPDATE issue_thread_interactions SET result=$2, updated_at=now() \
             WHERE id=$1 AND company_id=$3 AND created_by_agent_id=$4 \
               AND kind='request_confirmation' AND status='accepted' \
               AND result->>'outcome'='accepted' \
               AND coalesce(result->>'consumedByRunId', result->>'consumedAt') IS NULL",
        )
        .bind(accepted.id)
        .bind(result)
        .bind(input.company_id)
        .bind(actor_agent_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if consumed == 0 {
            return Err(ChangeConsentError::GateRequired);
        }
        tx.commit().await?;
        Ok(true)
    }
}
