//! Service —— `ChangeConsentGateService` 实现。
//!
//! 与 Node `changeConsentGateService(db)` 1:1 对齐。

use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use pc_repos::Db;

use super::helpers::{
    expand_target_keys_for_legacy_compatibility, payload_has_displayed_diff, read_non_empty_string,
    request_confirmation_result_consumed,
};
use super::types::{
    codes, mark_result_consumed, AssertConsentedInput, ChangeConsentError, ChangeConsentResult,
};

// ============================================================================
// Service
// ============================================================================

/// Change-consent gate 服务（与 Node `changeConsentGateService(db)` 1:1 对齐）。
pub struct ChangeConsentGateService {
    db: Db,
}

impl ChangeConsentGateService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 校验 actor 是否有权执行目标 mutation（与 Node `assertConsented` 1:1 对齐）。
    ///
    /// 成功：返回 `Ok(true)`，并把命中的 `request_confirmation.result` 标记为"已消费"。
    /// 失败：返回 `Forbidden`（含 code）。
    pub async fn assert_consented(
        &self,
        input: &AssertConsentedInput,
    ) -> ChangeConsentResult<bool> {
        // 1. actor agent id 必须非空
        let actor_agent_id = input
            .actor_agent_id
            .as_ref()
            .and_then(|s| read_non_empty_string(&Value::String(s.clone())))
            .ok_or_else(|| ChangeConsentError::Forbidden {
                message: "Reflection Coach mutations require an agent actor".to_string(),
                code: codes::REFLECTION_COACH_MUTATION_RUN_ID_REQUIRED,
                details: Value::Null,
            })?;

        // 2. actor run id 必须非空
        let actor_run_id = input
            .actor_run_id
            .as_ref()
            .and_then(|s| read_non_empty_string(&Value::String(s.clone())))
            .ok_or_else(|| ChangeConsentError::Forbidden {
                message: "Reflection Coach mutations require a run id".to_string(),
                code: codes::REFLECTION_COACH_MUTATION_RUN_ID_REQUIRED,
                details: Value::Null,
            })?;

        // 3. target keys 必须非空
        let actor_run_id_uuid =
            Uuid::parse_str(&actor_run_id).map_err(|_| ChangeConsentError::Forbidden {
                message: "actor run id must be a valid uuid".to_string(),
                code: codes::REFLECTION_COACH_MUTATION_RUN_ID_REQUIRED,
                details: Value::Null,
            })?;
        let actor_agent_id_uuid =
            Uuid::parse_str(&actor_agent_id).map_err(|_| ChangeConsentError::Forbidden {
                message: "actor agent id must be a valid uuid".to_string(),
                code: codes::REFLECTION_COACH_MUTATION_RUN_ID_REQUIRED,
                details: Value::Null,
            })?;

        let target_keys: Vec<String> = input
            .target_keys
            .iter()
            .filter_map(|k| read_non_empty_string(&Value::String(k.clone())))
            .collect::<std::collections::BTreeSet<_>>() // 用 BTreeSet 去重
            .into_iter()
            .collect();

        if target_keys.is_empty() {
            return Err(ChangeConsentError::Forbidden {
                message: "Reflection Coach mutation target is not gateable".to_string(),
                code: codes::REFLECTION_COACH_MUTATION_TARGET_REQUIRED,
                details: Value::Null,
            });
        }
        let query_target_keys = expand_target_keys_for_legacy_compatibility(&target_keys);

        // 4. 查询候选 accepted interactions（最多 10 条，按 resolvedAt/createdAt desc）。
        let rows = sqlx::query(
            r#"
            SELECT id, source_run_id, payload, result
            FROM issue_thread_interactions
            WHERE company_id = $1
              AND created_by_agent_id = $2
              AND kind = 'request_confirmation'
              AND status = 'accepted'
              AND (payload->'target'->>'key' = ANY($3))
            ORDER BY COALESCE(resolved_at, created_at) DESC
            LIMIT 10
            "#,
        )
        .bind(input.company_id)
        .bind(actor_agent_id_uuid)
        .bind(&query_target_keys)
        .fetch_all(self.db.pool())
        .await?;

        // 5. 找到第一个有效 row（满足所有约束）
        let accepted = rows.into_iter().find(|row| {
            let payload: Value = row.try_get("payload").unwrap_or(Value::Null);
            let result: Option<Value> = row.try_get("result").ok();
            let source_run_id: Option<Uuid> = row.try_get("source_run_id").ok();

            // payload.target.type === "custom"
            let target_type = payload
                .as_object()
                .and_then(|o| o.get("target"))
                .and_then(|t| t.as_object())
                .and_then(|t| t.get("type"))
                .and_then(|v| v.as_str());
            if target_type != Some("custom") {
                return false;
            }

            let target_key = payload
                .as_object()
                .and_then(|o| o.get("target"))
                .and_then(|t| t.as_object())
                .and_then(|t| t.get("key"))
                .and_then(|v| v.as_str());
            if let Some(k) = target_key {
                if !query_target_keys.iter().any(|x| x == k) {
                    return false;
                }
            } else {
                return false;
            }

            // result.outcome === "accepted" && !consumed
            let outcome = result
                .as_ref()
                .and_then(|r| r.as_object())
                .and_then(|o| o.get("outcome"))
                .and_then(|v| v.as_str());
            if outcome != Some("accepted") {
                return false;
            }
            if request_confirmation_result_consumed(result.as_ref()) {
                return false;
            }

            // payload has displayed diff
            if !payload_has_displayed_diff(&payload) {
                return false;
            }

            // sourceRunId 存在且 !== actorRunId
            match source_run_id {
                Some(src) if src != actor_run_id_uuid => true,
                _ => false,
            }
        });

        let Some(accepted) = accepted else {
            return Err(gate_required_error(&target_keys));
        };

        let accepted_id: Uuid = accepted
            .try_get("id")
            .map_err(|e| ChangeConsentError::Repo(format!("missing id column: {e}")))?;

        // 6. 把命中行标记为 consumed
        let original_result: Value = accepted
            .try_get("result")
            .map_err(|e| ChangeConsentError::Repo(format!("missing result column: {e}")))?;
        let consumed_result = mark_result_consumed(
            original_result.clone(),
            &actor_run_id,
            &chrono::Utc::now().to_rfc3339(),
        );

        let consumed = sqlx::query(
            r#"
            UPDATE issue_thread_interactions
            SET result = $2,
                updated_at = now()
            WHERE id = $1
              AND company_id = $3
              AND created_by_agent_id = $4
              AND kind = 'request_confirmation'
              AND status = 'accepted'
              AND result->>'outcome' = 'accepted'
              AND coalesce(result->>'consumedByRunId', result->>'consumedAt') IS NULL
            RETURNING id
            "#,
        )
        .bind(accepted_id)
        .bind(&consumed_result)
        .bind(input.company_id)
        .bind(actor_agent_id_uuid)
        .fetch_optional(self.db.pool())
        .await?;

        if consumed.is_none() {
            // 并发场景：另一 run 同时消费了
            return Err(gate_required_error(&target_keys));
        }

        Ok(true)
    }
}

fn gate_required_error(target_keys: &[String]) -> ChangeConsentError {
    ChangeConsentError::Forbidden {
        message: "Reflection Coach mutations require an accepted request_confirmation with a displayed diff for this target, created in a previous run and not already consumed.".to_string(),
        code: codes::REFLECTION_COACH_MUTATION_GATE_REQUIRED,
        details: serde_json::json!({ "targetKeys": target_keys }),
    }
}
