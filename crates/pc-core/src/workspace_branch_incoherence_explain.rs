//! `workspace_branch_incoherence_explain` 域（Round 278）。
//!
//! 与原 `paperclip/server/src/services/execution-workspaces.ts` 中两个 pure helper
//! 1:1 对齐：
//! - `explainGitWorktreeBranchReconcileInspection` — 生成 plainLanguage 原因文本
//! - `formatBranchReconcileAuditComment` — 生成 reconcile audit comment markdown body
//!
//! 设计目标：高内聚低耦合。
//! - 高内聚：本模块只关心"branch incoherence 报告文本"。
//! - 低耦合：仅依赖 `workspace_runtime_strings::{format_branch_for_message, format_short_sha}`；
//!   类型字段用 typed struct，不依赖 DB。

use serde::{Deserialize, Serialize};

use crate::workspace_branch_incoherence::{
    fingerprint_workspace_branch_incoherence, BranchIncoherenceInput,
};

/// 与 Node `ExecutionWorkspaceBranchRefResolution` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionWorkspaceBranchRefResolution {
    Resolved,
    Missing,
    Error,
}

impl ExecutionWorkspaceBranchRefResolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Missing => "missing",
            Self::Error => "error",
        }
    }
}

/// Rescue ref 信息（Node `ExecutionWorkspaceBranchReconcileResult['rescueRef']`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RescueRef {
    pub branch_name: String,
    pub commit_sha: String,
    pub file_count: u32,
    pub source_audit_comment_id: Option<String>,
    pub claimant_audit_comment_id: Option<String>,
}

// ============================================================================
// ancestry verdict 字符串
// ============================================================================

/// 与 Node `GitWorktreeBranchAncestryVerdict` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AncestryVerdict {
    Ancestor,
    Diverged,
    Unknown,
}

impl Default for AncestryVerdict {
    fn default() -> Self {
        Self::Unknown
    }
}

impl AncestryVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            AncestryVerdict::Ancestor => "ancestor",
            AncestryVerdict::Diverged => "diverged",
            AncestryVerdict::Unknown => "unknown",
        }
    }
}

/// Branch reconcile mode：与 Node `ExecutionWorkspaceBranchReconcileMode` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileMode {
    Forward,
    Override,
    QuarantineRestore,
}

impl ReconcileMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ReconcileMode::Forward => "forward",
            ReconcileMode::Override => "override",
            ReconcileMode::QuarantineRestore => "quarantine_restore",
        }
    }
}

// ============================================================================
// explainGitWorktreeBranchReconcileInspection（Round 278）
// ============================================================================

#[derive(Debug, Clone)]
pub struct ExplainInspectionInput {
    pub from_branch: String,
    pub to_branch: String,
    pub from_sha: Option<String>,
    pub to_sha: Option<String>,
    pub ancestry_verdict: AncestryVerdict,
}

/// `explainGitWorktreeBranchReconcileInspection` 1:1 对位 Node：
/// - 都缺失 → `Paperclip could not determine branch ancestry because ...`
/// - SHA 相同 → `The recorded branch ... and checked-out branch ... resolve to the same commit.`
/// - ancestor → `The recorded branch ... is an ancestor of ...`
/// - diverged → `The recorded branch ... is not an ancestor of ...`
/// - 其他/unknown → `Paperclip could not determine whether ... is forward of ...`
pub fn explain_git_worktree_branch_reconcile_inspection(input: &ExplainInspectionInput) -> String {
    if input.from_sha.is_none() || input.to_sha.is_none() {
        return format!(
            "Paperclip could not determine branch ancestry because \"{}\" or \"{}\" is missing a resolvable HEAD commit.",
            input.from_branch, input.to_branch
        );
    }
    let from_sha = input.from_sha.as_deref().unwrap_or("");
    let to_sha = input.to_sha.as_deref().unwrap_or("");
    if from_sha == to_sha {
        return format!(
            "The recorded branch \"{}\" and checked-out branch \"{}\" resolve to the same commit.",
            input.from_branch, input.to_branch
        );
    }
    match input.ancestry_verdict {
        AncestryVerdict::Ancestor => format!(
            "The recorded branch \"{}\" is an ancestor of the checked-out branch \"{}\".",
            input.from_branch, input.to_branch
        ),
        AncestryVerdict::Diverged => format!(
            "The recorded branch \"{}\" is not an ancestor of the checked-out branch \"{}\".",
            input.from_branch, input.to_branch
        ),
        AncestryVerdict::Unknown => format!(
            "Paperclip could not determine whether \"{}\" is forward of \"{}\".",
            input.to_branch, input.from_branch
        ),
    }
}

// ============================================================================
// formatBranchReconcileAuditComment（Round 278）
// ============================================================================

#[derive(Debug, Clone)]
pub struct FormatAuditCommentInput {
    pub mode: ReconcileMode,
    pub reason: Option<String>,
    pub workspace_id: String,
    pub inspection: InspectionLite,
    pub recovery_action_id: Option<String>,
    pub rescue_ref: Option<RescueRef>,
}

#[derive(Debug, Clone)]
pub struct InspectionLite {
    pub from_branch: String,
    pub to_branch: String,
    pub from_sha: Option<String>,
    pub to_sha: Option<String>,
    pub ancestry_verdict: AncestryVerdict,
    pub fingerprint: String,
}

/// `formatBranchReconcileAuditComment(input)` 1:1 对位 Node。
/// 输出 markdown body，按固定顺序拼接。
pub fn format_branch_reconcile_audit_comment(input: &FormatAuditCommentInput) -> String {
    let mut lines: Vec<String> = vec![
        "Execution workspace branch reconciled.".to_string(),
        String::new(),
        format!("- Workspace: `{}`", input.workspace_id),
        format!("- Mode: `{}`", input.mode.as_str()),
        format!(
            "- From branch: `{}`",
            crate::workspace_runtime_strings::format_branch_for_message(Some(
                &input.inspection.from_branch
            ))
            .unwrap_or_else(|| "<detached>".to_string())
        ),
        format!(
            "- To branch: `{}`",
            crate::workspace_runtime_strings::format_branch_for_message(Some(
                &input.inspection.to_branch
            ))
            .unwrap_or_else(|| "<detached>".to_string())
        ),
        format!(
            "- From SHA: `{}`",
            input
                .inspection
                .from_sha
                .clone()
                .unwrap_or_else(|| "unknown".to_string())
        ),
        format!(
            "- To SHA: `{}`",
            input
                .inspection
                .to_sha
                .clone()
                .unwrap_or_else(|| "unknown".to_string())
        ),
        format!(
            "- Verdict: `{}`",
            input.inspection.ancestry_verdict.as_str()
        ),
        format!("- Fingerprint: `{}`", input.inspection.fingerprint),
        format!(
            "- Recovery action: {}",
            match &input.recovery_action_id {
                Some(id) => format!("`{id}`"),
                None => "none matched".to_string(),
            }
        ),
    ];
    if let Some(ref r) = input.rescue_ref {
        lines.push(format!("- Rescue ref: `{}`", r.branch_name));
        lines.push(format!("- Rescue commit: `{}`", r.commit_sha));
        lines.push(format!("- Rescued file count: `{}`", r.file_count));
    }
    if let Some(ref reason) = input.reason {
        lines.push(format!("- Operator reason: {reason}"));
    }
    lines.join("\n")
}

/// 便利：通过 `BranchIncoherenceInput` 直接构建 `InspectionLite`，并调用 fingerprint。
pub fn build_inspection_lite(
    input: &BranchIncoherenceInput,
    to_branch: String,
    to_sha: Option<String>,
    from_branch_ref_status: ExecutionWorkspaceBranchRefResolution,
    to_branch_ref_status: ExecutionWorkspaceBranchRefResolution,
    ancestry_verdict: AncestryVerdict,
) -> InspectionLite {
    let fingerprint = fingerprint_workspace_branch_incoherence(input);
    let _ = (from_branch_ref_status, to_branch_ref_status); // 引用保留以维持 API 表面对位
    InspectionLite {
        from_branch: input.expected_branch.clone(),
        to_branch,
        from_sha: input.expected_head_sha.clone(),
        to_sha,
        ancestry_verdict,
        fingerprint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestry_verdict_strings_match_node() {
        assert_eq!(AncestryVerdict::Ancestor.as_str(), "ancestor");
        assert_eq!(AncestryVerdict::Diverged.as_str(), "diverged");
        assert_eq!(AncestryVerdict::Unknown.as_str(), "unknown");
    }

    #[test]
    fn reconcile_mode_strings_match_node() {
        assert_eq!(ReconcileMode::Forward.as_str(), "forward");
        assert_eq!(ReconcileMode::Override.as_str(), "override");
        assert_eq!(
            ReconcileMode::QuarantineRestore.as_str(),
            "quarantine_restore"
        );
    }

    #[test]
    fn explain_missing_sha_returns_missing_message() {
        let out = explain_git_worktree_branch_reconcile_inspection(&ExplainInspectionInput {
            from_branch: "feature/x".into(),
            to_branch: "main".into(),
            from_sha: None,
            to_sha: Some("abc".into()),
            ancestry_verdict: AncestryVerdict::Unknown,
        });
        assert!(out.contains("could not determine branch ancestry"));
        assert!(out.contains("feature/x"));
        assert!(out.contains("main"));
    }

    #[test]
    fn explain_same_sha_returns_same_commit_message() {
        let out = explain_git_worktree_branch_reconcile_inspection(&ExplainInspectionInput {
            from_branch: "main".into(),
            to_branch: "main".into(),
            from_sha: Some("abc123".into()),
            to_sha: Some("abc123".into()),
            ancestry_verdict: AncestryVerdict::Unknown, // 不影响
        });
        assert!(out.contains("resolve to the same commit"));
    }

    #[test]
    fn explain_ancestor_returns_ancestor_message() {
        let out = explain_git_worktree_branch_reconcile_inspection(&ExplainInspectionInput {
            from_branch: "main".into(),
            to_branch: "feature".into(),
            from_sha: Some("aaa".into()),
            to_sha: Some("bbb".into()),
            ancestry_verdict: AncestryVerdict::Ancestor,
        });
        assert!(out.contains("is an ancestor of"));
    }

    #[test]
    fn explain_diverged_returns_not_ancestor_message() {
        let out = explain_git_worktree_branch_reconcile_inspection(&ExplainInspectionInput {
            from_branch: "main".into(),
            to_branch: "feature".into(),
            from_sha: Some("aaa".into()),
            to_sha: Some("bbb".into()),
            ancestry_verdict: AncestryVerdict::Diverged,
        });
        assert!(out.contains("not an ancestor of"));
    }

    #[test]
    fn explain_unknown_returns_unable_message() {
        let out = explain_git_worktree_branch_reconcile_inspection(&ExplainInspectionInput {
            from_branch: "main".into(),
            to_branch: "feature".into(),
            from_sha: Some("aaa".into()),
            to_sha: Some("bbb".into()),
            ancestry_verdict: AncestryVerdict::Unknown,
        });
        assert!(out.contains("Paperclip could not determine whether"));
    }

    #[test]
    fn audit_comment_basic_layout() {
        let input = FormatAuditCommentInput {
            mode: ReconcileMode::Forward,
            reason: None,
            workspace_id: "ws-1".into(),
            inspection: InspectionLite {
                from_branch: "main".into(),
                to_branch: "feature/x".into(),
                from_sha: Some("aaaa".into()),
                to_sha: Some("bbbb".into()),
                ancestry_verdict: AncestryVerdict::Ancestor,
                fingerprint: "fp:abc".into(),
            },
            recovery_action_id: None,
            rescue_ref: None,
        };
        let out = format_branch_reconcile_audit_comment(&input);
        assert!(out.contains("Execution workspace branch reconciled."));
        assert!(out.contains("- Workspace: `ws-1`"));
        assert!(out.contains("- Mode: `forward`"));
        assert!(out.contains("- From branch: `main`"));
        assert!(out.contains("- To branch: `feature/x`"));
        assert!(out.contains("- From SHA: `aaaa`"));
        assert!(out.contains("- To SHA: `bbbb`"));
        assert!(out.contains("- Verdict: `ancestor`"));
        assert!(out.contains("- Fingerprint: `fp:abc`"));
        assert!(out.contains("- Recovery action: none matched"));
        assert!(!out.contains("Rescue ref"));
        assert!(!out.contains("Operator reason"));
    }

    #[test]
    fn audit_comment_with_rescue_ref_and_reason() {
        let input = FormatAuditCommentInput {
            mode: ReconcileMode::QuarantineRestore,
            reason: Some("manual cleanup".into()),
            workspace_id: "ws-2".into(),
            inspection: InspectionLite {
                from_branch: "main".into(),
                to_branch: "main".into(),
                from_sha: Some("aaa".into()),
                to_sha: Some("aaa".into()),
                ancestry_verdict: AncestryVerdict::Unknown,
                fingerprint: "fp:def".into(),
            },
            recovery_action_id: Some("recover-1".into()),
            rescue_ref: Some(RescueRef {
                branch_name: "paperclip/rescue/PAPER-42/20260806T123456Z".into(),
                commit_sha: "deadbeef".into(),
                file_count: 5,
                source_audit_comment_id: None,
                claimant_audit_comment_id: None,
            }),
        };
        let out = format_branch_reconcile_audit_comment(&input);
        assert!(out.contains("- Mode: `quarantine_restore`"));
        assert!(out.contains("- Recovery action: `recover-1`"));
        assert!(out.contains("- Rescue ref: `paperclip/rescue/PAPER-42/20260806T123456Z`"));
        assert!(out.contains("- Rescue commit: `deadbeef`"));
        assert!(out.contains("- Rescued file count: `5`"));
        assert!(out.contains("- Operator reason: manual cleanup"));
    }

    #[test]
    fn audit_comment_unknown_sha_is_unknown_string() {
        let input = FormatAuditCommentInput {
            mode: ReconcileMode::Override,
            reason: None,
            workspace_id: "ws-3".into(),
            inspection: InspectionLite {
                from_branch: "main".into(),
                to_branch: "main".into(),
                from_sha: None,
                to_sha: None,
                ancestry_verdict: AncestryVerdict::Unknown,
                fingerprint: "fp:x".into(),
            },
            recovery_action_id: None,
            rescue_ref: None,
        };
        let out = format_branch_reconcile_audit_comment(&input);
        assert!(out.contains("- From SHA: `unknown`"));
        assert!(out.contains("- To SHA: `unknown`"));
    }
}
