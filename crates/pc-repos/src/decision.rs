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

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct SignedDecisionRow {
    pub company_id: Uuid,
    pub options: serde_json::Value,
    pub target_snapshots: serde_json::Value,
    pub signed_spec: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct DecisionEffectExecutionRow {
    pub id: Uuid,
    pub decision_id: Uuid,
    pub effect_index: i32,
    pub effect_type: String,
    pub target_issue_id: Uuid,
    pub status: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub activity_log_id: Option<Uuid>,
    pub executed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Default)]
pub struct DecisionListFilter {
    pub status: Option<String>,
    pub bundle_id: Option<Uuid>,
    pub origin_agent_id: Option<Uuid>,
    pub target_issue_id: Option<Uuid>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct DecisionStatsFilter {
    pub origin_agent_id: Option<Uuid>,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecisionStatsCounts {
    pub proposed: i64,
    pub accepted: i64,
    pub rejected: i64,
    pub expired: i64,
}

impl DecisionStatsCounts {
    pub const ZERO: Self = Self { proposed: 0, accepted: 0, rejected: 0, expired: 0 };
    pub fn add(&mut self, other: &Self) {
        self.proposed += other.proposed;
        self.accepted += other.accepted;
        self.rejected += other.rejected;
        self.expired += other.expired;
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRuleKeyGroup {
    pub rule_key: Option<String>,
    #[serde(flatten)]
    pub counts: DecisionStatsCounts,
    pub chosen_options: Vec<DecisionChosenOptionCount>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionChosenOptionCount {
    pub option_id: String,
    pub count: i64,
}

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

    /// 注意力队列用：按最近活动时间列出仍等待处理的 open decisions。
    pub async fn list_open_attention(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<DecisionRow>> {
        let sql = format!(
            "SELECT {COLS} FROM decisions \
             WHERE company_id = $1 AND status = 'open' \
             ORDER BY updated_at DESC, id DESC LIMIT $2"
        );
        sqlx::query_as::<_, DecisionRow>(&sql)
            .bind(company_id)
            .bind(limit.clamp(1, 200))
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

    /// Insert a decision with default `options = []` and `expires_at = now + 7 days`.
    /// Thin wrapper around [`Self::create_with_options`] that preserves the
    /// legacy 4-arg signature.
    pub async fn create(
        &self,
        company_id: Uuid,
        title: &str,
        body: &str,
        decision_signing: &DecisionSigningService,
    ) -> sqlx::Result<DecisionRow> {
        self.create_with_options(
            company_id,
            title,
            body,
            serde_json::json!([]),
            None,
            decision_signing,
        )
        .await
    }

    /// Insert a decision with caller-supplied `options` and optional `expires_at`.
    /// When `expires_at` is `None`, the default of `now + 7 days` is used
    /// (same as the legacy `create`).
    pub async fn create_with_options(
        &self,
        company_id: Uuid,
        title: &str,
        body: &str,
        options: serde_json::Value,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
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
        let target_snapshots = serde_json::json!({});
        let signed_spec = decision_signing
            .sign(&decision_signature_spec(id, &options, &target_snapshots))
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let expires_clause = match expires_at {
            Some(ts) => format!("'{}'::timestamptz", ts.to_rfc3339()),
            None => "now() + interval '7 days'".to_string(),
        };
        let sql = format!(
            "INSERT INTO decisions (id, company_id, origin_agent_id, origin_issue_id, origin_run_id, \
             title, body, options, signed_spec, target_snapshots, expires_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10, {expires_clause}) RETURNING {COLS}"
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

    /// R799: returns the deleted row directly (was bool). 0 rows = `RowNotFound`.
    pub async fn delete(&self, id: Uuid) -> sqlx::Result<DecisionRow> {
        sqlx::query_as::<_, DecisionRow>(
            "DELETE FROM decisions WHERE id = $1 \
             RETURNING id, company_id, bundle_id, origin_agent_id, origin_issue_id, origin_run_id, \
                rule_key, title, body, options, inputs, status, execution_status, \
                chosen_option_id, input_values, decided_by_user_id, decided_at, expires_at, \
                idempotency_key, signed_spec, target_snapshots, continuation_policy, metadata, \
                created_at, updated_at",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(sqlx::Error::RowNotFound)
    }

    // ============ Round 173: signed fields + status transitions + stats ============

    /// 取决策签名相关字段（company_id + options + target_snapshots + signed_spec）。
    pub async fn get_signed_fields(
        &self,
        decision_id: Uuid,
    ) -> sqlx::Result<Option<SignedDecisionRow>> {
        sqlx::query_as::<_, SignedDecisionRow>(
            "SELECT company_id, options, target_snapshots, signed_spec              FROM decisions WHERE id = $1",
        )
        .bind(decision_id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// 将决策置为 decided：写入 chosen_option_id / decided_by / decided_at / input_values。
    pub async fn mark_decided(
        &self,
        decision_id: Uuid,
        chosen_option_id: &str,
        decided_by_user_id: Option<&str>,
        input_values: Option<&serde_json::Value>,
    ) -> sqlx::Result<bool> {
        let r = sqlx::query(
            "UPDATE decisions SET status = 'decided',                 chosen_option_id = $1, decided_by_user_id = $2,                 decided_at = now(),                 input_values = COALESCE($3, input_values),                 updated_at = now()              WHERE id = $4",
        )
        .bind(chosen_option_id)
        .bind(decided_by_user_id)
        .bind(input_values)
        .bind(decision_id)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// 将决策置为 dismissed：把 reason + dismissedByUserId 写入 metadata。
    pub async fn mark_dismissed(
        &self,
        decision_id: Uuid,
        reason: &str,
        decided_by_user_id: &str,
    ) -> sqlx::Result<bool> {
        let r = sqlx::query(
            "UPDATE decisions SET status = 'dismissed',                 metadata = COALESCE(metadata, '{}'::jsonb)                     || jsonb_build_object(                         'dismissReason', to_jsonb($1::text),                         'dismissedByUserId', to_jsonb($2::text)                     ),                 updated_at = now()              WHERE id = $3",
        )
        .bind(reason)
        .bind(decided_by_user_id)
        .bind(decision_id)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// 取决策的 company_id。
    pub async fn get_company_id(&self, decision_id: Uuid) -> sqlx::Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM decisions WHERE id = $1")
            .bind(decision_id)
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row.map(|(c,)| c))
    }

    /// R802: 将决策置为 cancelled (returns DecisionRow; 0 rows = RowNotFound).
    pub async fn mark_cancelled(&self, decision_id: Uuid) -> sqlx::Result<DecisionRow> {
        sqlx::query_as::<_, DecisionRow>(
            "UPDATE decisions SET status = 'cancelled', updated_at = now() WHERE id = $1 \
             RETURNING id, company_id, bundle_id, origin_agent_id, origin_issue_id, origin_run_id, \
                rule_key, title, body, options, inputs, status, execution_status, \
                chosen_option_id, input_values, decided_by_user_id, decided_at, expires_at, \
                idempotency_key, signed_spec, target_snapshots, continuation_policy, metadata, \
                created_at, updated_at",
        )
        .bind(decision_id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(sqlx::Error::RowNotFound)
    }

    /// 按状态统计某公司的决策数。
    pub async fn status_counts(&self, company_id: Uuid) -> sqlx::Result<Vec<(String, i64)>> {
        sqlx::query_as(
            "SELECT status, COUNT(*) FROM decisions              WHERE company_id = $1 GROUP BY status",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// 原子声明一个 effect execution 行；如果已存在则返回原行。
    /// 与上游  中的  等价。
    pub async fn claim_effect_execution(
        &self,
        decision_id: Uuid,
        effect_index: i32,
        effect_type: &str,
        target_issue_id: Uuid,
    ) -> sqlx::Result<Option<DecisionEffectExecutionRow>> {
        let row: Option<DecisionEffectExecutionRow> = sqlx::query_as(
            "INSERT INTO decision_effect_executions                 (decision_id, effect_index, effect_type, target_issue_id)              VALUES ($1, $2, $3, $4)              ON CONFLICT (decision_id, effect_index) DO NOTHING              RETURNING id, decision_id, effect_index, effect_type, target_issue_id,                        status, result, error, activity_log_id, executed_at"
        )
        .bind(decision_id)
        .bind(effect_index)
        .bind(effect_type)
        .bind(target_issue_id)
        .fetch_optional(self.db.pool())
        .await?;
        if row.is_some() { return Ok(row); }
        // 已经存在 → 读取
        sqlx::query_as::<_, DecisionEffectExecutionRow>(
            "SELECT id, decision_id, effect_index, effect_type, target_issue_id,                     status, result, error, activity_log_id, executed_at              FROM decision_effect_executions              WHERE decision_id = $1 AND effect_index = $2"
        )
        .bind(decision_id)
        .bind(effect_index)
        .fetch_optional(self.db.pool())
        .await
    }

    /// 标记一个 execution 的最终状态（executed / failed / skipped）。
    pub async fn finish_effect_execution(
        &self,
        execution_id: Uuid,
        status: &str,
        error: Option<&str>,
        result: Option<&serde_json::Value>,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "UPDATE decision_effect_executions              SET status = $1, error = $2, result = $3, executed_at = now()              WHERE id = $4"
        )
        .bind(status)
        .bind(error)
        .bind(result)
        .bind(execution_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 标记一个 effect execution 失败（包装 finish_effect_execution）。
    pub async fn fail_effect_execution(
        &self,
        execution_id: Uuid,
        error: &str,
        result: Option<&serde_json::Value>,
    ) -> sqlx::Result<()> {
        self.finish_effect_execution(execution_id, "failed", Some(error), result).await
    }

    /// 更新决策的 execution_status + metadata。
    pub async fn set_execution_status(
        &self,
        decision_id: Uuid,
        execution_status: &str,
        metadata_patch: Option<&serde_json::Value>,
    ) -> sqlx::Result<bool> {
        let r = if let Some(patch) = metadata_patch {
            sqlx::query(
                "UPDATE decisions SET execution_status = $1,                     metadata = COALESCE(metadata, '{}'::jsonb) || $2::jsonb,                     updated_at = now() WHERE id = $3"
            )
            .bind(execution_status)
            .bind(patch)
            .bind(decision_id)
            .execute(self.db.pool())
            .await?
        } else {
            sqlx::query(
                "UPDATE decisions SET execution_status = $1, updated_at = now()                  WHERE id = $2"
            )
            .bind(execution_status)
            .bind(decision_id)
            .execute(self.db.pool())
            .await?
        };
        Ok(r.rows_affected() > 0)
    }

    pub async fn list_filtered(
        &self,
        company_id: Uuid,
        filter: &DecisionListFilter,
    ) -> sqlx::Result<Vec<DecisionRow>> {
        let limit = filter.limit.unwrap_or(50).clamp(1, 100);
        if filter.target_issue_id.is_some() {
            let issue_id = filter.target_issue_id.expect("present");
            return sqlx::query_as::<_, DecisionRow>(&format!(
                "SELECT {COLS} FROM decisions                 WHERE company_id = $1                   AND id IN (SELECT decision_id FROM decision_target_issues                              WHERE company_id = $1 AND issue_id = $2)                 ORDER BY created_at DESC LIMIT $3"
            ))
            .bind(company_id)
            .bind(issue_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await;
        }
        let mut sql = format!("SELECT {COLS} FROM decisions WHERE company_id = $1");
        let mut idx = 2;
        if filter.status.is_some() {
            sql.push_str(&format!(" AND status = ${idx}"));
            idx += 1;
        }
        if filter.bundle_id.is_some() {
            sql.push_str(&format!(" AND bundle_id = ${idx}"));
            idx += 1;
        }
        if filter.origin_agent_id.is_some() {
            sql.push_str(&format!(" AND origin_agent_id = ${idx}"));
            idx += 1;
        }
        sql.push_str(" ORDER BY created_at DESC");
        sql.push_str(&format!(" LIMIT ${idx}"));
        let mut q = sqlx::query_as::<_, DecisionRow>(&sql).bind(company_id);
        if let Some(s) = &filter.status { q = q.bind(s); }
        if let Some(b) = filter.bundle_id { q = q.bind(b); }
        if let Some(a) = filter.origin_agent_id { q = q.bind(a); }
        q = q.bind(limit);
        q.fetch_all(self.db.pool()).await
    }

    pub async fn current_target_timestamps(
        &self,
        company_id: Uuid,
        decision_ids: &[Uuid],
    ) -> sqlx::Result<std::collections::HashMap<Uuid, Timestamp>> {
        if decision_ids.is_empty() { return Ok(Default::default()); }
        let rows: Vec<(Uuid, Timestamp)> = sqlx::query_as(
            "SELECT i.id, i.updated_at             FROM decision_target_issues dti             INNER JOIN issues i                ON i.company_id = dti.company_id AND i.id = dti.issue_id             WHERE dti.company_id = $1 AND dti.decision_id = ANY($2)"
        )
        .bind(company_id)
        .bind(decision_ids)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows.into_iter().collect())
    }

    pub async fn executions_for_one(
        &self,
        decision_id: Uuid,
    ) -> sqlx::Result<Vec<DecisionEffectExecutionRow>> {
        sqlx::query_as::<_, DecisionEffectExecutionRow>(
            "SELECT id, decision_id, effect_index, effect_type, target_issue_id,                    status, result, error, activity_log_id, executed_at             FROM decision_effect_executions             WHERE decision_id = $1 ORDER BY effect_index ASC"
        )
        .bind(decision_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn executions_for_many(
        &self,
        decision_ids: &[Uuid],
    ) -> sqlx::Result<Vec<DecisionEffectExecutionRow>> {
        if decision_ids.is_empty() { return Ok(Vec::new()); }
        sqlx::query_as::<_, DecisionEffectExecutionRow>(
            "SELECT id, decision_id, effect_index, effect_type, target_issue_id,                    status, result, error, activity_log_id, executed_at             FROM decision_effect_executions             WHERE decision_id = ANY($1)             ORDER BY decision_id, effect_index ASC"
        )
        .bind(decision_ids)
        .fetch_all(self.db.pool())
        .await
    }
    pub async fn stats_by_rule_key(
        &self,
        company_id: Uuid,
        filter: &DecisionStatsFilter,
    ) -> sqlx::Result<Vec<DecisionRuleKeyGroup>> {
        use std::collections::BTreeMap;
        let mut sql = String::from(
            "SELECT rule_key, status, chosen_option_id,                     COALESCE(metadata->'dismissed' = 'true'::jsonb, false) AS dismissed,                     COUNT(*) AS value             FROM decisions WHERE company_id = $1"
        );
        let mut idx = 2;
        if filter.origin_agent_id.is_some() {
            sql.push_str(&format!(" AND origin_agent_id = ${idx}"));
            idx += 1;
        }
        if filter.since.is_some() {
            sql.push_str(&format!(" AND created_at >= ${idx}"));
            idx += 1;
        }
        sql.push_str(" GROUP BY rule_key, status, chosen_option_id, dismissed");
        let mut q = sqlx::query_as::<_, (Option<String>, String, Option<String>, bool, i64)>(&sql)
            .bind(company_id);
        if let Some(a) = filter.origin_agent_id { q = q.bind(a); }
        if let Some(s) = filter.since { q = q.bind(s); }
        let rows = q.fetch_all(self.db.pool()).await?;
        let mut grouped: BTreeMap<Option<String>, (DecisionStatsCounts, BTreeMap<String, i64>)> =
            BTreeMap::new();
        for (rule_key, status, chosen_option_id, dismissed, value) in rows {
            let entry = grouped.entry(rule_key).or_insert_with(|| (
                DecisionStatsCounts::ZERO,
                BTreeMap::new(),
            ));
            let counts = &mut entry.0;
            let chosen = &mut entry.1;
            let v = value;
            match status.as_str() {
                "open" => counts.proposed += v,
                "expired" => counts.expired += v,
                "decided" => {
                    let rejected = chosen_option_id.as_deref() == Some("dismissed") || dismissed;
                    if rejected {
                        counts.rejected += v;
                    } else {
                        counts.accepted += v;
                        if let Some(oid) = chosen_option_id.as_ref() {
                            *chosen.entry(oid.clone()).or_insert(0) += v;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(grouped.into_iter().map(|(rule_key, (counts, chosen))| DecisionRuleKeyGroup {
            rule_key,
            counts,
            chosen_options: chosen.into_iter().map(|(option_id, count)| DecisionChosenOptionCount {
                option_id,
                count,
            }).collect(),
        }).collect())
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
