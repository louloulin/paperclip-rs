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
    #[serde(default)]
    pub heads_up: Vec<HeadsUpItem>,
    pub counts: PipelineAttentionCounts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PipelineAttentionCounts {
    pub suggestions: usize,
    pub reviews: usize,
    #[serde(default)]
    pub heads_up: usize,
}

/// heads_up 数据源的单条记录（drift detection）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadsUpItem {
    #[serde(rename = "case")]
    pub case: AttentionCaseDisplay,
    pub drift: DriftEvent,
    #[serde(default)]
    pub active_work: Option<ActiveWork>,
    #[serde(default)]
    pub work_issue: Option<OpenWorkIssue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftEvent {
    pub event_id: String,
    pub created_at: String,
    #[serde(default)]
    pub previous_version: Option<i32>,
    #[serde(default)]
    pub version: Option<i32>,
    pub upstream: DriftUpstreamRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftUpstreamRef {
    pub case_id: Option<String>,
    pub case_key: Option<String>,
    pub title: Option<String>,
    pub pipeline_id: Option<String>,
    pub pipeline_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveWork {
    pub issue_id: String,
    pub issue_identifier: Option<String>,
    pub issue_title: String,
    pub issue_role: String,
    pub agent_id: String,
    pub agent_name: String,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenWorkIssue {
    pub issue_id: String,
    pub issue_identifier: Option<String>,
    pub title: String,
    pub status: String,
}

pub const PIPELINE_ATTENTION_DEFAULT_LIMIT: i64 = 50;
pub const PIPELINE_ATTENTION_MAX_LIMIT: i64 = 100;

/// 与 Node \`boundedLimit\` 1:1。
pub fn bounded_limit(limit: Option<i64>, fallback: i64, max: i64) -> i64 {
    let value = limit.unwrap_or(fallback);
    value.clamp(1, max)
}

// ===== R639.2.3: listCompanyCaseEvents =====

pub const COMPANY_CASE_EVENTS_DEFAULT_LIMIT: i64 = 50;
pub const COMPANY_CASE_EVENTS_MAX_LIMIT: i64 = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CompanyCaseEventsPage {
    pub items: Vec<CompanyCaseEventItem>,
    pub limit: i64,
    pub offset: i64,
    pub has_more: bool,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyCaseEventItem {
    pub id: String,
    pub company_id: String,
    pub case_id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub actor_type: String,
    #[serde(default)]
    pub actor_user_id: Option<String>,
    #[serde(default)]
    pub actor_agent_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub from_stage_id: Option<String>,
    #[serde(default)]
    pub to_stage_id: Option<String>,
    pub payload: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
    pub case: CompanyCaseEventCase,
    pub pipeline: CompanyCaseEventPipeline,
    #[serde(default)]
    pub from_stage: Option<CompanyCaseEventStage>,
    #[serde(default)]
    pub to_stage: Option<CompanyCaseEventStage>,
    #[serde(default)]
    pub actor_agent: Option<CompanyCaseEventAgent>,
    #[serde(default)]
    pub automation: Option<AutomationContext>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyCaseEventCase {
    pub id: String,
    pub case_key: String,
    pub title: String,
    #[serde(default)]
    pub terminal_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyCaseEventPipeline {
    pub id: String,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyCaseEventStage {
    pub id: String,
    pub key: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyCaseEventAgent {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationContext {
    #[serde(default)]
    pub routine: Option<AutomationRoutine>,
    #[serde(default)]
    pub issue: Option<AutomationIssue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRoutine {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationIssue {
    pub id: String,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
}

// ===== R639.2.3: getDirectChildrenSummary =====

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CaseChildrenRollup {
    pub total: i64,
    pub done: i64,
    pub dropped: i64,
    pub in_motion: i64,
}

/// 解析 stage.config.onEnter -> StageAutomation（与 Node stageAutomationFromConfig 1:1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageAutomation {
    pub id: String,
    pub routine_id: String,
}

pub fn stage_automation_from_config(stage_id: &str, config: &serde_json::Value) -> Option<StageAutomation> {
    let cfg = config.as_object()?;
    let on_enter = cfg.get("onEnter")?.as_object()?;
    if on_enter.get("type").and_then(|v| v.as_str()) != Some("run_routine") {
        return None;
    }
    let routine_id = on_enter.get("routineId").and_then(|v| v.as_str())?.trim();
    if routine_id.is_empty() {
        return None;
    }
    let id = on_enter
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{stage_id}:on_enter"));
    Some(StageAutomation {
        id,
        routine_id: routine_id.to_string(),
    })
}

/// 解析 event.payload 中的字符串字段（与 Node payloadString 1:1）。
pub fn payload_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

// ===== R639.2.4: getCaseChildrenTree =====

pub const CASE_CHILDREN_TREE_MAX_NODES: i64 = 1000;
pub const CASE_CHILDREN_TREE_MAX_DEPTH: i32 = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseChildStage {
    pub id: String,
    pub key: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseChildPipeline {
    pub id: String,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseChildGroup {
    pub pipeline: CaseChildPipeline,
    pub cases: Vec<CaseChildNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseChildNode {
    pub id: String,
    pub case_key: String,
    pub title: String,
    #[serde(default)]
    pub terminal_kind: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub pipeline: CaseChildPipeline,
    pub stage: CaseChildStage,
    pub rollup: CaseChildrenRollup,
    pub child_groups: Vec<CaseChildGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseChildrenTree {
    #[serde(rename = "case")]
    pub case: CaseChildNode,
    pub rollup: CaseChildrenRollup,
    pub child_groups: Vec<CaseChildGroup>,
    pub truncated: bool,
    pub total_nodes: usize,
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

    #[test]
    fn r6392_heads_up_items_serialize_camel_case() {
        let item = HeadsUpItem {
            case: AttentionCaseDisplay {
                id: "case-1".into(),
                case_key: "PAP-1".into(),
                title: "Case 1".into(),
                summary: None,
                version: 1,
                terminal_kind: None,
                updated_at: "2026-08-12T00:00:00Z".into(),
                created_at: "2026-08-12T00:00:00Z".into(),
                pipeline: AttentionPipelineRef {
                    id: "p-1".into(),
                    key: "p".into(),
                    name: "Pipeline".into(),
                },
                stage: AttentionStageRef {
                    id: "s-1".into(),
                    key: "working".into(),
                    name: "Working".into(),
                    kind: "working".into(),
                },
            },
            drift: DriftEvent {
                event_id: "evt-1".into(),
                created_at: "2026-08-12T00:00:00Z".into(),
                previous_version: Some(1),
                version: Some(2),
                upstream: DriftUpstreamRef {
                    case_id: Some("upstream-1".into()),
                    case_key: Some("PAP-2".into()),
                    title: Some("Upstream".into()),
                    pipeline_id: Some("p-2".into()),
                    pipeline_name: Some("Up Pipeline".into()),
                },
            },
            active_work: None,
            work_issue: None,
        };
        let json = serde_json::to_string(&item).expect("serialize");
        assert!(json.contains("\"caseKey\":\"PAP-1\""), "caseKey camelCase: {json}");
        assert!(json.contains("\"terminalKind\":null"), "terminalKind camelCase: {json}");
        assert!(json.contains("\"createdAt\":"), "createdAt camelCase: {json}");
        assert!(json.contains("\"previousVersion\":1"), "camelCase: {json}");
        assert!(json.contains("\"pipelineName\":\"Up Pipeline\""), "camelCase: {json}");
        assert!(json.contains("\"eventId\":\"evt-1\""), "eventId camelCase: {json}");
        assert!(json.contains("\"activeWork\":null"), "activeWork key present: {json}");
        assert!(json.contains("\"workIssue\":null"), "workIssue key present: {json}");
    }

    #[test]
    fn r63923_stage_automation_from_config_extracts_routine_id() {
        use serde_json::json;
        let cfg = json!({"onEnter": {"type": "run_routine", "routineId": "r-1", "id": "auto-1"}});
        let auto = stage_automation_from_config("stage-1", &cfg).unwrap();
        assert_eq!(auto.id, "auto-1");
        assert_eq!(auto.routine_id, "r-1");

        let cfg2 = json!({"onEnter": {"type": "run_routine", "routineId": " r-2 "}});
        let auto2 = stage_automation_from_config("stage-2", &cfg2).unwrap();
        assert_eq!(auto2.routine_id, "r-2");
        assert_eq!(auto2.id, "stage-2:on_enter", "default id is {{stage_id}}:on_enter");

        let cfg3 = json!({"onEnter": {"type": "other"}});
        assert!(stage_automation_from_config("stage-3", &cfg3).is_none());

        let cfg4 = json!({});
        assert!(stage_automation_from_config("stage-4", &cfg4).is_none());
    }

    #[test]
    fn r63923_payload_string_reads_string_value() {
        use serde_json::json;
        let payload = json!({"foo": "bar", "n": 42});
        assert_eq!(payload_string(&payload, "foo").as_deref(), Some("bar"));
        assert!(payload_string(&payload, "n").is_none(), "non-string returns None");
        assert!(payload_string(&payload, "missing").is_none());
    }

    #[test]
    fn r63923_company_case_events_page_serde_round_trip() {
        let page = CompanyCaseEventsPage {
            items: vec![],
            limit: 50,
            offset: 0,
            has_more: false,
            total: 0,
        };
        let json = serde_json::to_string(&page).expect("serialize");
        assert!(json.contains("\"limit\":50"));
        assert!(json.contains("\"hasMore\":false"));
        assert!(json.contains("\"total\":0"));
        let parsed: CompanyCaseEventsPage = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.limit, 50);
        assert_eq!(parsed.offset, 0);
        assert!(!parsed.has_more);
    }

    #[test]
    fn r63923_case_children_rollup_default_is_zero() {
        let r = CaseChildrenRollup::default();
        assert_eq!(r.total, 0);
        assert_eq!(r.done, 0);
        assert_eq!(r.dropped, 0);
        assert_eq!(r.in_motion, 0);
    }
}
