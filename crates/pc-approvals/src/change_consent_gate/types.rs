//! Types —— Change-consent gate DTOs and constants.
//!
//! 与 Node `server/src/services/change-consent-gate.ts` 1:1 对齐。

use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

// ============================================================================
// Constants
// ============================================================================

/// Agent profile 变更需要 consent 的字段集合（与 Node `AGENT_PROFILE_CHANGE_CONSENT_FIELDS` 1:1 对齐）。
pub const AGENT_PROFILE_CHANGE_CONSENT_FIELDS: &[&str] = &["name", "role", "title", "capabilities"];

/// Reflection Coach mutation gate 错误码（与 Node `forbidden({ code: ... })` 1:1 对齐）。
pub mod codes {
    pub const REFLECTION_COACH_MUTATION_RUN_ID_REQUIRED: &str =
        "reflection_coach_mutation_run_id_required";
    pub const REFLECTION_COACH_MUTATION_TARGET_REQUIRED: &str =
        "reflection_coach_mutation_target_required";
    pub const REFLECTION_COACH_MUTATION_GATE_REQUIRED: &str =
        "reflection_coach_mutation_gate_required";
}

// ============================================================================
// Errors
// ============================================================================

/// Change-consent gate 错误。
#[derive(Debug, Error)]
pub enum ChangeConsentError {
    #[error("forbidden: {message} (code={code})")]
    Forbidden {
        message: String,
        code: &'static str,
        /// 额外的结构化详情（`targetKeys` 等）。
        details: Value,
    },

    #[error("repo error: {0}")]
    Repo(String),
}

impl From<sqlx::Error> for ChangeConsentError {
    fn from(e: sqlx::Error) -> Self {
        Self::Repo(e.to_string())
    }
}

pub type ChangeConsentResult<T> = Result<T, ChangeConsentError>;

// ============================================================================
// Inputs
// ============================================================================

/// `assertConsented` 输入（与 Node 端 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct AssertConsentedInput {
    pub company_id: Uuid,
    pub actor_agent_id: Option<String>,
    pub actor_run_id: Option<String>,
    pub target_keys: Vec<String>,
}

impl AssertConsentedInput {
    pub fn new(
        company_id: Uuid,
        actor_agent_id: Option<String>,
        actor_run_id: Option<String>,
        target_keys: Vec<String>,
    ) -> Self {
        Self {
            company_id,
            actor_agent_id,
            actor_run_id,
            target_keys,
        }
    }
}

// ============================================================================
// Consumed marker
// ============================================================================

/// 在 `result` JSON 上写 `consumedAt` / `consumedByRunId` 后返回新对象。
///
/// 与 Node `markRequestConfirmationResultConsumed` 1:1 对齐：
/// ```js
/// return { ...result, consumedAt: consumedAt.toISOString(), consumedByRunId: actorRunId };
/// ```
pub fn mark_result_consumed(mut result: Value, actor_run_id: &str, consumed_at: &str) -> Value {
    if let Some(obj) = result.as_object_mut() {
        obj.insert(
            "consumedAt".to_string(),
            Value::String(consumed_at.to_string()),
        );
        obj.insert(
            "consumedByRunId".to_string(),
            Value::String(actor_run_id.to_string()),
        );
    }
    result
}
