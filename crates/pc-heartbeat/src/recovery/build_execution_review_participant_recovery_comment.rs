//! `buildExecutionReviewParticipantRecoveryComment` —— Node `services/recovery/service.ts:315`。
//!
//! 业务语义：
//! - 当 execution-review participant 自动重试一次但仍无 completed decision 或 live reviewer run 时，
//!   触发此函数生成 escalation comment body。
//! - 内容：解释当前状态（已重试但 review 阶段仍卡住）+ 可能附 failure summary。
//! - 引导 recovery owner：修复 reviewer runtime / 恢复 review stage / 记录人工解决方案。
//!
//! 设计意图：
//! - pure 函数：输入 view struct + 输出 String
//! - 复用 `summarize_run_failure_for_issue_comment`（已存在）
//! - 输入 view 与 `build_recovery_issue_in_place_escalation_comment::EscalationRunView` 形状一致；
//!   复用 `EscalationRunView` 而不另设独立 struct（与 R327 一致）
//!
//! 与 buildExecutionReviewParticipantUnavailableComment 的区别：
//! - 本函数：自动重试已经发生过一次但仍失败
//! - unavailable 函数：participant 不可调用，且 review 阶段无 completed decision / live reviewer run

use crate::recovery::build_recovery_issue_in_place_escalation_comment::EscalationRunView;
use crate::recovery::summarize_run_failure::{
    summarize_run_failure_for_issue_comment, RunFailureView,
};

/// Node `buildExecutionReviewParticipantRecoveryComment` 的 Rust 等价。
///
/// - 输入：latest run view（含 error / error_code / context_snapshot）
/// - 输出：comment body（markdown 风格的长文 + failure summary）
pub fn build_execution_review_participant_recovery_comment(
    latest_run: &EscalationRunView,
) -> String {
    let failure_summary = summarize_run_failure_for_issue_comment(Some(&RunFailureView {
        error: latest_run.error.as_deref(),
        error_code: latest_run.error_code.as_deref(),
    }))
    .unwrap_or("");

    [
        "Paperclip retried the pending execution-review participant once, but the review stage still has no completed decision or live reviewer run.",
        failure_summary,
        " Moving it to `blocked` with a source-scoped recovery action so the recovery owner can repair the reviewer runtime, restore the review stage, or record an intentional manual resolution.",
    ]
    .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view(error: Option<&str>, error_code: Option<&str>) -> EscalationRunView {
        EscalationRunView {
            id: uuid::Uuid::nil(),
            agent_id: Some(uuid::Uuid::nil()),
            status: "failed".to_owned(),
            error: error.map(str::to_owned),
            error_code: error_code.map(str::to_owned),
            context_snapshot: Some(json!({})),
        }
    }

    #[test]
    fn clean_run_produces_no_failure_summary() {
        let body = build_execution_review_participant_recovery_comment(&view(None, None));
        assert!(body.starts_with("Paperclip retried the pending execution-review participant once"));
        assert!(body.contains("or live reviewer run."));
        // clean run → no failure summary inserted
        assert!(!body.contains("withheld"));
        assert!(body.contains("Moving it to `blocked`"));
        assert!(body.contains("source-scoped recovery action"));
        assert!(body.contains("manual resolution."));
    }

    #[test]
    fn failed_run_includes_failure_summary() {
        let body = build_execution_review_participant_recovery_comment(&view(Some("boom"), None));
        assert!(body.contains("Latest retry failure details were withheld"));
    }

    #[test]
    fn error_code_only_includes_failure_summary() {
        let body = build_execution_review_participant_recovery_comment(&view(
            None,
            Some("adapter_failed"),
        ));
        assert!(body.contains("Latest retry failure details were withheld"));
    }

    #[test]
    fn body_contains_recovery_owner_guidance() {
        let body = build_execution_review_participant_recovery_comment(&view(None, None));
        assert!(body.contains("repair the reviewer runtime"));
        assert!(body.contains("restore the review stage"));
        assert!(body.contains("manual resolution"));
    }
}
