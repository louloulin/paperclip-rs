#![forbid(unsafe_code)]
//! 状态卡 "stalled generation" 终结化的纯逻辑 helper（原 `pc-status-card-finalization` 已下沉）。
//!
//! 对应 Node `server/src/services/status-card-finalization.ts`（73 行）。
//!
//! 设计目标：1:1 复刻
//! - `STALLED_GENERATION_STATUSES = {"done", "cancelled", "blocked"}` 集合
//! - `failureReasonForIssue(issue)` —— 根据 status 生成对应的 failure reason 文案
//! - `isStalledGeneration(status)` —— 判断 status 是否触发 stalled 终结化
//!
//! DB 写操作（`finalizeStatusCardsForStalledGeneration`）由上层接入 `pc-repos`；
//! 本 crate 只暴露纯逻辑 helper。

/// Stalled generation 状态集合 —— 与 Node `STALLED_GENERATION_STATUSES` 1:1 对齐。
pub const STALLED_GENERATION_STATUSES: &[&str] = &["done", "cancelled", "blocked"];

/// Issue status enum（针对 stalled generation 三态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StalledStatus {
    Done,
    Cancelled,
    Blocked,
}

impl StalledStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::Blocked => "blocked",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// 判断 status 是否触发 stalled generation 终结化。
///
/// 与 Node `STALLED_GENERATION_STATUSES.has(issue.status)` 1:1 对齐。
pub fn is_stalled_generation(status: &str) -> bool {
    STALLED_GENERATION_STATUSES.contains(&status)
}

/// 简化的 issue 信息 —— 1:1 对应 Node `StalledGenerationIssue`。
#[derive(Debug, Clone)]
pub struct StalledGenerationIssue<'a> {
    pub id: &'a str,
    pub company_id: &'a str,
    pub identifier: Option<&'a str>,
    pub title: &'a str,
    pub status: &'a str,
}

/// 根据 issue 信息生成 failure reason 文案。
///
/// 与 Node `failureReasonForIssue` 1:1 对齐。
pub fn failure_reason_for_issue(issue: &StalledGenerationIssue<'_>) -> String {
    let label = match issue.identifier {
        Some(ident) => format!("{}: {}", ident, issue.title),
        None => issue.title.to_string(),
    };
    match issue.status {
        "cancelled" => format!(
            "Status-card generation task {label} was cancelled before writing a summary."
        ),
        "blocked" => format!(
            "Status-card generation task {label} was blocked before writing a summary; re-run to retry."
        ),
        _ => format!(
            "Status-card generation task {label} finished without writing a summary."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r706_stalled_set_contains_three_statuses() {
        assert_eq!(STALLED_GENERATION_STATUSES.len(), 3);
        assert!(is_stalled_generation("done"));
        assert!(is_stalled_generation("cancelled"));
        assert!(is_stalled_generation("blocked"));
    }

    #[test]
    fn r706_stalled_set_excludes_other_statuses() {
        assert!(!is_stalled_generation("queued"));
        assert!(!is_stalled_generation("running"));
        assert!(!is_stalled_generation("backlog"));
        assert!(!is_stalled_generation(""));
    }

    #[test]
    fn r706_failure_reason_cancelled() {
        let issue = StalledGenerationIssue {
            id: "i1",
            company_id: "c1",
            identifier: Some("ISSUE-1"),
            title: "Generate summary",
            status: "cancelled",
        };
        let r = failure_reason_for_issue(&issue);
        assert!(r.contains("ISSUE-1: Generate summary"));
        assert!(r.contains("cancelled before writing a summary"));
    }

    #[test]
    fn r706_failure_reason_blocked() {
        let issue = StalledGenerationIssue {
            id: "i1",
            company_id: "c1",
            identifier: None,
            title: "Generate summary",
            status: "blocked",
        };
        let r = failure_reason_for_issue(&issue);
        assert!(r.contains("Generate summary"));
        assert!(r.contains("blocked before writing a summary"));
        assert!(r.contains("re-run to retry"));
        // 没有 identifier 前缀
        assert!(!r.starts_with(": "));
    }

    #[test]
    fn r706_failure_reason_done_default_branch() {
        let issue = StalledGenerationIssue {
            id: "i1",
            company_id: "c1",
            identifier: Some("ISSUE-2"),
            title: "Done task",
            status: "done",
        };
        let r = failure_reason_for_issue(&issue);
        assert!(r.contains("ISSUE-2: Done task"));
        assert!(r.contains("finished without writing a summary"));
    }

    #[test]
    fn r706_failure_reason_with_unknown_status_uses_default() {
        // 即使 status 未知也走 default 分支（与 Node switch 语义一致）
        let issue = StalledGenerationIssue {
            id: "i1",
            company_id: "c1",
            identifier: None,
            title: "T",
            status: "unknown",
        };
        let r = failure_reason_for_issue(&issue);
        assert!(r.contains("finished without writing a summary"));
    }

    #[test]
    fn r706_stalled_status_enum_round_trip() {
        for s in [
            StalledStatus::Done,
            StalledStatus::Cancelled,
            StalledStatus::Blocked,
        ] {
            assert_eq!(StalledStatus::from_str(s.as_str()), Some(s));
        }
        assert_eq!(StalledStatus::from_str("unknown"), None);
    }

    #[test]
    fn r706_identifier_or_title_label() {
        let with_id = StalledGenerationIssue {
            id: "i1",
            company_id: "c1",
            identifier: Some("ID"),
            title: "T",
            status: "cancelled",
        };
        let without_id = StalledGenerationIssue {
            id: "i1",
            company_id: "c1",
            identifier: None,
            title: "T",
            status: "cancelled",
        };
        let r1 = failure_reason_for_issue(&with_id);
        let r2 = failure_reason_for_issue(&without_id);
        assert!(r1.starts_with("Status-card generation task ID: T"));
        assert!(r2.starts_with("Status-card generation task T"));
    }
}
