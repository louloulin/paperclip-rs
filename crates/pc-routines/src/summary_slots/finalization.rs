#![forbid(unsafe_code)]

//! Slot finalization helpers — 1:1 port of Node
//! `paperclip/server/src/services/summary-slot-finalization.ts`.
//!
//! Responsibility:
//! - Mark any `summary_slots` whose `generating_issue_id` matches a just-terminal
//!   generation issue as `failed` and record the human-readable `failure_reason`.
//! - No-op for issues that aren't `done` / `cancelled`.
//!
//! 设计：
//! - `failure_reason_for_terminal_issue` 是纯文本函数，可独立测试。
//! - `finalize_summary_slots_for_terminal_issue` 走 `pc_repos::summary::SummaryRepo`
//!   提供的 SQL update；这里返回的 `FinalizationPatch` 描述了"要改什么"，
//!   上层（issue 终止副作用流水线）负责真正执行。
//!
//! 与 Node 1:1 对齐：
//! - 仅当 issue 状态 ∈ `TERMINAL_ISSUE_STATUSES = {done, cancelled}` 时执行。
//! - 更新条件：`company_id = issue.company_id` AND `generating_issue_id = issue.id` AND `status = 'generating'`。
//! - 返回所有被更新的 slot id（用于追溯/事件发布）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::{SummarySlotStatus, TERMINAL_ISSUE_STATUSES};

// ============================================================================
// Type definitions
// ============================================================================

/// Mirror of Node `TerminalGenerationIssue` — 仅 finalization 需要的子集。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalGenerationIssue {
    pub id: Uuid,
    pub company_id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
}

/// Failure-reason payload describing the patch to apply to a `summary_slots` row.
///
/// 与 Node `dbOrTx.update(summarySlots).set({status: 'failed', failureReason, updatedAt})` 1:1。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FinalizationPatch {
    pub status: SummarySlotStatus,
    pub failure_reason: String,
    pub updated_at: DateTime<Utc>,
}

/// Finalization errors — mirrors Node's silent-skip semantics for non-terminal issues.
#[derive(Debug, Error)]
pub enum FinalizationError {
    #[error("invalid issue status: {0}")]
    InvalidStatus(String),
    #[error("repo error: {0}")]
    Repo(String),
}

pub type FinalizationResult<T> = std::result::Result<T, FinalizationError>;

// ============================================================================
// Pure helpers
// ============================================================================

/// 解析 issue status string 为 `done` / `cancelled` 终态。
///
/// 与 Node `TERMINAL_ISSUE_STATUSES.has(issue.status)` 检查 1:1。
pub fn is_terminal_issue_status(status: &str) -> bool {
    TERMINAL_ISSUE_STATUSES.contains(&status)
}

/// Construct the user-facing failure reason for a terminal generation issue.
///
/// 与 Node `failureReasonForIssue(issue)` 1:1：
/// - `cancelled` → "Summary generation task {label} was cancelled before writing a summary."
/// - `done` / 其它终态 → "Summary generation task {label} finished without writing a summary."
///
/// `label` = `identifier: title` 若有 identifier，否则 `title`。
pub fn failure_reason_for_terminal_issue(issue: &TerminalGenerationIssue) -> String {
    let label = match &issue.identifier {
        Some(identifier) => format!("{}: {}", identifier, issue.title),
        None => issue.title.clone(),
    };
    match issue.status.as_str() {
        "cancelled" => format!(
            "Summary generation task {} was cancelled before writing a summary.",
            label
        ),
        _ => format!(
            "Summary generation task {} finished without writing a summary.",
            label
        ),
    }
}

/// Build the patch that should be applied to each `summary_slots` row tied to a
/// terminal generation issue.
///
/// 与 Node `dbOrTx.update(...).set({...})` payload 1:1：
/// - `status = 'failed'`
/// - `failure_reason = failureReasonForIssue(issue)`
/// - `updated_at = now`
pub fn build_finalization_patch(
    issue: &TerminalGenerationIssue,
    now: DateTime<Utc>,
) -> FinalizationResult<FinalizationPatch> {
    if !is_terminal_issue_status(&issue.status) {
        return Err(FinalizationError::InvalidStatus(issue.status.clone()));
    }
    Ok(FinalizationPatch {
        status: SummarySlotStatus::Failed,
        failure_reason: failure_reason_for_terminal_issue(issue),
        updated_at: now,
    })
}

/// Description of the WHERE clause used by the SQL update.
///
/// 与 Node `dbOrTx.update(summarySlots).where(...)` 1:1：
/// - `company_id = issue.company_id`
/// - `generating_issue_id = issue.id`
/// - `status = 'generating'`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FinalizationScope {
    pub company_id: Uuid,
    pub generating_issue_id: Uuid,
    pub status: SummarySlotStatus,
}

pub fn finalization_scope(issue: &TerminalGenerationIssue) -> FinalizationScope {
    FinalizationScope {
        company_id: issue.company_id,
        generating_issue_id: issue.id,
        status: SummarySlotStatus::Generating,
    }
}

// ============================================================================
// Orchestration entry point
// ============================================================================

/// Finalize all summary slots linked to a just-terminal generation issue.
///
/// 与 Node `finalizeSummarySlotsForTerminalIssue(dbOrTx, issue)` 1:1：
/// - 非终态 issue → 返回空列表（no-op）
/// - 终态 → 构造 patch + scope，把"要更新什么"和"WHERE 条件"返回给上层，
///   上层（issue 终结副作用流水）通过 `pc_repos::summary::SummaryRepo::mark_failed`
///   完成 SQL update 并把 `RETURNING id` 列表传回。
///
/// 设计理由：本 crate 不持 `&Db`，避免重复注入 db handle；最终 SQL 入口集中在
/// `pc_repos::summary::SummaryRepo`，与本模块的"纯业务逻辑"职责分离。
pub fn finalize_summary_slots_for_terminal_issue(
    issue: &TerminalGenerationIssue,
    now: DateTime<Utc>,
) -> FinalizationResult<Option<FinalizationPlan>> {
    if !is_terminal_issue_status(&issue.status) {
        return Ok(None);
    }
    let patch = build_finalization_patch(issue, now)?;
    let scope = finalization_scope(issue);
    Ok(Some(FinalizationPlan { scope, patch }))
}

/// Plan returned to the caller — describes the update to execute.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FinalizationPlan {
    pub scope: FinalizationScope,
    pub patch: FinalizationPatch,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_issue(status: &str) -> TerminalGenerationIssue {
        TerminalGenerationIssue {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            identifier: Some("ACME-7".to_string()),
            title: "Generate header summary".to_string(),
            status: status.to_string(),
        }
    }

    #[test]
    fn is_terminal_for_done_and_cancelled() {
        assert!(is_terminal_issue_status("done"));
        assert!(is_terminal_issue_status("cancelled"));
    }

    #[test]
    fn is_terminal_rejects_other_statuses() {
        assert!(!is_terminal_issue_status("in_progress"));
        assert!(!is_terminal_issue_status("todo"));
        assert!(!is_terminal_issue_status("blocked"));
        assert!(!is_terminal_issue_status(""));
    }

    #[test]
    fn failure_reason_cancelled_text() {
        let issue = sample_issue("cancelled");
        let msg = failure_reason_for_terminal_issue(&issue);
        assert!(msg.contains("ACME-7: Generate header summary"));
        assert!(msg.contains("cancelled before writing a summary"));
    }

    #[test]
    fn failure_reason_done_text() {
        let issue = sample_issue("done");
        let msg = failure_reason_for_terminal_issue(&issue);
        assert!(msg.contains("ACME-7: Generate header summary"));
        assert!(msg.contains("finished without writing a summary"));
    }

    #[test]
    fn failure_reason_without_identifier_uses_title_only() {
        let mut issue = sample_issue("cancelled");
        issue.identifier = None;
        let msg = failure_reason_for_terminal_issue(&issue);
        assert!(msg.contains("Generate header summary"));
        assert!(!msg.starts_with("Summary generation task :"));
    }

    #[test]
    fn build_finalization_patch_status_and_reason() {
        let issue = sample_issue("done");
        let now = Utc::now();
        let patch = build_finalization_patch(&issue, now).expect("ok");
        assert_eq!(patch.status, SummarySlotStatus::Failed);
        assert!(patch.failure_reason.contains("finished without writing a summary"));
        assert_eq!(patch.updated_at, now);
    }

    #[test]
    fn build_finalization_patch_rejects_non_terminal() {
        let issue = sample_issue("in_progress");
        let result = build_finalization_patch(&issue, Utc::now());
        assert!(matches!(result, Err(FinalizationError::InvalidStatus(_))));
    }

    #[test]
    fn finalization_scope_matches_node_filters() {
        let issue = sample_issue("done");
        let scope = finalization_scope(&issue);
        assert_eq!(scope.company_id, issue.company_id);
        assert_eq!(scope.generating_issue_id, issue.id);
        assert_eq!(scope.status, SummarySlotStatus::Generating);
    }

    #[test]
    fn finalize_returns_none_for_non_terminal_issue() {
        let issue = sample_issue("in_progress");
        let plan = finalize_summary_slots_for_terminal_issue(&issue, Utc::now()).expect("ok");
        assert!(plan.is_none());
    }

    #[test]
    fn finalize_returns_plan_for_done() {
        let issue = sample_issue("done");
        let now = Utc::now();
        let plan =
            finalize_summary_slots_for_terminal_issue(&issue, now).expect("ok").expect("some");
        assert_eq!(plan.scope.company_id, issue.company_id);
        assert_eq!(plan.scope.generating_issue_id, issue.id);
        assert_eq!(plan.patch.status, SummarySlotStatus::Failed);
        assert_eq!(plan.patch.updated_at, now);
    }

    #[test]
    fn finalize_returns_plan_for_cancelled() {
        let issue = sample_issue("cancelled");
        let plan = finalize_summary_slots_for_terminal_issue(&issue, Utc::now())
            .expect("ok")
            .expect("some");
        assert!(plan.patch.failure_reason.contains("cancelled before writing"));
    }
}