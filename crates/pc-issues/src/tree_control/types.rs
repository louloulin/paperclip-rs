//! Business types for the issue tree control service.
//!
//! 类型严格对应 Node `services/issue-tree-control.ts` 暴露给上层的形状，
//! 但在 Rust 侧用更精确的 enum + struct 表达。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_core::Timestamp;

/// Tree control mode — 与 Node `IssueTreeControlMode` 严格对齐。
///
/// - `pause`: 暂停子树所有 issue（不取消 runs）。
/// - `stop`: 取消子树所有 active runs。
/// - `throttle`: 限流（标记待后续路由层使用）。
/// - `isolate`: 隔离（阻止外部 wake / 唤醒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueTreeControlMode {
    Pause,
    Stop,
    Throttle,
    Isolate,
}

impl IssueTreeControlMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Stop => "stop",
            Self::Throttle => "throttle",
            Self::Isolate => "isolate",
        }
    }
}

/// 单个 issue 在预览中的快照 — 与 Node `IssueTreePreviewIssue` 对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreePreviewIssue {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub depth: i32,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
    pub assignee_agent_id: Option<Uuid>,
    pub assignee_user_id: Option<String>,
}

/// 单条 hold 在列表 / 详情视图的轻量投影。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreeHoldSummary {
    pub id: Uuid,
    pub company_id: Uuid,
    pub root_issue_id: Uuid,
    pub mode: String,
    pub status: String,
    pub reason: Option<String>,
    pub release_policy: serde_json::Value,
    pub released_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 单条 hold 的全量投影（含 release 元数据 + actor）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreeHoldInfo {
    pub id: Uuid,
    pub company_id: Uuid,
    pub root_issue_id: Uuid,
    pub mode: String,
    pub status: String,
    pub reason: Option<String>,
    pub release_policy: serde_json::Value,
    pub created_by_actor_type: String,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
    pub released_at: Option<Timestamp>,
    pub released_by_actor_type: Option<String>,
    pub released_by_agent_id: Option<Uuid>,
    pub released_by_user_id: Option<String>,
    pub released_by_run_id: Option<Uuid>,
    pub release_reason: Option<String>,
    pub release_metadata: Option<serde_json::Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 预览阶段统计的子树计数 — 与 Node 端 `preview.totals` 对齐。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreeAffectedCount {
    pub total: i64,
    pub active: i64,
    pub cancelled: i64,
    pub done: i64,
    pub paused: i64,
    pub skipped: i64,
}

/// 预览阶段输出的非致命告警 — 例如 root 已是终态 / 已有 active hold。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreePreviewWarning {
    pub code: String,
    pub message: String,
    pub issue_id: Option<Uuid>,
}

/// 预览完整结果 — 与 Node `previewTreeControl` 返回对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreePreview {
    pub company_id: Uuid,
    pub root_issue_id: Uuid,
    pub mode: String,
    pub reason: Option<String>,
    pub counts: IssueTreeAffectedCount,
    pub issues: Vec<IssueTreePreviewIssue>,
    pub existing_hold_id: Option<Uuid>,
    pub warnings: Vec<IssueTreePreviewWarning>,
}

/// apply 阶段写入 hold + members 后的返回值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreeApplyResult {
    pub hold_id: Uuid,
    pub company_id: Uuid,
    pub root_issue_id: Uuid,
    pub mode: String,
    pub member_count: i64,
    pub skipped_count: i64,
    pub cancelled_runs: i64,
    pub created_at: Timestamp,
}

/// release 阶段返回值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreeReleaseResult {
    pub hold_id: Uuid,
    pub company_id: Uuid,
    pub root_issue_id: Uuid,
    pub mode: String,
    pub reason: Option<String>,
    pub released_at: Timestamp,
    pub released_by_actor_type: String,
}

/// 单个被 hold 影响的 issue 快照 — 与 Node `IssueTreePreviewIssue` 相同
/// 形状但来源于持久化的 hold_members 表。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AffectedIssue {
    pub hold_id: Uuid,
    pub issue_id: Uuid,
    pub parent_issue_id: Option<Uuid>,
    pub depth: i32,
    pub issue_identifier: Option<String>,
    pub issue_title: String,
    pub issue_status: String,
    pub assignee_agent_id: Option<Uuid>,
    pub assignee_user_id: Option<String>,
    pub active_run_id: Option<Uuid>,
    pub active_run_status: Option<String>,
    pub skipped: bool,
    pub skip_reason: Option<String>,
}
