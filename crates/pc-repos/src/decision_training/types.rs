//! `decision_training::types` —— 共享类型定义（与 Node `decision-training.ts` 类型 1:1 对齐）。
//!
//! 包含：
//! - [`DecisionTrainingSourceKind`] —— source kind 枚举（'interaction' / 'approval' / 'execution_decision'）
//! - [`DecisionTrainingExampleRow`] —— `decision_training_examples` 表行（13 字段）
//! - [`CaptureInput`] / [`ListInput`] / [`ScrubDeletedCommentsInput`] —— 输入结构体
//! - [`CaptureResult`] / [`ScrubDeletedCommentsResult`] —— 输出结构体
//! - [`NotesHistoryEntry`] —— notes 历史条目
//! - [`ListExampleRow`] —— list JOIN 返回（example + issueTitle + issueIdentifier）
//! - [`DecisionTrainingSnapshotV1`] —— snapshot v1 结构（含 code / comments / runs 等嵌套）

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::types::Json;
use uuid::Uuid;

use pc_core::Timestamp;

// ============================================================================
// Source kind
// ============================================================================

/// Decision training source kind（与 DB CHECK 约束 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionTrainingSourceKind {
    Interaction,
    Approval,
    ExecutionDecision,
}

impl DecisionTrainingSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interaction => "interaction",
            Self::Approval => "approval",
            Self::ExecutionDecision => "execution_decision",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "interaction" => Some(Self::Interaction),
            "approval" => Some(Self::Approval),
            "execution_decision" => Some(Self::ExecutionDecision),
            _ => None,
        }
    }
}

// ============================================================================
// Table row
// ============================================================================

/// `decision_training_examples` 表行（与 Drizzle schema 1:1 对齐，13 字段）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionTrainingExampleRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub source_kind: String,
    pub source_id: Uuid,
    pub issue_id: Uuid,
    pub cutoff_at: Timestamp,
    pub notes: String,
    pub notes_history: Json<Vec<NotesHistoryEntry>>,
    pub decision_outcome: Option<String>,
    pub snapshot: Json<DecisionTrainingSnapshotV1>,
    pub retention_policy: String,
    pub created_by_user_id: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// ============================================================================
// Notes history entry
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesHistoryEntry {
    pub author: String,
    pub at: String, // ISO 8601 string (matches Node `new Date().toISOString()`)
    pub body: String,
}

// ============================================================================
// Inputs
// ============================================================================

/// `captureDecisionSnapshot` 入参（与 Node `CaptureInput` 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct CaptureInput {
    pub company_id: Uuid,
    pub source_kind: DecisionTrainingSourceKind,
    pub source_id: Uuid,
    pub issue_id: Uuid,
}

/// `decisionTrainingService.list` 入参（与 Node `ListInput` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct ListInput {
    pub project_id: Option<Uuid>,
    pub kind: Option<DecisionTrainingSourceKind>,
    pub author: Option<String>,
    pub q: Option<String>,
}

/// `decisionTrainingService.scrubDeletedComments` 入参（与 Node `ScrubDeletedCommentsInput` 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct ScrubDeletedCommentsInput {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub comment_ids: Vec<String>,
    pub deleted_at: DateTime<Utc>,
}

/// `decisionTrainingService.create` 入参（含 notes + createdByUserId）。
#[derive(Debug, Clone)]
pub struct CreateInput {
    pub company_id: Uuid,
    pub source_kind: DecisionTrainingSourceKind,
    pub source_id: Uuid,
    pub issue_id: Uuid,
    pub notes: String,
    pub created_by_user_id: String,
}

// ============================================================================
// Outputs
// ============================================================================

/// `captureDecisionSnapshot` 返回（与 Node 返回类型 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub cutoff_at: DateTime<Utc>,
    pub decision_outcome: Option<String>,
    pub snapshot: DecisionTrainingSnapshotV1,
}

/// `scrubDeletedComments` 返回（与 Node `{ updatedCount }` 1:1 对齐）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrubDeletedCommentsResult {
    pub updated_count: u64,
}

/// `list` 返回 JOIN 行（与 Node `select({ example, issueTitle, issueIdentifier })` 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct ListExampleRow {
    pub example: DecisionTrainingExampleRow,
    pub issue_title: String,
    pub issue_identifier: String,
}

// ============================================================================
// Snapshot v1
// ============================================================================

/// Snapshot v1 结构（与 Node `DecisionTrainingSnapshotV1` 1:1 对齐）。
///
/// 注：snapshot 字段是 `jsonb`，DB schema 用宽松形状（无强制嵌套结构）。
/// 本结构体提供 1:1 类型化表达；serde 反序列化时缺字段降级到默认值。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionTrainingSnapshotV1 {
    pub version: i32,
    #[serde(default)]
    pub retention: Option<SnapshotRetention>,
    #[serde(default)]
    pub captured_at: Option<String>,
    #[serde(default)]
    pub cutoff: Option<SnapshotCutoff>,
    #[serde(default)]
    pub issue: Option<Value>,
    #[serde(default)]
    pub comments: Option<Vec<Value>>,
    #[serde(default)]
    pub runs: Option<Vec<Value>>,
    #[serde(default)]
    pub decision: Option<SnapshotDecision>,
    #[serde(default)]
    pub code: Option<SnapshotCode>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRetention {
    pub policy: String,
    pub comment_deletion: String,
    pub issue_deletion: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCutoff {
    pub at: String,
    pub last_comment_id: Option<String>,
    pub comment_count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDecision {
    pub kind: String,
    pub payload: Value,
    pub actor: Option<Value>,
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCode {
    pub repo_url: Option<String>,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub commit_sha: Option<String>,
    pub resolution: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- DecisionTrainingSourceKind ----

    #[test]
    fn source_kind_as_str_matches_node() {
        assert_eq!(DecisionTrainingSourceKind::Interaction.as_str(), "interaction");
        assert_eq!(DecisionTrainingSourceKind::Approval.as_str(), "approval");
        assert_eq!(
            DecisionTrainingSourceKind::ExecutionDecision.as_str(),
            "execution_decision"
        );
    }

    #[test]
    fn source_kind_parse_round_trip() {
        for k in [
            DecisionTrainingSourceKind::Interaction,
            DecisionTrainingSourceKind::Approval,
            DecisionTrainingSourceKind::ExecutionDecision,
        ] {
            assert_eq!(DecisionTrainingSourceKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(DecisionTrainingSourceKind::parse("unknown"), None);
    }

    // ---- NotesHistoryEntry ----

    #[test]
    fn notes_history_entry_serializes_camel_case() {
        let entry = NotesHistoryEntry {
            author: "u1".into(),
            at: "2026-01-01T00:00:00Z".into(),
            body: "old notes".into(),
        };
        let v = serde_json::to_value(&entry).unwrap();
        assert_eq!(v["author"], serde_json::json!("u1"));
        assert_eq!(v["at"], serde_json::json!("2026-01-01T00:00:00Z"));
        assert_eq!(v["body"], serde_json::json!("old notes"));
    }

    // ---- DecisionTrainingSnapshotV1 ----

    #[test]
    fn snapshot_v1_default_has_version_one() {
        let s = DecisionTrainingSnapshotV1::default();
        assert_eq!(s.version, 0); // Default::default gives 0
    }

    #[test]
    fn snapshot_v1_round_trips() {
        let s = DecisionTrainingSnapshotV1 {
            version: 1,
            retention: Some(SnapshotRetention {
                policy: "scrub_deleted_comments_v1".into(),
                comment_deletion: "redact".into(),
                issue_deletion: "cascade".into(),
            }),
            captured_at: Some("2026-01-01T00:00:00Z".into()),
            cutoff: Some(SnapshotCutoff {
                at: "2026-01-01T00:00:00Z".into(),
                last_comment_id: Some("c1".into()),
                comment_count: 3,
            }),
            issue: Some(serde_json::json!({"id": "i1"})),
            comments: Some(vec![serde_json::json!({"id": "c1"})]),
            runs: Some(vec![serde_json::json!({"id": "r1"})]),
            decision: Some(SnapshotDecision {
                kind: "interaction".into(),
                payload: serde_json::json!({}),
                actor: None,
                outcome: Some("approved".into()),
            }),
            code: Some(SnapshotCode {
                repo_url: Some("https://github.com/x/y".into()),
                ref_: Some("main".into()),
                commit_sha: Some("abc1234".into()),
                resolution: "exact".into(),
            }),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["retention"]["policy"], "scrub_deleted_comments_v1");
        assert_eq!(v["cutoff"]["commentCount"], 3);
        // `ref` 字段重命名为 "ref"
        assert_eq!(v["code"]["ref"], "main");
        // `ref_` 字段不会泄漏到 JSON
        assert!(v["code"].get("ref_").is_none());

        // 反序列化
        let back: DecisionTrainingSnapshotV1 = serde_json::from_value(v).unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(
            back.retention.as_ref().unwrap().policy,
            "scrub_deleted_comments_v1"
        );
    }
}

// `FromRow` import
use sqlx::FromRow;
