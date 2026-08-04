//! `decision` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;
use pc_secrets::{DecisionSigningError, DecisionSigningService};

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct DecisionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub bundle_id: Option<Uuid>,
    pub origin_agent_id: Option<Uuid>,
    pub origin_issue_id: Option<Uuid>,
    pub origin_run_id: Option<Uuid>,
    pub rule_key: Option<String>,
    pub title: String,
    pub body: String,
    pub options: serde_json::Value,
    pub inputs: Option<serde_json::Value>,
    pub status: String,
    pub execution_status: Option<String>,
    pub chosen_option_id: Option<String>,
    pub input_values: Option<serde_json::Value>,
    pub decided_by_user_id: Option<String>,
    pub decided_at: Option<Timestamp>,
    pub expires_at: Timestamp,
    pub idempotency_key: Option<String>,
    pub signed_spec: String,
    pub target_snapshots: serde_json::Value,
    pub continuation_policy: String,
    pub metadata: serde_json::Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const COLS: &str = "id, company_id, bundle_id, origin_agent_id, origin_issue_id, origin_run_id, \
    rule_key, title, body, options, inputs, status, execution_status, chosen_option_id, \
    input_values, decided_by_user_id, decided_at, expires_at, idempotency_key, signed_spec, \
    target_snapshots, continuation_policy, metadata, created_at, updated_at";

pub struct DecisionRepo<'a> {
    pub db: &'a Db,
}

impl<'a> DecisionRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<DecisionRow>> {
        let sql = format!(
            "SELECT {COLS} FROM decisions WHERE company_id = $1 ORDER BY created_at DESC LIMIT 200"
        );
        sqlx::query_as::<_, DecisionRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await
    }

    /// 列出全部（跨公司）；limit 默认 200。
    pub async fn list_all(&self, limit: i64) -> sqlx::Result<Vec<DecisionRow>> {
        let sql = format!("SELECT {COLS} FROM decisions ORDER BY created_at DESC LIMIT $1");
        sqlx::query_as::<_, DecisionRow>(&sql)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<DecisionRow>> {
        let sql = format!("SELECT {COLS} FROM decisions WHERE id = $1");
        sqlx::query_as::<_, DecisionRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn create(
        &self,
        company_id: Uuid,
        title: &str,
        body: &str,
        decision_signing: &DecisionSigningService,
    ) -> sqlx::Result<DecisionRow> {
        // decisions 表要求 origin_agent_id/issue_id/run_id NOT NULL
        // 若公司下已有 agent + issue，则用最新的；否则用零 UUID 占位（FK 会校验，必须有真实记录）
        // 三个独立查询（不用 JOIN，因为 LEFT JOIN 在 issue 不存在时整行被滤掉）
        let agent_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM agents WHERE company_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
        let issue_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM issues WHERE company_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
        let run_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM heartbeat_runs WHERE agent_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(agent_id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;

        let id = Uuid::new_v4();
        let options = serde_json::json!([]);
        let target_snapshots = serde_json::json!({});
        let signed_spec = decision_signing
            .sign(&decision_signature_spec(id, &options, &target_snapshots))
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let sql = format!(
            "INSERT INTO decisions (id, company_id, origin_agent_id, origin_issue_id, origin_run_id, \
             title, body, options, signed_spec, target_snapshots, expires_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10, now() + interval '7 days') RETURNING {COLS}"
        );
        sqlx::query_as::<_, DecisionRow>(&sql)
            .bind(id)
            .bind(company_id)
            .bind(agent_id)
            .bind(issue_id)
            .bind(run_id)
            .bind(title)
            .bind(body)
            .bind(options)
            .bind(signed_spec)
            .bind(target_snapshots)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM decisions WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }
}

pub fn decision_signature_spec(
    id: Uuid,
    options: &serde_json::Value,
    target_snapshots: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "decisionId": id.to_string(),
        "options": options,
        "targetSnapshots": target_snapshots,
    })
}

pub fn verify_decision_signature(
    id: Uuid,
    options: &serde_json::Value,
    target_snapshots: &serde_json::Value,
    signed_spec: &str,
    decision_signing: &DecisionSigningService,
) -> Result<bool, DecisionSigningError> {
    decision_signing.verify(
        &decision_signature_spec(id, options, target_snapshots),
        signed_spec,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn signature_spec_matches_node_shape() {
        let id = Uuid::parse_str("6b43a722-4bf8-4ead-9d7f-74e99d37ff75").unwrap();
        assert_eq!(
            decision_signature_spec(id, &serde_json::json!([]), &serde_json::json!({})),
            serde_json::json!({
                "decisionId": "6b43a722-4bf8-4ead-9d7f-74e99d37ff75",
                "options": [],
                "targetSnapshots": {},
            })
        );
    }

    #[test]
    fn signature_verification_detects_tampering() {
        let id = Uuid::new_v4();
        let signer = DecisionSigningService::from_secret(TEST_SECRET).unwrap();
        let options = serde_json::json!([{ "id": "yes", "effects": [] }]);
        let snapshots = serde_json::json!({});
        let signature = signer
            .sign(&decision_signature_spec(id, &options, &snapshots))
            .unwrap();

        assert!(verify_decision_signature(id, &options, &snapshots, &signature, &signer).unwrap());
        assert!(!verify_decision_signature(
            id,
            &serde_json::json!([{ "id": "tampered", "effects": [] }]),
            &snapshots,
            &signature,
            &signer
        )
        .unwrap());
    }
}
