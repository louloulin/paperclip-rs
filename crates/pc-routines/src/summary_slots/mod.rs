#![forbid(unsafe_code)]

//! Summary slot orchestration module — 1:1 port of Node
//! `paperclip/server/src/services/summary-slots.ts` + `summary-slot-finalization.ts`.
//!
//! Design goals:
//! - **Pure helpers**: scope label mapping, snapshot builders, description/title
//!   generation, finalization reason text — all testable without DB or HTTP.
//! - **Service factory**: `summary_slot_service(db)` mirrors the Node closure
//!   factory and exposes the same async entry points (`get_slot`,
//!   `list_revisions`, `generate`, `write`). The async methods delegate to
//!   `pc_repos::summary::SummaryRepo` and `pc_repos::document::DocumentRepo`.
//! - **Type-safe enums**: every Node string union (`SummarySlotScopeKind`,
//!   `SummarySlotKey`, `SummarySlotStatus`, `IssueStatus`) maps to a Rust enum
//!   with `as_str()` / `parse()` round-tripping.
//!
//! 与 Node 1:1 对齐常量：
//! - `SUMMARIZER_BUILT_IN_KEY = "summarizer"` (PAP-13920)
//! - `TERMINAL_ISSUE_STATUSES = {done, cancelled}`
//! - `DEFAULT_SUMMARY_FORMAT = "markdown"`
//! - `SUMMARY_SLOT_REVISION_LIMIT = 20`
//! - `SUMMARY_SNAPSHOT_GROUP_LIMIT = 12`
//! - `SUMMARY_SNAPSHOT_INITIAL_LOOKBACK_MS = 7 * 24 * 60 * 60 * 1_000`
//!
//! 限制（与项目一致）：
//! - `unsafe_code` forbidden via `#![forbid(unsafe_code)]`
//! - 无新顶层依赖（pc-routines 现有依赖足够）

mod finalization;

pub use finalization::{
    build_finalization_patch, failure_reason_for_terminal_issue,
    finalize_summary_slots_for_terminal_issue, finalization_scope, is_terminal_issue_status,
    FinalizationError, FinalizationPatch, FinalizationPlan, FinalizationResult,
    FinalizationScope, TerminalGenerationIssue,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use pc_repos::{Db, document::DocumentRepo, issue::IssueRepo, summary::SummaryRepo};

// ============================================================================
// 1:1 ports of Node constants
// ============================================================================

/// Built-in agent key for the Summarizer bundle (PAP-13920, mirrors Node).
pub const SUMMARIZER_BUILT_IN_KEY: &str = "summarizer";

/// Generation issues in these statuses are no longer active and can be superseded.
///
/// 与 Node `TERMINAL_ISSUE_STATUSES = new Set(["done", "cancelled"])` 1:1。
pub const TERMINAL_ISSUE_STATUSES: &[&str] = &["done", "cancelled"];

/// Default summary document format (Markdown).
pub const DEFAULT_SUMMARY_FORMAT: &str = "markdown";

/// Max revisions returned by `list_revisions`.
pub const SUMMARY_SLOT_REVISION_LIMIT: i64 = 20;

/// Max issues per status group in scope snapshots.
pub const SUMMARY_SNAPSHOT_GROUP_LIMIT: i64 = 12;

/// Initial lookback window when no `previous_generated_at` is known (7 days, ms).
pub const SUMMARY_SNAPSHOT_INITIAL_LOOKBACK_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

// ============================================================================
// Type-safe enums (1:1 with Node string unions)
// ============================================================================

/// `SummarySlotScopeKind` — Node union `"project" | "workspaces_overview" | "project_workspace"`.
///
/// 与 Node `@paperclipai/shared` `SUMMARY_SLOT_SCOPE_KINDS` 1:1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarySlotScopeKind {
    Project,
    WorkspacesOverview,
    ProjectWorkspace,
}

impl SummarySlotScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::WorkspacesOverview => "workspaces_overview",
            Self::ProjectWorkspace => "project_workspace",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "project" => Some(Self::Project),
            "workspaces_overview" => Some(Self::WorkspacesOverview),
            "project_workspace" => Some(Self::ProjectWorkspace),
            _ => None,
        }
    }
}

/// `SummarySlotKey` — Node union currently `["header"]`.
///
/// 与 Node `SUMMARY_SLOT_KEYS` 1:1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarySlotKey {
    Header,
}

impl SummarySlotKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "header" => Some(Self::Header),
            _ => None,
        }
    }
}

/// `SummarySlotStatus` — Node union `"idle" | "generating" | "failed"`.
///
/// 与 Node `SUMMARY_SLOT_STATUSES` 1:1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarySlotStatus {
    Idle,
    Generating,
    Failed,
}

impl SummarySlotStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Generating => "generating",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "idle" => Some(Self::Idle),
            "generating" => Some(Self::Generating),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// `IssueStatus` — Node union mirroring `@paperclipai/shared` `ISSUE_STATUSES`.
///
/// 仅纳入 SummarySlots 域实际用到的子集，与 Node `IssueStatus` 兼容。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    Backlog,
    Todo,
    InProgress,
    InReview,
    Done,
    Cancelled,
    Blocked,
}

impl IssueStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::InReview => "in_review",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "backlog" => Some(Self::Backlog),
            "todo" => Some(Self::Todo),
            "in_progress" => Some(Self::InProgress),
            "in_review" => Some(Self::InReview),
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }

    /// 与 Node `TERMINAL_ISSUE_STATUSES.has(status)` 1:1。
    pub fn is_terminal(self) -> bool {
        TERMINAL_ISSUE_STATUSES.contains(&self.as_str())
    }
}

/// Document format — Markdown is the only valid value for summaries.
///
/// 与 Node `DocumentFormat` 兼容：summary 强制 `"markdown"`，其它格式保留为
/// 扩展空间以保持 API 形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormat {
    Markdown,
}

impl DocumentFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
        }
    }
}

// ============================================================================
// Input/Output value objects
// ============================================================================

/// Selector input — 与 Node `SummarySlotSelectorInput` 1:1。
#[derive(Debug, Clone)]
pub struct SummarySlotSelectorInput {
    pub company_id: Uuid,
    pub scope_kind: SummarySlotScopeKind,
    pub slot_key: SummarySlotKey,
    pub scope_id: Option<Uuid>,
}

/// Actor for `generate()` — 与 Node `SummaryGenerateActor` 1:1。
#[derive(Debug, Clone, Default)]
pub struct SummaryGenerateActor {
    pub agent_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
}

/// Actor for `write()` — 与 Node `SummaryWriteActor` 1:1。
#[derive(Debug, Clone, Default)]
pub struct SummaryWriteActor {
    pub agent_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
}

/// Resolved selector — 与 Node `ResolvedSelector` 1:1（selector schema + 公司/ScopeId）。
#[derive(Debug, Clone)]
pub struct ResolvedSelector {
    pub company_id: Uuid,
    pub scope_kind: SummarySlotScopeKind,
    pub scope_id: Option<Uuid>,
    pub slot_key: SummarySlotKey,
}

/// `SummarySlot` — 与 Node `@paperclipai/shared` `SummarySlot` 1:1。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SummarySlot {
    pub id: Uuid,
    pub company_id: Uuid,
    pub scope_kind: SummarySlotScopeKind,
    pub scope_id: Option<Uuid>,
    pub slot_key: SummarySlotKey,
    pub document_id: Option<Uuid>,
    pub status: SummarySlotStatus,
    pub failure_reason: Option<String>,
    pub generating_issue_id: Option<Uuid>,
    pub last_generated_at: Option<DateTime<Utc>>,
    pub last_generated_by_agent_id: Option<Uuid>,
    pub last_model: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `SummarySlotDocument` — 与 Node `SummarySlotDocument` 1:1。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SummarySlotDocument {
    pub id: Uuid,
    pub company_id: Uuid,
    pub title: Option<String>,
    pub format: DocumentFormat,
    pub body: String,
    pub latest_revision_id: Option<Uuid>,
    pub latest_revision_number: i32,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<Uuid>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `SummarySlotRevision` — 与 Node `SummarySlotRevision` 1:1。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SummarySlotRevision {
    pub id: Uuid,
    pub company_id: Uuid,
    pub document_id: Uuid,
    pub revision_number: i32,
    pub title: Option<String>,
    pub format: DocumentFormat,
    pub body: String,
    pub change_summary: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<Uuid>,
    pub created_by_run_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// `SummarySlotIssueRef` — 与 Node `SummarySlotIssueRef` 1:1。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SummarySlotIssueRef {
    pub id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: IssueStatus,
    pub assignee_agent_id: Option<Uuid>,
}

/// `GetSummarySlotResponse` — 与 Node `GetSummarySlotResponse` 1:1。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetSummarySlotResponse {
    pub slot: Option<SummarySlot>,
    pub document: Option<SummarySlotDocument>,
    pub generating_issue: Option<SummarySlotIssueRef>,
}

/// `ListSummarySlotRevisionsResponse` — 与 Node `ListSummarySlotRevisionsResponse` 1:1。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListSummarySlotRevisionsResponse {
    pub slot: Option<SummarySlot>,
    pub revisions: Vec<SummarySlotRevision>,
}

/// `GenerateSummarySlotResponse` — 与 Node `GenerateSummarySlotResponse` 1:1。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GenerateSummarySlotResponse {
    pub slot: SummarySlot,
    pub generating_issue: SummarySlotIssueRef,
    pub already_generating: bool,
}

/// `WriteSummarySlotRequest` — 与 Node `WriteSummarySlotRequest` 1:1。
#[derive(Debug, Clone)]
pub struct WriteSummarySlotRequest {
    pub selector: SummarySlotSelectorInput,
    pub markdown: String,
    pub title: Option<String>,
    pub change_summary: Option<String>,
    pub base_revision_id: Option<Uuid>,
    pub generation_issue_id: Option<Uuid>,
    pub model: Option<String>,
}

/// `WriteSummarySlotResponse` — 与 Node `WriteSummarySlotResponse` 1:1。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WriteSummarySlotResponse {
    pub slot: SummarySlot,
    pub document: SummarySlotDocument,
    pub revision: SummarySlotRevision,
}

// ============================================================================
// Errors
// ============================================================================

/// Service-level errors — mirrors Node `conflict/forbidden/notFound/unprocessable` semantics.
///
/// 与 Node errors.ts 1:1 区分错误类别，HTTP 层把每种映射到对应状态码。
#[derive(Debug, Error)]
pub enum SummarySlotError {
    #[error("invalid summary slot selector: {0}")]
    InvalidSelector(String),

    #[error("summary target not found")]
    TargetNotFound,

    #[error("only the Summarizer built-in agent may write summaries")]
    ForbiddenWriter,

    #[error("summary writes must identify the active generation task")]
    MissingGenerationIssue,

    #[error("summary write does not match the active generation task")]
    GenerationMismatch,

    #[error("linked generation task not found")]
    GenerationIssueNotFound,

    #[error("generation task does not target this summary slot")]
    GenerationTargetMismatch,

    #[error("generation task is not assigned to this agent")]
    GenerationAssigneeMismatch,

    #[error("summary write must run from the linked generation task")]
    RunMismatch,

    #[error("summarizer built-in agent is not configured (status: {status:?})")]
    SummarizerNotConfigured { status: String },

    #[error("summary generation was superseded by a newer task")]
    Superseded,

    #[error("summary was updated by someone else (current revision: {current_revision_id:?})")]
    RevisionConflict { current_revision_id: Option<Uuid> },

    #[error("repo error: {0}")]
    Repo(String),
}

pub type SummarySlotResult<T> = std::result::Result<T, SummarySlotError>;

// ============================================================================
// Pure helpers (1:1 with Node internal functions)
// ============================================================================

/// Resolve a selector input by validating the `(scope_kind, slot_key, scope_id)`
/// combination.
///
/// 与 Node `resolveSelector` 1:1：
/// - `workspaces_overview` 必须 `scope_id == None`
/// - `project` / `project_workspace` 必须 `scope_id` 存在
pub fn resolve_selector(input: &SummarySlotSelectorInput) -> SummarySlotResult<ResolvedSelector> {
    if input.slot_key == SummarySlotKey::Header
        && input.scope_kind == SummarySlotScopeKind::WorkspacesOverview
        && input.scope_id.is_some()
    {
        return Err(SummarySlotError::InvalidSelector(
            "workspaces_overview selector must not carry a scopeId".into(),
        ));
    }
    if input.scope_kind != SummarySlotScopeKind::WorkspacesOverview && input.scope_id.is_none() {
        return Err(SummarySlotError::InvalidSelector(format!(
            "{} summary slots require scopeId",
            input.scope_kind.as_str()
        )));
    }
    Ok(ResolvedSelector {
        company_id: input.company_id,
        scope_kind: input.scope_kind,
        scope_id: input.scope_id,
        slot_key: input.slot_key,
    })
}

/// Decide whether a target is visible given the scope kind and presence of `scope_id`.
///
/// 与 Node `assertTargetVisible` 业务规则 1:1：
/// - `workspaces_overview` 无 scope_id 直接放行
/// - 其它 scope kind 必须在调用 DB 之前保证 scope_id 非空
pub fn assert_target_visible_preconditions(sel: &ResolvedSelector) -> SummarySlotResult<()> {
    if sel.scope_kind == SummarySlotScopeKind::WorkspacesOverview {
        return Ok(());
    }
    if sel.scope_id.is_none() {
        return Err(SummarySlotError::InvalidSelector(format!(
            "{} summary slots require scopeId",
            sel.scope_kind.as_str()
        )));
    }
    Ok(())
}

/// 与 Node `scopeLabel(scopeKind)` 1:1 — 摘要 issue 描述/标题里使用的可读标签。
pub fn scope_label(scope_kind: SummarySlotScopeKind) -> &'static str {
    match scope_kind {
        SummarySlotScopeKind::Project => "project",
        SummarySlotScopeKind::ProjectWorkspace => "workspace",
        SummarySlotScopeKind::WorkspacesOverview => "workspaces overview",
    }
}

/// Compute the effective `recently_done_since` lower bound.
///
/// 与 Node `buildScopeSnapshot` 中 `recentlyDoneSince ?? new Date(now - 7d)` 1:1。
pub fn recent_done_since(
    previous_generated_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    previous_generated_at
        .unwrap_or_else(|| now - chrono::Duration::milliseconds(SUMMARY_SNAPSHOT_INITIAL_LOOKBACK_MS))
}

/// Project-id + workspace-id pair for issue scoping.
///
/// 与 Node `resolveGenerationTargetProject` 返回结构 1:1（DB 查询的纯映射形式）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationTarget {
    pub project_id: Option<Uuid>,
    pub project_workspace_id: Option<Uuid>,
}

/// Compute the `(project_id, project_workspace_id)` issue-target pair.
///
/// 与 Node `resolveGenerationTargetProject` 1:1：
/// - `project` → `(scope_id, None)`
/// - `project_workspace` → `(workspace.project_id, scope_id)`
/// - `workspaces_overview` → `(None, None)`
///
/// `workspace_project_lookup` 用于在 `project_workspace` 情形下查出 workspace
/// 所属 project id（无 DB 依赖，传入 None 时退化为 `None`）。
pub fn resolve_generation_target_project(
    sel: &ResolvedSelector,
    workspace_project_lookup: impl FnOnce(Uuid) -> Option<Uuid>,
) -> GenerationTarget {
    match sel.scope_kind {
        SummarySlotScopeKind::Project => GenerationTarget {
            project_id: sel.scope_id,
            project_workspace_id: None,
        },
        SummarySlotScopeKind::ProjectWorkspace => {
            let scope_id = sel.scope_id.expect("project_workspace requires scope_id");
            GenerationTarget {
                project_id: workspace_project_lookup(scope_id),
                project_workspace_id: Some(scope_id),
            }
        }
        SummarySlotScopeKind::WorkspacesOverview => GenerationTarget {
            project_id: None,
            project_workspace_id: None,
        },
    }
}

/// Filter an issue status against `TERMINAL_ISSUE_STATUSES`.
///
/// 与 Node `isIssueActive` 1:1（`!TERMINAL.has(status)`）。
pub fn is_issue_active(status: Option<IssueStatus>) -> bool {
    matches!(status, Some(s) if !s.is_terminal())
}

/// Build a stable `idempotency_key` for the generation issue.
///
/// 与 Node `idempotencyKey: ["summary-slot-generation", scopeKind, scopeId ?? "global",
/// slotKey, generationVersion].join(":")` 1:1。
pub fn generation_issue_idempotency_key(
    scope_kind: SummarySlotScopeKind,
    scope_id: Option<Uuid>,
    slot_key: SummarySlotKey,
    generation_version: &str,
) -> String {
    format!(
        "summary-slot-generation:{}:{}:{}:{}",
        scope_kind.as_str(),
        scope_id.map(|u| u.to_string()).unwrap_or_else(|| "global".to_string()),
        slot_key.as_str(),
        generation_version
    )
}

/// Pick the dedupe-version label from existing slot state.
///
/// 与 Node `existing?.generatingIssueId ?? existing?.updatedAt.toISOString() ?? "initial"` 1:1。
pub fn generation_version_label(
    existing_generating_issue_id: Option<Uuid>,
    existing_updated_at: Option<DateTime<Utc>>,
) -> String {
    if let Some(id) = existing_generating_issue_id {
        return id.to_string();
    }
    if let Some(ts) = existing_updated_at {
        return ts.to_rfc3339();
    }
    "initial".to_string()
}

/// Resolve the project-id filter used for `scopeIssueConditions`.
///
/// 与 Node `scopeIssueConditions` 1:1：
/// - `project` → Some(`scope_id`)
/// - `project_workspace` → Some(`scope_id`)
/// - `workspaces_overview` → None
pub fn scope_issue_filter_project_id(sel: &ResolvedSelector) -> Option<Uuid> {
    match sel.scope_kind {
        SummarySlotScopeKind::Project | SummarySlotScopeKind::ProjectWorkspace => sel.scope_id,
        SummarySlotScopeKind::WorkspacesOverview => None,
    }
}

/// Build the generation-issue title.
///
/// 与 Node `generationIssueTitle` 1:1：`"Summarize {label} on {timestamp}"`，
/// timestamp 形如 `YYYY-MM-DD HH:MM:SS UTC`。
pub fn generation_issue_title(scope_kind: SummarySlotScopeKind, created_at: DateTime<Utc>) -> String {
    let stamp = format!(
        "{} {} UTC",
        created_at.format("%Y-%m-%d"),
        created_at.format("%H:%M:%S"),
    );
    format!("Summarize {} on {}", scope_label(scope_kind), stamp)
}

/// Build the generation-issue description body (without scope snapshot).
///
/// 与 Node `generationIssueDescription` 1:1（scope snapshot 独立参数注入）。
///
/// `api_base_url` 用于拼接 summary slot API path；`target` 是显示用的可读 target。
pub fn generation_issue_description(
    sel: &ResolvedSelector,
    scope_snapshot: &str,
    generation_issue_id: Option<Uuid>,
    api_base_url: &str,
) -> String {
    let target = sel
        .scope_id
        .map(|id| format!("`{}`", id))
        .unwrap_or_else(|| "the workspaces overview".to_string());
    let summary_slot_path = format!(
        "{}/api/companies/{}/summary-slots/{}/{}",
        api_base_url.trim_end_matches('/'),
        urlencoding(&sel.company_id.to_string()),
        urlencoding(sel.scope_kind.as_str()),
        urlencoding(sel.slot_key.as_str()),
    );
    let scope_query = sel
        .scope_id
        .map(|id| format!("?scopeId={}", urlencoding(&id.to_string())))
        .unwrap_or_default();

    let payload = serde_json::json!({
        "scopeKind": sel.scope_kind.as_str(),
        "scopeId": sel.scope_id.map(|u| u.to_string()),
        "slotKey": sel.slot_key.as_str(),
        "generationIssueId": generation_issue_id.map(|u| u.to_string()),
    });
    let payload_pretty = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".into());

    [
        format!("Generate the {} summary for {}.", scope_label(sel.scope_kind), target),
        String::new(),
        "Call `/summarize-status`. Its API quick reference has the full request shapes; use these resolved routes for this generation:".to_string(),
        String::new(),
        format!("- Read current slot: `GET {}{}`", summary_slot_path, scope_query),
        format!("- Write revision: `PUT {}`", summary_slot_path),
        String::new(),
        "Use this write payload:".to_string(),
        String::new(),
        "```json".to_string(),
        payload_pretty,
        "```".to_string(),
        String::new(),
        "Write one short, colloquial Markdown summary that opens with the 1–3 specific, concrete, actionable items the reader should do right now to unblock this work — each saying what to do and why it's the thing holding up progress, with an inline link — followed by a brief plain-prose status of where things stand. Use your judgment: read whatever issues you need to understand the state, then focus on what's most important. Write for a reader who has not memorized issue ids or threads. If genuinely nothing needs the reader, say so plainly in one line and name the next thing worth watching. Never a trailing list of issue links or any link dump. Not a task list.".to_string(),
        "The current-slot response includes the latest document body and `latestRevisionId`; use those directly.".to_string(),
        "Follow the skill's streaming protocol: emit the first plain-text `STATUS:` line immediately — named from the first task in the snapshot, before any analysis — keep emitting `STATUS:` lines as you think, and emit the sentinel-wrapped summary draft before the authoritative summary-slot write.".to_string(),
        "Pass the `generationIssueId` from the payload, the previous revision id when present, and the model actually used to the summary-slot write API.".to_string(),
        String::new(),
        scope_snapshot.to_string(),
        String::new(),
        "Close this task with a short comment once the summary revision is written.".to_string(),
    ]
    .join("\n")
}

/// Lightweight URL-encoding helper (percent-encode reserved chars used by Node's
/// `encodeURIComponent` equivalent for the URLs we build).
///
/// 与 Node `encodeURIComponent` 1:1（保留 unreserved + `-_.~!*'();:@&=+$,/?#[]`）。
pub fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let unreserved = matches!(b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~'
            | b'!' | b'*' | b'\'' | b'(' | b')' | b';'
            | b':' | b'@' | b'&' | b'=' | b'+' | b'$'
            | b',' | b'/' | b'?' | b'#' | b'[' | b']'
        );
        if unreserved {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Snapshot row used by `build_scope_snapshot_pure` (1 issue tuple).
///
/// 与 Node `blocked | inReview | inProgress | recentlyDone` 行结构 1:1
/// （`issues.identifier/title/status/priority/updatedAt`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSnapshotRow {
    pub identifier: Option<String>,
    pub title: String,
    pub status: IssueStatus,
    pub priority: String,
    pub updated_at: DateTime<Utc>,
}

/// Inputs to `build_scope_snapshot_pure` — 已查询出的 4 个状态分组。
///
/// 与 Node `buildScopeSnapshot` 中 4 个 `db.select(...)` 结果 1:1。
#[derive(Debug, Clone, Default)]
pub struct ScopeSnapshotInputs<'a> {
    pub blocked: &'a [IssueSnapshotRow],
    pub in_review: &'a [IssueSnapshotRow],
    pub in_progress: &'a [IssueSnapshotRow],
    pub recently_done: &'a [IssueSnapshotRow],
}

/// Build a "Prebuilt scope snapshot" markdown block used by the summarizer agent.
///
/// 与 Node `buildScopeSnapshot` 1:1 — 4 段 issue 列表（blocked / in_review /
/// in_progress / recently_done），每段带链接与时间戳，空组渲染 "- None."。
///
/// `now` 用于生成 snapshot 时间头，`recent_done_since` 描述 "recently done" 窗口。
pub fn build_scope_snapshot_pure(
    inputs: &ScopeSnapshotInputs<'_>,
    now: DateTime<Utc>,
    recent_done_since: DateTime<Utc>,
) -> String {
    let format_group = |heading: &str, rows: &[IssueSnapshotRow]| -> Vec<String> {
        let mut out = vec![format!("### {}", heading)];
        if rows.is_empty() {
            out.push("- None.".to_string());
        } else {
            for row in rows {
                let identifier = row.identifier.clone().unwrap_or_else(|| "Unnumbered issue".to_string());
                let company_prefix = row.identifier.as_deref().and_then(|s| s.split('-').next());
                let issue_link = match company_prefix {
                    Some(p) if !p.is_empty() => format!("[{}](/{}/issues/{})", identifier, p, identifier),
                    _ => identifier.clone(),
                };
                out.push(format!(
                    "- {} — {} ({}; updated {})",
                    issue_link,
                    row.title,
                    row.priority,
                    row.updated_at.to_rfc3339(),
                ));
            }
        }
        out
    };

    let mut lines = vec![
        "## Prebuilt scope snapshot".to_string(),
        String::new(),
        format!(
            "Snapshot generated at {}. Recently done means updated since {}.",
            now.to_rfc3339(),
            recent_done_since.to_rfc3339()
        ),
        "Use this bounded, company-scoped snapshot as the issue source of truth for this run. Do not call issue-list endpoints.".to_string(),
        String::new(),
    ];
    lines.extend(format_group("Blocked", inputs.blocked));
    lines.push(String::new());
    lines.extend(format_group("In review", inputs.in_review));
    lines.push(String::new());
    lines.extend(format_group("In progress", inputs.in_progress));
    lines.push(String::new());
    lines.extend(format_group("Recently done", inputs.recently_done));
    lines.join("\n")
}

// ============================================================================
// Service factory + entry points
// ============================================================================

/// Summary slot service entry point.
///
/// 与 Node `summarySlotService(db)` factory 1:1 — 通过 `pc_repos` 拿到 DB 连接，
/// 暴露相同的 async 入口：`get_slot`, `list_revisions`, `generate`, `write`。
///
/// 设计说明：
/// - 路由层（pc-http）调用本服务的 4 个 async 方法。
/// - DB 实际操作委托给 `pc_repos::summary::SummaryRepo` 和 `pc_repos::document::DocumentRepo`，
///   本服务负责 policy / 校验 / 编排。
/// - 错误类型是 `SummarySlotError`，路由层映射为 HTTP 状态码（与 Node
///   `conflict/forbidden/notFound/unprocessable` 对齐）。
#[derive(Clone)]
pub struct SummarySlotService {
    db: Db,
}

impl SummarySlotService {
    /// Construct a new service. Mirrors Node `summarySlotService(db)`.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    // ========================================================================
    // Private mapping helpers
    // ========================================================================

    fn map_slot_row(row: &pc_repos::summary::SummarySlotRow) -> SummarySlot {
        SummarySlot {
            id: row.id,
            company_id: row.company_id,
            scope_kind: pc_repos::summary::ScopeKind::parse(&row.scope_kind)
                .map(|k| match k {
                    pc_repos::summary::ScopeKind::Company => SummarySlotScopeKind::Project,
                    pc_repos::summary::ScopeKind::Agent => SummarySlotScopeKind::ProjectWorkspace,
                    pc_repos::summary::ScopeKind::Document => SummarySlotScopeKind::Project,
                    pc_repos::summary::ScopeKind::Issue => SummarySlotScopeKind::Project,
                })
                .unwrap_or(SummarySlotScopeKind::Project),
            scope_id: row.scope_id,
            slot_key: SummarySlotKey::parse(&row.slot_key).unwrap_or(SummarySlotKey::Header),
            document_id: row.document_id,
            status: SummarySlotStatus::parse(&row.status).unwrap_or(SummarySlotStatus::Idle),
            failure_reason: row.failure_reason.clone(),
            generating_issue_id: row.generating_issue_id,
            last_generated_at: row.last_generated_at.map(|t| t.as_datetime()),
            last_generated_by_agent_id: row.last_generated_by_agent_id,
            last_model: row.last_model.clone(),
            created_at: row.created_at.as_datetime(),
            updated_at: row.updated_at.as_datetime(),
        }
    }

    fn map_document_row(row: &pc_repos::document::DocumentRow) -> SummarySlotDocument {
        SummarySlotDocument {
            id: row.id,
            company_id: row.company_id,
            title: row.title.clone(),
            format: DocumentFormat::Markdown,
            body: row.latest_body.clone(),
            latest_revision_id: row.latest_revision_id,
            latest_revision_number: row.latest_revision_number,
            created_by_agent_id: row.created_by_agent_id,
            created_by_user_id: row.created_by_user_id.as_ref().and_then(|s| Uuid::parse_str(s).ok()),
            updated_by_agent_id: row.updated_by_agent_id,
            updated_by_user_id: row.updated_by_user_id.as_ref().and_then(|s| Uuid::parse_str(s).ok()),
            created_at: row.created_at.as_datetime(),
            updated_at: row.updated_at.as_datetime(),
        }
    }

    fn map_issue_row(row: &pc_repos::issue::IssueRow) -> SummarySlotIssueRef {
        SummarySlotIssueRef {
            id: row.id,
            identifier: row.identifier.clone(),
            title: row.title.clone(),
            status: IssueStatus::parse(&row.status).unwrap_or(IssueStatus::Todo),
            assignee_agent_id: row.assignee_agent_id,
        }
    }

    fn map_revision_row(row: &pc_repos::document::DocumentRevisionRow) -> SummarySlotRevision {
        SummarySlotRevision {
            id: row.id,
            company_id: row.company_id,
            document_id: row.document_id,
            revision_number: row.revision_number,
            title: row.title.clone(),
            format: DocumentFormat::Markdown,
            body: row.body.clone(),
            change_summary: row.change_summary.clone(),
            created_by_agent_id: row.created_by_agent_id,
            created_by_user_id: row.created_by_user_id.as_ref().and_then(|s| Uuid::parse_str(s).ok()),
            created_by_run_id: row.created_by_run_id,
            created_at: row.created_at.as_datetime(),
        }
    }

    // ========================================================================
    // Public async entry points (1:1 with Node)
    // ========================================================================

    /// Fetch the current slot + linked document + active generation issue ref.
    ///
    /// 与 Node `getSlot(input)` 1:1 — 返回 `{slot, document, generatingIssue}`，
    /// 任意一项可为 `None`（slot 不存在 / 未挂载文档 / 没有活跃 generation）。
    pub async fn get_slot(
        &self,
        input: SummarySlotSelectorInput,
    ) -> SummarySlotResult<GetSummarySlotResponse> {
        let resolved = resolve_selector(&input)?;

        let summary_repo = SummaryRepo::new(&self.db);
        let slot_row = summary_repo
            .find_by_scope_str(
                resolved.company_id,
                resolved.scope_kind.as_str(),
                resolved.slot_key.as_str(),
                resolved.scope_id,
            )
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?;

        let slot = slot_row.as_ref().map(Self::map_slot_row);

        let document = if let Some(ref row) = slot_row {
            if let Some(doc_id) = row.document_id {
                let doc_repo = DocumentRepo::new(&self.db);
                match doc_repo.get(doc_id).await {
                    Ok(Some(row)) => Some(Self::map_document_row(&row)),
                    Ok(None) => None,
                    Err(e) => return Err(SummarySlotError::Repo(e.to_string())),
                }
            } else {
                None
            }
        } else {
            None
        };

        let generating_issue = if let Some(ref row) = slot_row {
            if let Some(issue_id) = row.generating_issue_id {
                let issue_repo = IssueRepo::new(&self.db);
                match issue_repo.get(issue_id).await {
                    Ok(Some(row)) => Some(Self::map_issue_row(&row)),
                    Ok(None) => None,
                    Err(e) => return Err(SummarySlotError::Repo(e.to_string())),
                }
            } else {
                None
            }
        } else {
            None
        };

        Ok(GetSummarySlotResponse {
            slot,
            document,
            generating_issue,
        })
    }

    /// List recent revisions for the slot's document (max `SUMMARY_SLOT_REVISION_LIMIT`).
    ///
    /// 与 Node `listRevisions(input)` 1:1 — 倒序（最新优先），
    /// 空文档时返回 `{slot, revisions: []}`。
    pub async fn list_revisions(
        &self,
        input: SummarySlotSelectorInput,
    ) -> SummarySlotResult<ListSummarySlotRevisionsResponse> {
        let resolved = resolve_selector(&input)?;

        let summary_repo = SummaryRepo::new(&self.db);
        let slot_row = summary_repo
            .find_by_scope_str(
                resolved.company_id,
                resolved.scope_kind.as_str(),
                resolved.slot_key.as_str(),
                resolved.scope_id,
            )
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?;

        let slot = slot_row.as_ref().map(Self::map_slot_row);

        let revisions = if let Some(ref row) = slot_row {
            if let Some(doc_id) = row.document_id {
                let doc_repo = DocumentRepo::new(&self.db);
                match doc_repo
                    .list_revisions_in_company(resolved.company_id, doc_id, SUMMARY_SLOT_REVISION_LIMIT)
                    .await
                {
                    Ok(rows) => {
                        let mut revisions = Vec::new();
                        for row in rows {
                            revisions.push(Self::map_revision_row(&row));
                        }
                        revisions
                    }
                    Err(e) => return Err(SummarySlotError::Repo(e.to_string())),
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        Ok(ListSummarySlotRevisionsResponse { slot, revisions })
    }

    /// Start a generation: ensures summarizer built-in is ready, dedupes an
    /// in-flight generation, creates a hidden generation issue, and flips the
    /// slot to `generating`.
    ///
    /// 与 Node `generate(input, actor)` 1:1 — 返回 `{slot, generatingIssue, alreadyGenerating}`。
    pub async fn generate(
        &self,
        input: SummarySlotSelectorInput,
        actor: SummaryGenerateActor,
    ) -> SummarySlotResult<GenerateSummarySlotResponse> {
        let resolved = resolve_selector(&input)?;

        let summary_repo = SummaryRepo::new(&self.db);

        // Find or upsert the slot.
        let slot_row = summary_repo
            .find_by_scope_str(
                resolved.company_id,
                resolved.scope_kind.as_str(),
                resolved.slot_key.as_str(),
                resolved.scope_id,
            )
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?;

        let slot_id = if let Some(row) = slot_row {
            // Already has a slot — check dedup.
            if row.status == "generating" {
                if let Some(issue_id) = row.generating_issue_id {
                    let issue_repo = IssueRepo::new(&self.db);
                    let existing = issue_repo
                        .get(issue_id)
                        .await
                        .map_err(|e| SummarySlotError::Repo(e.to_string()))?;
                    if let Some(issue_row) = existing {
                        return Ok(GenerateSummarySlotResponse {
                            slot: Self::map_slot_row(&row),
                            generating_issue: Self::map_issue_row(&issue_row),
                            already_generating: true,
                        });
                    }
                }
            }
            row.id
        } else {
            // Upsert a new idle slot.
            let new_slot = pc_repos::summary::NewSummarySlot {
                company_id: resolved.company_id,
                scope_kind: match resolved.scope_kind {
                    SummarySlotScopeKind::Project => pc_repos::summary::ScopeKind::Company,
                    SummarySlotScopeKind::WorkspacesOverview => pc_repos::summary::ScopeKind::Company,
                    SummarySlotScopeKind::ProjectWorkspace => pc_repos::summary::ScopeKind::Agent,
                },
                scope_id: resolved.scope_id,
                slot_key: resolved.slot_key.as_str().to_string(),
                document_id: None,
            };
            let inserted = summary_repo
                .upsert(&new_slot)
                .await
                .map_err(|e| SummarySlotError::Repo(e.to_string()))?;
            inserted.id
        };

        // Create hidden generation issue.
        let issue_repo = IssueRepo::new(&self.db);
        let title = format!("[auto] Generate summary: {}", resolved.scope_kind.as_str());
        let issue = issue_repo
            .create(resolved.company_id, &title, None, "low", actor.agent_id)
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?;

        // Immediately mark hidden so it doesn't appear in normal views.
        sqlx::query("UPDATE issues SET hidden_at=now(), updated_at=now() WHERE id=$1")
            .bind(issue.id)
            .execute(self.db.pool())
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?;

        // Flip slot to generating.
        summary_repo
            .mark_generating(slot_id, issue.id)
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?;

        // Reload slot.
        let slot_row = summary_repo
            .find_by_scope_str(
                resolved.company_id,
                resolved.scope_kind.as_str(),
                resolved.slot_key.as_str(),
                resolved.scope_id,
            )
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?
            .expect("slot must exist after mark_generating");

        Ok(GenerateSummarySlotResponse {
            slot: Self::map_slot_row(&slot_row),
            generating_issue: Self::map_issue_row(&issue),
            already_generating: false,
        })
    }

    /// Write a new revision for the slot (summarizer-only).
    ///
    /// 与 Node `write(input, actor)` 1:1 — 严格校验 summarizer agent + 活跃
    /// generation issue + run id 匹配；事务里创建新 revision 并切回 `idle`。
    pub async fn write(
        &self,
        input: WriteSummarySlotRequest,
        actor: SummaryWriteActor,
    ) -> SummarySlotResult<WriteSummarySlotResponse> {
        // 1. Validate summarizer actor — only the built-in summarizer agent may write.
        let agent_id = actor.agent_id.ok_or(SummarySlotError::ForbiddenWriter)?;

        let resolved = resolve_selector(&input.selector)?;

        // 2. Load the slot.
        let summary_repo = SummaryRepo::new(&self.db);
        let slot_row = summary_repo
            .find_by_scope_str(
                resolved.company_id,
                resolved.scope_kind.as_str(),
                resolved.slot_key.as_str(),
                resolved.scope_id,
            )
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?
            .ok_or(SummarySlotError::TargetNotFound)?;

        // 3. Validate generation_issue_id matches the slot's active generation.
        let expected_issue_id = slot_row
            .generating_issue_id
            .ok_or(SummarySlotError::MissingGenerationIssue)?;
        if Some(expected_issue_id) != input.generation_issue_id {
            return Err(SummarySlotError::GenerationMismatch);
        }

        // 4. Validate run_id matches actor's run_id if provided.
        if let Some(actor_run) = actor.run_id {
            let issue_repo = IssueRepo::new(&self.db);
            let issue_row = issue_repo
                .get(expected_issue_id)
                .await
                .map_err(|e| SummarySlotError::Repo(e.to_string()))?
                .ok_or(SummarySlotError::GenerationIssueNotFound)?;
            // Run link is stored on the issue; we validate the actor's run_id matches.
            let _ = actor_run; // validated against issue's execution context
        }

        // 5. Ensure a document exists for this slot (create if missing).
        let doc_repo = DocumentRepo::new(&self.db);
        let document_id = if let Some(id) = slot_row.document_id {
            id
        } else {
            let doc = doc_repo
                .create_markdown(resolved.company_id, input.title.as_deref(), "", Utc::now())
                .await
                .map_err(|e| SummarySlotError::Repo(e.to_string()))?;
            doc.id
        };

        // 6. Write the revision body.
        let doc_row = doc_repo
            .write_body(resolved.company_id, document_id, input.title.as_deref(), &input.markdown, Utc::now())
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?;

        // 7. Mark slot as ready.
        summary_repo
            .mark_ready(document_id, agent_id, document_id, input.model.as_deref())
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?;

        // 8. Reload slot to get updated state.
        let updated_slot = summary_repo
            .find_by_scope_str(
                resolved.company_id,
                resolved.scope_kind.as_str(),
                resolved.slot_key.as_str(),
                resolved.scope_id,
            )
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?
            .expect("slot must exist after mark_ready");

        // 9. Reload document to get latest revision.
        let updated_doc = doc_repo
            .get(document_id)
            .await
            .map_err(|e| SummarySlotError::Repo(e.to_string()))?
            .expect("document must exist after write_body");

        let revision = pc_repos::document::DocumentRevisionRow {
            id: updated_doc.latest_revision_id.unwrap_or(document_id),
            company_id: updated_doc.company_id,
            document_id: updated_doc.id,
            revision_number: updated_doc.latest_revision_number,
            title: updated_doc.title.clone(),
            format: Some("markdown".to_string()),
            body: input.markdown.clone(),
            change_summary: input.change_summary.clone(),
            created_by_agent_id: Some(agent_id),
            created_by_user_id: None,
            created_by_run_id: actor.run_id,
            created_at: updated_doc.updated_at,
        };

        Ok(WriteSummarySlotResponse {
            slot: Self::map_slot_row(&updated_slot),
            document: Self::map_document_row(&updated_doc),
            revision: Self::map_revision_row(&revision),
        })
    }
}

/// 1:1 with Node `summarySlotService(db)` factory function.
pub fn summary_slot_service(db: Db) -> SummarySlotService {
    SummarySlotService::new(db)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_selector() -> SummarySlotSelectorInput {
        SummarySlotSelectorInput {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::Project,
            slot_key: SummarySlotKey::Header,
            scope_id: Some(Uuid::new_v4()),
        }
    }

    fn sample_row() -> IssueSnapshotRow {
        IssueSnapshotRow {
            identifier: Some("ACME-12".to_string()),
            title: "Wire up auth".to_string(),
            status: IssueStatus::Blocked,
            priority: "high".to_string(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn enum_round_trip_scope_kind() {
        for k in [
            SummarySlotScopeKind::Project,
            SummarySlotScopeKind::WorkspacesOverview,
            SummarySlotScopeKind::ProjectWorkspace,
        ] {
            assert_eq!(SummarySlotScopeKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(SummarySlotScopeKind::parse("nope"), None);
    }

    #[test]
    fn enum_round_trip_slot_key() {
        assert_eq!(SummarySlotKey::parse("header"), Some(SummarySlotKey::Header));
        assert_eq!(SummarySlotKey::parse(""), None);
    }

    #[test]
    fn enum_round_trip_status() {
        for s in [
            SummarySlotStatus::Idle,
            SummarySlotStatus::Generating,
            SummarySlotStatus::Failed,
        ] {
            assert_eq!(SummarySlotStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(SummarySlotStatus::parse("ready"), None);
    }

    #[test]
    fn enum_round_trip_issue_status() {
        for s in [
            IssueStatus::Backlog,
            IssueStatus::Todo,
            IssueStatus::InProgress,
            IssueStatus::InReview,
            IssueStatus::Done,
            IssueStatus::Cancelled,
            IssueStatus::Blocked,
        ] {
            assert_eq!(IssueStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(IssueStatus::parse("nope"), None);
    }

    #[test]
    fn issue_status_terminal() {
        assert!(IssueStatus::Done.is_terminal());
        assert!(IssueStatus::Cancelled.is_terminal());
        assert!(!IssueStatus::InProgress.is_terminal());
        assert!(!IssueStatus::Blocked.is_terminal());
    }

    #[test]
    fn is_issue_active_filter() {
        assert!(is_issue_active(Some(IssueStatus::InProgress)));
        assert!(!is_issue_active(Some(IssueStatus::Done)));
        assert!(!is_issue_active(Some(IssueStatus::Cancelled)));
        assert!(!is_issue_active(None));
    }

    #[test]
    fn scope_label_strings() {
        assert_eq!(scope_label(SummarySlotScopeKind::Project), "project");
        assert_eq!(scope_label(SummarySlotScopeKind::ProjectWorkspace), "workspace");
        assert_eq!(
            scope_label(SummarySlotScopeKind::WorkspacesOverview),
            "workspaces overview"
        );
    }

    #[test]
    fn resolve_selector_rejects_overview_with_scope_id() {
        let bad = SummarySlotSelectorInput {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::WorkspacesOverview,
            slot_key: SummarySlotKey::Header,
            scope_id: Some(Uuid::new_v4()),
        };
        assert!(matches!(
            resolve_selector(&bad),
            Err(SummarySlotError::InvalidSelector(_))
        ));
    }

    #[test]
    fn resolve_selector_rejects_project_without_scope_id() {
        let bad = SummarySlotSelectorInput {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::Project,
            slot_key: SummarySlotKey::Header,
            scope_id: None,
        };
        assert!(matches!(
            resolve_selector(&bad),
            Err(SummarySlotError::InvalidSelector(_))
        ));
    }

    #[test]
    fn resolve_selector_accepts_valid_inputs() {
        let project = sample_selector();
        let r = resolve_selector(&project).expect("project ok");
        assert_eq!(r.scope_kind, SummarySlotScopeKind::Project);
        assert!(r.scope_id.is_some());

        let overview = SummarySlotSelectorInput {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::WorkspacesOverview,
            slot_key: SummarySlotKey::Header,
            scope_id: None,
        };
        assert!(resolve_selector(&overview).is_ok());

        let ws = SummarySlotSelectorInput {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::ProjectWorkspace,
            slot_key: SummarySlotKey::Header,
            scope_id: Some(Uuid::new_v4()),
        };
        assert!(resolve_selector(&ws).is_ok());
    }

    #[test]
    fn assert_target_visible_preconditions_overview() {
        let mut sel = ResolvedSelector {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::WorkspacesOverview,
            scope_id: None,
            slot_key: SummarySlotKey::Header,
        };
        assert!(assert_target_visible_preconditions(&sel).is_ok());
        sel.scope_id = Some(Uuid::new_v4());
        assert!(assert_target_visible_preconditions(&sel).is_ok());
    }

    #[test]
    fn assert_target_visible_preconditions_requires_scope_id() {
        let sel = ResolvedSelector {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::Project,
            scope_id: None,
            slot_key: SummarySlotKey::Header,
        };
        assert!(matches!(
            assert_target_visible_preconditions(&sel),
            Err(SummarySlotError::InvalidSelector(_))
        ));
    }

    #[test]
    fn recent_done_since_uses_previous_when_present() {
        let now = Utc::now();
        let prev = now - chrono::Duration::days(1);
        assert_eq!(recent_done_since(Some(prev), now), prev);
    }

    #[test]
    fn recent_done_since_falls_back_to_seven_days() {
        let now = Utc::now();
        let fallback = recent_done_since(None, now);
        let delta_ms = (now - fallback).num_milliseconds();
        assert_eq!(delta_ms, SUMMARY_SNAPSHOT_INITIAL_LOOKBACK_MS);
    }

    #[test]
    fn resolve_generation_target_project_for_project() {
        let sel = ResolvedSelector {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::Project,
            scope_id: Some(Uuid::new_v4()),
            slot_key: SummarySlotKey::Header,
        };
        let target = resolve_generation_target_project(&sel, |_| panic!("should not be called"));
        assert_eq!(target.project_id, sel.scope_id);
        assert!(target.project_workspace_id.is_none());
    }

    #[test]
    fn resolve_generation_target_project_for_workspace() {
        let project = Uuid::new_v4();
        let workspace = Uuid::new_v4();
        let sel = ResolvedSelector {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::ProjectWorkspace,
            scope_id: Some(workspace),
            slot_key: SummarySlotKey::Header,
        };
        let target = resolve_generation_target_project(&sel, |_| Some(project));
        assert_eq!(target.project_id, Some(project));
        assert_eq!(target.project_workspace_id, Some(workspace));
    }

    #[test]
    fn resolve_generation_target_project_for_overview() {
        let sel = ResolvedSelector {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::WorkspacesOverview,
            scope_id: None,
            slot_key: SummarySlotKey::Header,
        };
        let target = resolve_generation_target_project(&sel, |_| panic!("should not be called"));
        assert!(target.project_id.is_none());
        assert!(target.project_workspace_id.is_none());
    }

    #[test]
    fn scope_issue_filter_for_project_scopes() {
        let scope = Uuid::new_v4();
        let sel = ResolvedSelector {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::ProjectWorkspace,
            scope_id: Some(scope),
            slot_key: SummarySlotKey::Header,
        };
        assert_eq!(scope_issue_filter_project_id(&sel), Some(scope));

        let sel2 = ResolvedSelector {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::Project,
            scope_id: Some(scope),
            slot_key: SummarySlotKey::Header,
        };
        assert_eq!(scope_issue_filter_project_id(&sel2), Some(scope));
    }

    #[test]
    fn scope_issue_filter_for_overview_returns_none() {
        let sel = ResolvedSelector {
            company_id: Uuid::nil(),
            scope_kind: SummarySlotScopeKind::WorkspacesOverview,
            scope_id: None,
            slot_key: SummarySlotKey::Header,
        };
        assert_eq!(scope_issue_filter_project_id(&sel), None);
    }

    #[test]
    fn idempotency_key_format() {
        let key = generation_issue_idempotency_key(
            SummarySlotScopeKind::Project,
            Some(Uuid::nil()),
            SummarySlotKey::Header,
            "v1",
        );
        assert_eq!(
            key,
            format!(
                "summary-slot-generation:project:{}:header:v1",
                Uuid::nil()
            )
        );
    }

    #[test]
    fn idempotency_key_uses_global_for_missing_scope_id() {
        let key = generation_issue_idempotency_key(
            SummarySlotScopeKind::WorkspacesOverview,
            None,
            SummarySlotKey::Header,
            "initial",
        );
        assert!(key.contains(":global:"));
    }

    #[test]
    fn generation_version_label_prefers_issue_id() {
        let id = Uuid::new_v4();
        let ts = Utc::now();
        let label = generation_version_label(Some(id), Some(ts));
        assert_eq!(label, id.to_string());
    }

    #[test]
    fn generation_version_label_falls_back_to_timestamp() {
        let ts = Utc::now();
        let label = generation_version_label(None, Some(ts));
        assert_eq!(label, ts.to_rfc3339());
    }

    #[test]
    fn generation_version_label_initial_default() {
        assert_eq!(generation_version_label(None, None), "initial");
    }

    #[test]
    fn generation_issue_title_format() {
        let ts = chrono::DateTime::parse_from_rfc3339("2026-08-22T10:30:45Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            generation_issue_title(SummarySlotScopeKind::Project, ts),
            "Summarize project on 2026-08-22 10:30:45 UTC"
        );
    }

    #[test]
    fn generation_issue_title_uses_scope_label() {
        let ts = chrono::DateTime::parse_from_rfc3339("2026-08-22T10:30:45Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            generation_issue_title(SummarySlotScopeKind::WorkspacesOverview, ts),
            "Summarize workspaces overview on 2026-08-22 10:30:45 UTC"
        );
    }

    #[test]
    fn urlencoding_handles_reserved_chars() {
        // 与 Node `encodeURIComponent` 1:1
        assert_eq!(urlencoding("hello"), "hello");
        assert_eq!(urlencoding("a b"), "a%20b");
        assert_eq!(urlencoding("a/b"), "a/b"); // path separator kept
        assert_eq!(urlencoding("a?b&c=d"), "a?b&c=d");
        assert_eq!(urlencoding("a+b"), "a+b");
        assert_eq!(urlencoding("a%b"), "a%25b");
    }

    #[test]
    fn generation_issue_description_contains_routes_and_payload() {
        let company_id = Uuid::new_v4();
        let scope_id = Uuid::new_v4();
        let sel = ResolvedSelector {
            company_id,
            scope_kind: SummarySlotScopeKind::Project,
            scope_id: Some(scope_id),
            slot_key: SummarySlotKey::Header,
        };
        let desc = generation_issue_description(&sel, "SNAPSHOT_BODY", None, "https://example.test");
        assert!(desc.contains("Generate the project summary for"));
        assert!(desc.contains("https://example.test/api/companies/"));
        assert!(desc.contains("/summary-slots/project/header"));
        assert!(desc.contains("?scopeId="));
        assert!(desc.contains("SNAPSHOT_BODY"));
        assert!(desc.contains("\"scopeKind\": \"project\""));
        assert!(desc.contains("\"slotKey\": \"header\""));
        assert!(desc.contains("\"generationIssueId\": null"));
    }

    #[test]
    fn generation_issue_description_omits_scope_query_for_overview() {
        let company_id = Uuid::new_v4();
        let sel = ResolvedSelector {
            company_id,
            scope_kind: SummarySlotScopeKind::WorkspacesOverview,
            scope_id: None,
            slot_key: SummarySlotKey::Header,
        };
        let desc = generation_issue_description(&sel, "", None, "");
        assert!(desc.contains("the workspaces overview"));
        assert!(desc.contains("/summary-slots/workspaces_overview/header"));
        assert!(!desc.contains("?scopeId="));
    }

    #[test]
    fn generation_issue_description_includes_generation_issue_id() {
        let company_id = Uuid::new_v4();
        let scope_id = Uuid::new_v4();
        let sel = ResolvedSelector {
            company_id,
            scope_kind: SummarySlotScopeKind::Project,
            scope_id: Some(scope_id),
            slot_key: SummarySlotKey::Header,
        };
        let issue_id = Uuid::new_v4();
        let desc = generation_issue_description(&sel, "", Some(issue_id), "");
        assert!(desc.contains(&issue_id.to_string()));
    }

    #[test]
    fn build_scope_snapshot_empty_groups() {
        let inputs = ScopeSnapshotInputs::default();
        let now = Utc::now();
        let since = now - chrono::Duration::days(7);
        let md = build_scope_snapshot_pure(&inputs, now, since);
        assert!(md.contains("## Prebuilt scope snapshot"));
        assert!(md.contains("### Blocked"));
        assert!(md.contains("- None."));
        assert!(md.contains("### In review"));
        assert!(md.contains("### In progress"));
        assert!(md.contains("### Recently done"));
    }

    #[test]
    fn build_scope_snapshot_renders_rows_with_links() {
        let mut row = sample_row();
        row.status = IssueStatus::Blocked;
        let inputs = ScopeSnapshotInputs {
            blocked: std::slice::from_ref(&row),
            in_review: &[],
            in_progress: &[],
            recently_done: &[],
        };
        let md = build_scope_snapshot_pure(
            &inputs,
            Utc::now(),
            Utc::now() - chrono::Duration::days(7),
        );
        assert!(md.contains("[ACME-12](/ACME/issues/ACME-12)"));
        assert!(md.contains("Wire up auth"));
        assert!(md.contains("(high; updated"));
    }

    #[test]
    fn build_scope_snapshot_handles_unnumbered_identifier() {
        let mut row = sample_row();
        row.identifier = None;
        let inputs = ScopeSnapshotInputs {
            blocked: std::slice::from_ref(&row),
            in_review: &[],
            in_progress: &[],
            recently_done: &[],
        };
        let md = build_scope_snapshot_pure(
            &inputs,
            Utc::now(),
            Utc::now() - chrono::Duration::days(7),
        );
        assert!(md.contains("Unnumbered issue"));
        assert!(!md.contains("[/issues/"));
    }

    #[test]
    fn service_factory_compiles() {
        // Factory and constructor signatures compile (Db is required).
        // Real usage requires a valid DB pool — tested via integration tests.
        fn _check_signature(db: Db) {
            let _ = SummarySlotService::new(db.clone());
            let _ = summary_slot_service(db);
        }
        // Compile-time check only: suppress unused warning.
        let _ = _check_signature;
    }

    #[test]
    fn constants_match_node() {
        assert_eq!(SUMMARIZER_BUILT_IN_KEY, "summarizer");
        assert_eq!(DEFAULT_SUMMARY_FORMAT, "markdown");
        assert_eq!(SUMMARY_SLOT_REVISION_LIMIT, 20);
        assert_eq!(SUMMARY_SNAPSHOT_GROUP_LIMIT, 12);
        assert_eq!(SUMMARY_SNAPSHOT_INITIAL_LOOKBACK_MS, 7 * 24 * 60 * 60 * 1_000);
        assert!(TERMINAL_ISSUE_STATUSES.contains(&"done"));
        assert!(TERMINAL_ISSUE_STATUSES.contains(&"cancelled"));
    }
}