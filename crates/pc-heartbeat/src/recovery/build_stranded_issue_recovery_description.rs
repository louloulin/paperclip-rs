//! `buildStrandedIssueRecoveryDescription` —— Node `services/recovery/service.ts:2625`。
//!
//! 业务语义：
//! - 输入：source issue + latest run + previous status + prefix + cause + evidence
//! - 输出：description 文本（Markdown），与 Node 内容结构一致
//!
//! 两种主分支：
//! 1. **SuccessfulRunMissingState** —— "Safe Evidence" + "Required Action" 段
//! 2. **Default (stranded_assigned_issue / execution_review_participant_recovery)** ——
//!    "Source" + "Ownership" + "Required Action" 段
//!
//! 设计原则：
//! - 全部是 pure 函数，无副作用
//! - 输入用独立的 view struct (`LatestRunView` / `AgentShortView`) 而非强依赖完整 row
//! - 与 Node 业务文本 1:1 对齐（Markdown 格式相同；URL 链接简化为文本引用）
//! - 复用 `summarize_run_failure_for_issue_comment`（已存在）
//! - 复用 `read_retry_reason_from_context` 内部 helper 处理空值兜底

use crate::recovery::source_scoped_recovery_action::StrandedRecoveryCause;
use crate::recovery::summarize_run_failure::{
    summarize_run_failure_for_issue_comment, RunFailureView,
};
use pc_repos::issue::IssueRow;
use serde_json::Value;
use uuid::Uuid;

/// Latest run 的最少化 view（用于避免强依赖完整 `HeartbeatRunRow`）。
///
/// `context_snapshot` 持有 owned `Value` 以简化调用方 ergonomics。
#[derive(Debug, Clone)]
pub struct LatestRunView {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub status: Option<String>,
    pub context_snapshot: Option<Value>,
    /// latest run 的 result_json；用于从 `result_json.workspaceValidation.fingerprint`
    /// 推导 workspace_validation cause 下的 fingerprint。
    /// owned 以避免调用方需要长期借用。
    pub result_json: Option<Value>,
}

/// Source assignee 的最少化 view（用于失败摘要 / agent 链接引用）。
#[derive(Debug, Clone)]
pub struct AgentShortView {
    pub id: Uuid,
    pub name: String,
}

/// `buildStrandedIssueRecoveryDescription` 的输入。
#[derive(Debug, Clone)]
pub struct BuildStrandedIssueRecoveryDescriptionInput<'a> {
    pub issue: &'a IssueRow,
    pub latest_run: Option<&'a LatestRunView>,
    pub previous_status: &'a str,
    pub prefix: &'a str,
    pub recovery_cause: Option<StrandedRecoveryCause>,
    pub successful_run_handoff_evidence: Option<&'a Value>,
    pub source_assignee: Option<&'a AgentShortView>,
    /// `WorkspaceValidationFailed` cause 下用于 description 注入的 fingerprint override。
    /// 优先级高于从 `latest_run.result_json` 自动推导。
    /// 非 `WorkspaceValidationFailed` cause 时该字段被忽略。
    pub workspace_validation_fingerprint: Option<&'a str>,
}

/// 从 context_snapshot.retryReason 读取字符串（trim 后非空）。
///
/// 与 Node `readNonEmptyString(parseObject(...).retryReason) ?? "unknown"` 对齐。

/// 从 `latest_run.result_json.workspaceValidation.fingerprint` 读取 fingerprint。
///
/// 与 `scheduler::read_workspace_validation_fingerprint` 对齐；返回 `None` 表示：
/// - `result_json` 缺失
/// - `workspaceValidation` 段缺失或为空对象
/// - `workspaceValidation.fingerprint` 缺失、空字符串或非 string
fn read_workspace_validation_fingerprint_from_view(
    latest_run: Option<&LatestRunView>,
) -> Option<String> {
    let json = latest_run.and_then(|r| r.result_json.as_ref())?;
    let payload = json.get("workspaceValidation")?;
    if let serde_json::Value::Object(map) = payload {
        if map.is_empty() {
            return None;
        }
    }
    let raw = payload.get("fingerprint")?.as_str()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn read_retry_reason_from_context(context_snapshot: Option<&Value>) -> &str {
    let Some(value) = context_snapshot else {
        return "unknown";
    };
    let Some(obj) = value.as_object() else {
        return "unknown";
    };
    let Some(reason) = obj.get("retryReason") else {
        return "unknown";
    };
    let Some(s) = reason.as_str() else {
        return "unknown";
    };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        "unknown"
    } else {
        // 返回原始 s 的 borrow 字符串，避免重建
        s
    }
}

/// Issue UI link 的简化版：Markdown 文本（不生成 URL）。
///
/// 与 Node `issueUiLink(...)` 对齐；Rust 端简化为 `PAP-123 (id-uuid)` 形式。
fn format_issue_link(issue: &IssueRow) -> String {
    let ident = issue
        .identifier
        .clone()
        .unwrap_or_else(|| issue.title.clone());
    format!("{ident} (`{}`)", issue.id)
}

/// Agent UI link 的简化版：Markdown 文本（不生成 URL）。
///
/// 与 Node `agentUiLink(...)` 对齐；Rust 端简化为 `name (id-uuid)` 形式。
fn format_agent_link(assignee: Option<&AgentShortView>) -> String {
    match assignee {
        Some(a) => format!("{} (`{}`)", a.name, a.id),
        None => "unknown".to_string(),
    }
}

/// Run link 的简化版：Markdown 文本（不生成 URL）。
///
/// 与 Node `runUiLink(...)` 对齐；Rust 端简化为 `\`id\` (agent-id)` 形式。
fn format_run_link(run: Option<&LatestRunView>) -> String {
    match run {
        Some(r) => format!("`{}` (agent `{}`)", r.id, r.agent_id),
        None => "none".to_string(),
    }
}

/// 主入口：构造 stranded issue recovery issue 的 description。
///
/// 与 Node `buildStrandedIssueRecoveryDescription` 对齐：
/// - SuccessfulRunMissingState cause → "Safe Evidence" + "Required Action" 段
/// - 其他 cause → "Source" + "Ownership" + "Required Action" 段
/// - execution_review_participant_recovery cause → review-specific 文本
pub fn build_stranded_issue_recovery_description(
    input: &BuildStrandedIssueRecoveryDescriptionInput<'_>,
) -> String {
    let issue_link = format_issue_link(input.issue);
    let source_assignee_link = format_agent_link(input.source_assignee);
    let run_link = format_run_link(input.latest_run);

    // SuccessfulRunMissingState 路径
    if matches!(
        input.recovery_cause,
        Some(StrandedRecoveryCause::SuccessfulRunMissingState)
    ) {
        return build_successful_run_missing_state_description(
            input,
            &issue_link,
            &source_assignee_link,
            &run_link,
        );
    }

    // Default 路径（stranded_assigned_issue / execution_review_participant_recovery / 其他）
    let is_review_participant = matches!(
        input.recovery_cause,
        Some(StrandedRecoveryCause::ExecutionReviewParticipantRecovery)
    );
    let detected_invariant = if is_review_participant {
        "execution_review_participant_recovery"
    } else {
        "stranded_assigned_issue"
    };
    let intro = if is_review_participant {
        "Paperclip exhausted automatic recovery for a pending execution-review participant and created this explicit recovery task."
    } else {
        "Paperclip exhausted automatic recovery for an assigned issue and created this explicit recovery task."
    };
    let retry_reason =
        read_retry_reason_from_context(input.latest_run.and_then(|r| r.context_snapshot.as_ref()));
    let workspace_validation_fingerprint: Option<String> =
        if matches!(input.recovery_cause, Some(StrandedRecoveryCause::WorkspaceValidationFailed)) {
            input
                .workspace_validation_fingerprint
                .map(str::to_owned)
                .filter(|s| !s.trim().is_empty())
                .or_else(|| read_workspace_validation_fingerprint_from_view(input.latest_run))
        } else {
            None
        };
    // 当 cause == WorkspaceValidationFailed 时始终输出 fingerprint 行；
    // 缺失时 fallback 到 `none reported`，让阅读者明确知道 fingerprint 不可用。
    // 其他 cause 不展示该行（避免噪音）。
    let workspace_validation_fingerprint_line: Option<String> =
        if matches!(input.recovery_cause, Some(StrandedRecoveryCause::WorkspaceValidationFailed)) {
            Some(format!(
                "- Workspace validation fingerprint: `{}`",
                workspace_validation_fingerprint.as_deref().unwrap_or("none reported")
            ))
        } else {
            None
        };
    let run_failure_view = input.latest_run.map(|r| RunFailureView {
        error: None,
        error_code: None,
    });
    let failure_summary = summarize_run_failure_for_issue_comment(run_failure_view.as_ref());

    let required_action: Vec<&str> = if is_review_participant {
        vec![
            "- Inspect the latest reviewer run and the pending execution-review stage.",
            "- Fix the reviewer runtime, restore the source issue to `in_review` with a live participant, or record an intentional manual resolution.",
            "- When the source issue has a live review path or has been intentionally resolved, mark this recovery issue done.",
        ]
    } else {
        vec![
            "- Inspect the latest run and source issue state.",
            "- Fix the runtime/adapter problem, reassign the source issue, or convert the source issue into a clear manual-review state.",
            "- When the source issue has a live execution path or has been intentionally resolved, mark this recovery issue done.",
        ]
    };

    [
        intro,
        "",
        "## Source",
        "",
        &format!("- Source issue: {issue_link}"),
        &format!("- Previous source status: `{}`", input.previous_status),
        &format!("- Latest retry run: {run_link}"),
        &format!(
            "- Latest retry status: `{}`",
            input
                .latest_run
                .and_then(|r| r.status.as_deref())
                .unwrap_or("unknown")
        ),
        &format!("- Detected invariant: `{detected_invariant}`"),
        &format!("- Retry reason: `{retry_reason}`"),
        workspace_validation_fingerprint_line.as_deref().unwrap_or(""),
        failure_summary
            .map(|s| format!("- Failure: {}", s.trim()))
            .unwrap_or_else(|| "- Failure: none recorded".to_string())
            .as_str(),
        "",
        "## Ownership",
        "",
        "- Selected owner: the first invokable manager/creator/executive candidate with budget available.",
        "",
        "## Required Action",
        "",
    ]
    .into_iter()
    .chain(required_action.into_iter())
    .collect::<Vec<_>>()
    .join("\n")
}

/// SuccessfulRunMissingState 分支专用 builder。
fn build_successful_run_missing_state_description(
    input: &BuildStrandedIssueRecoveryDescriptionInput<'_>,
    issue_link: &str,
    source_assignee_link: &str,
    run_link: &str,
) -> String {
    let evidence = input.successful_run_handoff_evidence;
    let source_run_id = evidence
        .and_then(|v| v.get("sourceRunId"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let source_run_link = if !source_run_id.is_empty() {
        if let Some(latest) = input.latest_run {
            format!("`{source_run_id}` (agent `{}`)", latest.agent_id)
        } else {
            "unknown".to_string()
        }
    } else {
        "unknown".to_string()
    };
    let missing_disposition = evidence
        .and_then(|v| v.get("missingDisposition"))
        .and_then(|v| v.as_str())
        .unwrap_or("clear_next_step");

    [
        "Paperclip exhausted the bounded corrective handoff for a successful run that still has no valid issue disposition.",
        "",
        "This is not a runtime/adapter crash report. The source run succeeded; the remaining problem is the missing `done`, `in_review`, `blocked`, delegated follow-up, or explicit continuation path.",
        "",
        "## Safe Evidence",
        "",
        &format!("- Source issue: {issue_link}"),
        &format!("- Source run: {source_run_link}"),
        &format!("- Corrective handoff run: {run_link}"),
        &format!("- Source assignee: {source_assignee_link}"),
        &format!("- Latest issue status: `{}`", input.issue.status),
        &format!(
            "- Latest handoff run status: `{}`",
            input
                .latest_run
                .and_then(|r| r.status.as_deref())
                .unwrap_or("unknown")
        ),
        "- Normalized cause: `successful_run_missing_state`",
        &format!("- Missing disposition: `{missing_disposition}`"),
        "- Suggested manager action: choose and record a valid issue disposition without copying transcript content.",
        "",
        "## Required Action",
        "",
        "- Inspect the source issue and run metadata, not raw transcript excerpts.",
        "- Choose a valid issue disposition: `done`/`cancelled`, `in_review` with an owner, `blocked` with first-class blockers, delegated follow-up work, or an explicit continuation path.",
        "- When the source issue has a clear owner and disposition, mark this recovery issue done.",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_retry_reason_returns_unknown_for_none() {
        assert_eq!(read_retry_reason_from_context(None), "unknown");
    }

    #[test]
    fn read_retry_reason_returns_unknown_for_non_object() {
        let v = json!("just a string");
        assert_eq!(read_retry_reason_from_context(Some(&v)), "unknown");
    }

    #[test]
    fn read_retry_reason_returns_unknown_for_empty_string() {
        let v = json!({"retryReason": ""});
        assert_eq!(read_retry_reason_from_context(Some(&v)), "unknown");
    }

    #[test]
    fn read_retry_reason_returns_value() {
        let v = json!({"retryReason": "issue_continuation_needed"});
        assert_eq!(
            read_retry_reason_from_context(Some(&v)),
            "issue_continuation_needed"
        );
    }

    #[test]
    fn format_agent_link_uses_name() {
        let a = AgentShortView {
            id: Uuid::from_bytes([2; 16]),
            name: "agent-x".to_string(),
        };
        assert!(format_agent_link(Some(&a)).contains("agent-x"));
    }

    #[test]
    fn format_agent_link_unknown_when_none() {
        assert_eq!(format_agent_link(None), "unknown");
    }

    #[test]
    fn format_run_link_none_when_missing() {
        assert_eq!(format_run_link(None), "none");
    }
}
