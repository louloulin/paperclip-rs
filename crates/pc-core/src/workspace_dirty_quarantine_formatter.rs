//! `workspace_dirty_quarantine_formatter` — dirty worktree 隔离的纯文本格式化。
//!
//! 与 Node `formatDirtyQuarantineContentionRefusal` / `formatDirtyQuarantineFailure` /
//! `formatDirtyQuarantineAuditComment` 1:1 对齐。
//!
//! 设计目标：纯函数模块，不依赖 DB/IO。
use serde::{Deserialize, Serialize};

use crate::workspace_runtime_string_utils::{
    format_branch_for_message, format_issue_reference, git_error_includes,
};

// ============================================================================
// Enums & types
// ============================================================================

/// `GitWorktreeInProgressOperation`：与 Node union 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeInProgressOperation {
    Rebase,
    Merge,
    CherryPick,
    Revert,
    Bisect,
}

impl GitWorktreeInProgressOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rebase => "rebase",
            Self::Merge => "merge",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
            Self::Bisect => "bisect",
        }
    }
}

/// `ContentionActiveRun`：`activeRun` 子结构。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentionActiveRun {
    pub id: String,
    pub status: String,
}

/// `GitWorktreeBranchContention`：minimal subset for formatter。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitWorktreeBranchContention {
    pub claimed_by_workspace_id: String,
    pub claimed_by_issue_id: Option<String>,
    pub claimed_by_issue_identifier: Option<String>,
    pub active_run: Option<ContentionActiveRun>,
}

/// `GitWorktreeBranchIncoherenceEvidence`：minimal subset for formatter。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitWorktreeBranchIncoherenceEvidence {
    pub fingerprint: String,
    pub source_issue_id: Option<String>,
    pub source_identifier: Option<String>,
    pub execution_workspace_id: Option<String>,
    pub worktree_path: String,
    pub expected_branch: String,
    pub actual_branch: Option<String>,
    pub dirty_path_sample: Vec<String>,
    pub in_progress_operation: Option<GitWorktreeInProgressOperation>,
}

/// `SourceIssue`：minimal IssueRef for formatter。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceIssue {
    pub id: String,
    pub identifier: Option<String>,
}

/// `FormatDirtyQuarantineAuditCommentInput`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FormatDirtyQuarantineAuditCommentInput {
    pub evidence: GitWorktreeBranchIncoherenceEvidence,
    pub rescue_branch: String,
    pub rescue_commit_sha: String,
    pub file_count: i64,
    pub source_issue: Option<SourceIssue>,
    pub claimant: Option<GitWorktreeBranchContention>,
}

// ============================================================================
// formatDirtyQuarantineContentionRefusal
// ============================================================================

/// `formatDirtyQuarantineContentionRefusal(contention)`：
///
/// 与 Node 1:1 对齐：
/// - activeRun → " with active run <id>"
/// - 否则 → " with no active run"
pub fn format_dirty_quarantine_contention_refusal(
    contention: &GitWorktreeBranchContention,
) -> String {
    let active_run_text = match &contention.active_run {
        Some(run) => format!(" with active run {}", run.id),
        None => " with no active run".to_string(),
    };
    format!(
        "dirty quarantine repair refused because workspace {} already claims the live branch{}",
        contention.claimed_by_workspace_id, active_run_text
    )
}

// ============================================================================
// formatDirtyQuarantineFailure
// ============================================================================

/// `formatDirtyQuarantineFailure(errorMessage)`：
///
/// 与 Node 1:1 对齐：检测 git index lock 关键字，含则用专门前缀，否则通用前缀。
pub fn format_dirty_quarantine_failure(error_message: &str) -> String {
    if git_error_includes(error_message, "index.lock")
        || git_error_includes(error_message, "index lock")
        || git_error_includes(error_message, "another git process")
        || git_error_includes(error_message, "Unable to create")
    {
        format!(
            "dirty quarantine repair aborted because git reported index contention: {}",
            error_message
        )
    } else {
        format!("dirty quarantine repair failed: {}", error_message)
    }
}

// ============================================================================
// formatDirtyQuarantineAuditComment
// ============================================================================

/// `formatDirtyQuarantineAuditComment(input)`：构造隔离审计评论 markdown。
///
/// 与 Node 1:1 对齐：
/// - 标题 + 多行 bullet list
/// - Dirty file sample 缺失 → `\`none captured\``
/// - 异常操作 → 可选行
/// - Claimant → 可选行（用 `formatIssueReference`）
pub fn format_dirty_quarantine_audit_comment(
    input: &FormatDirtyQuarantineAuditCommentInput,
) -> String {
    let dirty_sample = if input.evidence.dirty_path_sample.is_empty() {
        "`none captured`".to_string()
    } else {
        input
            .evidence
            .dirty_path_sample
            .iter()
            .map(|entry| format!("`{}`", entry))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let source_ref = format_issue_reference(
        input.evidence.source_issue_id.as_deref(),
        input.evidence.source_identifier.as_deref().or(input
            .source_issue
            .as_ref()
            .and_then(|i| i.identifier.as_deref())),
    );

    let claimant_line = match &input.claimant {
        Some(c) => {
            let run_suffix = match &c.active_run {
                Some(run) => format!(" with active run `{}`", run.id),
                None => " with no active run".to_string(),
            };
            format!(
                "- Claimant: workspace `{}` on issue {}{}",
                c.claimed_by_workspace_id,
                format_issue_reference(
                    c.claimed_by_issue_id.as_deref(),
                    c.claimed_by_issue_identifier.as_deref()
                ),
                run_suffix
            )
        }
        None => "- Claimant: none".to_string(),
    };

    let mut lines: Vec<String> = vec![
        "Execution workspace dirty worktree quarantined before restore.".to_string(),
        String::new(),
        format!("- Source issue: {}", source_ref),
        format!(
            "- Workspace: `{}`",
            input
                .evidence
                .execution_workspace_id
                .clone()
                .unwrap_or_else(|| "unpersisted".to_string())
        ),
        format!("- Worktree: `{}`", input.evidence.worktree_path),
        format!("- Recorded branch: `{}`", input.evidence.expected_branch),
        format!(
            "- Live branch: `{}`",
            format_branch_for_message(input.evidence.actual_branch.as_deref())
        ),
        format!("- Rescue branch: `{}`", input.rescue_branch),
        format!("- Rescue commit: `{}`", input.rescue_commit_sha),
        format!("- Dirty file count: `{}`", input.file_count),
        format!("- Dirty path sample: {}", dirty_sample),
    ];

    if let Some(op) = input.evidence.in_progress_operation {
        lines.push(format!(
            "- Interrupted operation: `git {}` (state cleared after rescue; resolution preserved on the rescue branch)",
            op.as_str()
        ));
    }

    lines.push(format!("- Fingerprint: `{}`", input.evidence.fingerprint));
    lines.push(claimant_line);

    lines.join("\n")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn base_evidence() -> GitWorktreeBranchIncoherenceEvidence {
        GitWorktreeBranchIncoherenceEvidence {
            fingerprint: "fp-1".into(),
            source_issue_id: Some("iss-1".into()),
            source_identifier: Some("PROJ-1".into()),
            execution_workspace_id: Some("ws-1".into()),
            worktree_path: "/wt".into(),
            expected_branch: "feat/x".into(),
            actual_branch: Some("feat/y".into()),
            dirty_path_sample: vec!["a.txt".into(), "b.txt".into()],
            in_progress_operation: None,
        }
    }

    // ----- formatDirtyQuarantineContentionRefusal -----

    #[test]
    fn contention_refusal_no_active_run() {
        let c = GitWorktreeBranchContention {
            claimed_by_workspace_id: "ws-2".into(),
            claimed_by_issue_id: Some("iss-2".into()),
            claimed_by_issue_identifier: Some("PROJ-2".into()),
            active_run: None,
        };
        let out = format_dirty_quarantine_contention_refusal(&c);
        assert_eq!(
            out,
            "dirty quarantine repair refused because workspace ws-2 already claims the live branch with no active run"
        );
    }

    #[test]
    fn contention_refusal_with_active_run() {
        let c = GitWorktreeBranchContention {
            claimed_by_workspace_id: "ws-2".into(),
            claimed_by_issue_id: None,
            claimed_by_issue_identifier: None,
            active_run: Some(ContentionActiveRun {
                id: "run-1".into(),
                status: "running".into(),
            }),
        };
        let out = format_dirty_quarantine_contention_refusal(&c);
        assert!(out.contains("with active run run-1"));
    }

    // ----- formatDirtyQuarantineFailure -----

    #[test]
    fn failure_index_lock() {
        let out = format_dirty_quarantine_failure(
            "fatal: Unable to create '.git/index.lock': File exists",
        );
        assert!(out
            .starts_with("dirty quarantine repair aborted because git reported index contention"));
        assert!(out.contains("Unable to create"));
    }

    #[test]
    fn failure_index_lock_lowercase() {
        let out = format_dirty_quarantine_failure("error: index.lock exists");
        assert!(out.starts_with("dirty quarantine repair aborted"));
    }

    #[test]
    fn failure_generic() {
        let out = format_dirty_quarantine_failure("something else failed");
        assert_eq!(out, "dirty quarantine repair failed: something else failed");
    }

    // ----- formatDirtyQuarantineAuditComment -----

    #[test]
    fn audit_comment_basic() {
        let input = FormatDirtyQuarantineAuditCommentInput {
            evidence: base_evidence(),
            rescue_branch: "paperclip/rescue/iss-1/20250101T000000Z".into(),
            rescue_commit_sha: "abc123".into(),
            file_count: 2,
            source_issue: Some(SourceIssue {
                id: "iss-1".into(),
                identifier: Some("PROJ-1".into()),
            }),
            claimant: None,
        };
        let out = format_dirty_quarantine_audit_comment(&input);
        assert!(out.starts_with("Execution workspace dirty worktree quarantined before restore."));
        assert!(out.contains("- Source issue: [PROJ-1](/PROJ/issues/PROJ-1)"));
        assert!(out.contains("- Workspace: `ws-1`"));
        assert!(out.contains("- Worktree: `/wt`"));
        assert!(out.contains("- Recorded branch: `feat/x`"));
        assert!(out.contains("- Live branch: `feat/y`"));
        assert!(out.contains("- Rescue branch: `paperclip/rescue/iss-1/20250101T000000Z`"));
        assert!(out.contains("- Rescue commit: `abc123`"));
        assert!(out.contains("- Dirty file count: `2`"));
        assert!(out.contains("- Dirty path sample: `a.txt`, `b.txt`"));
        assert!(out.contains("- Fingerprint: `fp-1`"));
        assert!(out.contains("- Claimant: none"));
    }

    #[test]
    fn audit_comment_detached_branch() {
        let mut ev = base_evidence();
        ev.actual_branch = None;
        let input = FormatDirtyQuarantineAuditCommentInput {
            evidence: ev,
            rescue_branch: "rb".into(),
            rescue_commit_sha: "sha".into(),
            file_count: 0,
            source_issue: None,
            claimant: None,
        };
        let out = format_dirty_quarantine_audit_comment(&input);
        assert!(out.contains("- Live branch: `<detached>`"));
    }

    #[test]
    fn audit_comment_empty_dirty_sample() {
        let mut ev = base_evidence();
        ev.dirty_path_sample = vec![];
        let input = FormatDirtyQuarantineAuditCommentInput {
            evidence: ev,
            rescue_branch: "rb".into(),
            rescue_commit_sha: "sha".into(),
            file_count: 0,
            source_issue: None,
            claimant: None,
        };
        let out = format_dirty_quarantine_audit_comment(&input);
        assert!(out.contains("- Dirty path sample: `none captured`"));
    }

    #[test]
    fn audit_comment_unpersisted_workspace() {
        let mut ev = base_evidence();
        ev.execution_workspace_id = None;
        let input = FormatDirtyQuarantineAuditCommentInput {
            evidence: ev,
            rescue_branch: "rb".into(),
            rescue_commit_sha: "sha".into(),
            file_count: 0,
            source_issue: None,
            claimant: None,
        };
        let out = format_dirty_quarantine_audit_comment(&input);
        assert!(out.contains("- Workspace: `unpersisted`"));
    }

    #[test]
    fn audit_comment_with_in_progress_operation() {
        let mut ev = base_evidence();
        ev.in_progress_operation = Some(GitWorktreeInProgressOperation::Rebase);
        let input = FormatDirtyQuarantineAuditCommentInput {
            evidence: ev,
            rescue_branch: "rb".into(),
            rescue_commit_sha: "sha".into(),
            file_count: 0,
            source_issue: None,
            claimant: None,
        };
        let out = format_dirty_quarantine_audit_comment(&input);
        assert!(out.contains("- Interrupted operation: `git rebase`"));
    }

    #[test]
    fn audit_comment_with_claimant_active_run() {
        let input = FormatDirtyQuarantineAuditCommentInput {
            evidence: base_evidence(),
            rescue_branch: "rb".into(),
            rescue_commit_sha: "sha".into(),
            file_count: 1,
            source_issue: None,
            claimant: Some(GitWorktreeBranchContention {
                claimed_by_workspace_id: "ws-2".into(),
                claimed_by_issue_id: Some("iss-2".into()),
                claimed_by_issue_identifier: Some("PROJ-2".into()),
                active_run: Some(ContentionActiveRun {
                    id: "run-9".into(),
                    status: "running".into(),
                }),
            }),
        };
        let out = format_dirty_quarantine_audit_comment(&input);
        assert!(out.contains("- Claimant: workspace `ws-2` on issue [PROJ-2](/PROJ/issues/PROJ-2) with active run `run-9`"));
    }

    #[test]
    fn audit_comment_with_claimant_no_active_run() {
        let input = FormatDirtyQuarantineAuditCommentInput {
            evidence: base_evidence(),
            rescue_branch: "rb".into(),
            rescue_commit_sha: "sha".into(),
            file_count: 1,
            source_issue: None,
            claimant: Some(GitWorktreeBranchContention {
                claimed_by_workspace_id: "ws-2".into(),
                claimed_by_issue_id: None,
                claimed_by_issue_identifier: None,
                active_run: None,
            }),
        };
        let out = format_dirty_quarantine_audit_comment(&input);
        assert!(out.contains("- Claimant: workspace `ws-2` on issue `unknown` with no active run"));
    }

    #[test]
    fn in_progress_operation_labels() {
        assert_eq!(GitWorktreeInProgressOperation::Rebase.as_str(), "rebase");
        assert_eq!(GitWorktreeInProgressOperation::Merge.as_str(), "merge");
        assert_eq!(
            GitWorktreeInProgressOperation::CherryPick.as_str(),
            "cherry-pick"
        );
        assert_eq!(GitWorktreeInProgressOperation::Revert.as_str(), "revert");
        assert_eq!(GitWorktreeInProgressOperation::Bisect.as_str(), "bisect");
    }
}
