//! `summarize_run_failure_for_issue_comment` —— Node `services/recovery/service.ts:306`。
//!
//! 业务语义：
//! - 当 recovery escalation comment 中需要包含 "failure details withheld" 提示时调用
//! - 若 latest run 有 error / errorCode，返回固定字符串（不暴露原始错误）
//! - 若 latest run 干净（无 error / errorCode），返回 None（不附加任何内容）
//!
//! 设计意图：
//! - 这是一个纯函数（pure），无副作用
//! - 输入是 `&LatestIssueRun`（或最少化字段的 view struct）
//! - 输出是 `Option<String>` —— `None` 表示无附加内容，`Some(_)` 表示附加这段文字
//! - 与 Node 一致：不会泄露真实错误内容（error / errorCode），只提示"已记录但不出现在 issue 评论"
//!
//! 用例：
//! - `escalateStrandedAssignedIssue` 的多处 `failureSummary` 拼接
//! - `buildExecutionReviewParticipantRecoveryComment` / `buildExecutionReviewParticipantUnavailableComment`
//! - `buildRecoveryIssueInPlaceEscalationComment`
use serde::{Deserialize, Serialize};

/// `summarize_run_failure_for_issue_comment` 的最小化输入 view。
///
/// 设计为独立 struct 而非强依赖 `LatestIssueRun` —— 让本模块可在没有完整
/// `heartbeat_run` 行的情况下被调用（例如仅从 result_json / context_snapshot 推断）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunFailureView<'a> {
    pub error: Option<&'a str>,
    pub error_code: Option<&'a str>,
}

/// Node `summarizeRunFailureForIssueComment` 的 Rust 等价。
///
/// - 若 `error` 或 `error_code` 任一非空 → 返回固定字符串
/// - 否则 → 返回 None
pub fn summarize_run_failure_for_issue_comment(
    run: Option<&RunFailureView<'_>>,
) -> Option<&'static str> {
    let run = run?;
    let has_error = run.error.map(str::trim).map_or(false, |s| !s.is_empty());
    let has_error_code = run
        .error_code
        .map(str::trim)
        .map_or(false, |s| !s.is_empty());
    if has_error || has_error_code {
        Some(
            " Latest retry failure details were withheld from the issue thread; inspect the linked run for evidence.",
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(error: Option<&'a str>, error_code: Option<&'a str>) -> RunFailureView<'a> {
        RunFailureView { error, error_code }
    }

    #[test]
    fn returns_some_when_error_present() {
        let v = view(Some("boom"), None);
        let result = summarize_run_failure_for_issue_comment(Some(&v));
        assert!(result.is_some());
        assert!(result.unwrap().contains("withheld"));
    }

    #[test]
    fn returns_some_when_error_code_present() {
        let v = view(None, Some("adapter_failed"));
        let result = summarize_run_failure_for_issue_comment(Some(&v));
        assert!(result.is_some());
    }

    #[test]
    fn returns_some_when_both_present() {
        let v = view(Some("boom"), Some("adapter_failed"));
        assert!(summarize_run_failure_for_issue_comment(Some(&v)).is_some());
    }

    #[test]
    fn returns_none_when_no_error_and_no_error_code() {
        let v = view(None, None);
        assert!(summarize_run_failure_for_issue_comment(Some(&v)).is_none());
    }

    #[test]
    fn returns_none_when_run_is_none() {
        assert!(summarize_run_failure_for_issue_comment(None).is_none());
    }

    #[test]
    fn returns_none_when_empty_strings() {
        let v = view(Some(""), Some(""));
        assert!(summarize_run_failure_for_issue_comment(Some(&v)).is_none());
    }

    #[test]
    fn returns_none_when_whitespace_only() {
        let v = view(Some("   "), Some("\t"));
        assert!(summarize_run_failure_for_issue_comment(Some(&v)).is_none());
    }

    #[test]
    fn error_takes_priority_over_whitespace_error_code() {
        let v = view(Some("real error"), Some("   "));
        assert!(summarize_run_failure_for_issue_comment(Some(&v)).is_some());
    }
}
