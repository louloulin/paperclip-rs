#![forbid(unsafe_code)]
//! Pipeline attention aggregation —— Node \`pipelines-aggregation.ts\` 1:1。
//!
//! 当前 R639.2 子集：
//! - AttentionCaller（user / agent）
//! - AttentionCaseDisplay / SuggestionItem / ReviewItem
//! - 相关常量（PIPELINE_ATTENTION_DEFAULT_LIMIT / MAX_LIMIT）

use serde::{Deserialize, Serialize};

/// Pipeline attention 的调用方标识。
///
/// 与 Node \`AttentionCaller\` 1:1 对齐 —— review-stage 等待逻辑依赖此字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttentionCaller {
    User { user_id: String },
    Agent { agent_id: String },
}

impl AttentionCaller {
    pub fn is_user(&self) -> bool {
        matches!(self, Self::User { .. })
    }
    pub fn is_agent(&self) -> bool {
        matches!(self, Self::Agent { .. })
    }
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Agent { agent_id } => Some(agent_id.as_str()),
            _ => None,
        }
    }
}

/// Pipeline attention 中嵌入的 case 展示形态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionCaseDisplay {
    pub id: String,
    pub case_key: String,
    pub title: String,
    pub summary: Option<String>,
    pub version: i32,
    #[serde(default)]
    pub terminal_kind: Option<String>,
    pub updated_at: String,
    pub created_at: String,
    pub pipeline: AttentionPipelineRef,
    pub stage: AttentionStageRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionPipelineRef {
    pub id: String,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionStageRef {
    pub id: String,
    pub key: String,
    pub name: String,
    pub kind: String,
}

/// suggestion 数据源的单条记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionItem {
    #[serde(rename = "case")]
    pub case: AttentionCaseDisplay,
    pub suggestion: SuggestionPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionPayload {
    pub id: String,
    pub from_stage_key: String,
    pub from_stage_name: String,
    pub to_stage_key: String,
    #[serde(default)]
    pub to_stage_name: Option<String>,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    pub created_at: String,
    #[serde(default)]
    pub suggested_by: Option<SuggestionActor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionActor {
    pub agent_id: String,
    pub agent_name: String,
}

/// review 数据源的单条记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    #[serde(rename = "case")]
    pub case: AttentionCaseDisplay,
    pub review: ReviewConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewConfig {
    pub expected_version: i32,
    #[serde(default)]
    pub approve_to_stage_key: Option<String>,
    #[serde(default)]
    pub reject_to_stage_key: Option<String>,
    #[serde(default)]
    pub request_changes_to_stage_key: Option<String>,
    pub require_reject_reason: bool,
    pub require_request_changes_reason: bool,
    pub reviewer_kind: String,
}

/// list_pipeline_attention 返回结构。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineAttention {
    pub suggestions: Vec<SuggestionItem>,
    pub reviews: Vec<ReviewItem>,
    pub counts: PipelineAttentionCounts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PipelineAttentionCounts {
    pub suggestions: usize,
    pub reviews: usize,
}

pub const PIPELINE_ATTENTION_DEFAULT_LIMIT: i64 = 50;
pub const PIPELINE_ATTENTION_MAX_LIMIT: i64 = 100;

/// 与 Node \`boundedLimit\` 1:1。
pub fn bounded_limit(limit: Option<i64>, fallback: i64, max: i64) -> i64 {
    let value = limit.unwrap_or(fallback);
    value.clamp(1, max)
}

#[cfg(test)]
mod types_tests {
    use super::*;

    #[test]
    fn r6392_attention_caller_user_and_agent() {
        let user = AttentionCaller::User { user_id: "u-1".into() };
        assert!(user.is_user());
        assert!(!user.is_agent());
        assert_eq!(user.agent_id(), None);
        let agent = AttentionCaller::Agent { agent_id: "a-1".into() };
        assert!(!agent.is_user());
        assert!(agent.is_agent());
        assert_eq!(agent.agent_id(), Some("a-1"));
    }

    #[test]
    fn r6392_bounded_limit_clamps_range() {
        assert_eq!(bounded_limit(None, 50, 100), 50);
        assert_eq!(bounded_limit(Some(0), 50, 100), 1);
        assert_eq!(bounded_limit(Some(1000), 50, 100), 100);
        assert_eq!(bounded_limit(Some(10), 50, 100), 10);
    }
}
