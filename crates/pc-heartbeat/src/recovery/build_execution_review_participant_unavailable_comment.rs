//! `buildExecutionReviewParticipantUnavailableComment` —— Node `services/recovery/service.ts:325`。
//!
//! 业务语义：
//! - 当 execution-review participant 不可调用（not invokable）且 review 阶段无
//!   completed decision / live reviewer run 时，触发此函数生成 escalation comment body。
//! - 与 `build_execution_review_participant_recovery_comment` 不同：这里**没有发生自动重试**，
//!   是因为 participant 不可用，所以直接生成 unavailable 提示。
//!
//! 设计意图：
//! - pure 函数：输入 view struct + 输出 String
//! - 复用 `summarize_run_failure_for_issue_comment`（已存在）
//! - 输入 view 复用 `EscalationRunView`（与 R330 一致）
//!
//! 与 buildExecutionReviewParticipantRecoveryComment 的区别（措辞）：
//! - "Paperclip cannot continue the pending execution-review participant because the participant is not invokable"
//! - "and the review stage has no completed decision or live reviewer run"
//! - 后续引导部分一致

use crate::recovery::build_recovery_issue_in_place_escalation_comment::EscalationRunView;
use crate::recovery::summarize_run_failure::{
    summarize_run_failure_for_issue_comment, RunFailureView,
};

/// Node `buildExecutionReviewParticipantUnavailableComment` 的 Rust 等价。
///
/// - 输入：latest run view（含 error / error_code / context_snapshot）
/// - 输出：comment body（说明 participant 不可用 + 可能附 failure summary）
pub fn build_execution_review_participant_unavailable_comment(
    latest_run: &EscalationRunView,
) -> String {
    build_execution_review_participant_unavailable_comment_optional(Some(latest_run))
}

pub fn build_execution_review_participant_unavailable_comment_optional(
    latest_run: Option<&EscalationRunView>,
) -> String {
    let failure_summary = summarize_run_failure_for_issue_comment(Some(&RunFailureView {
        error: latest_run.and_then(|run| run.error.as_deref()),
        error_code: latest_run.and_then(|run| run.error_code.as_deref()),
    }))
    .unwrap_or("");

    [
        "Paperclip cannot continue the pending execution-review participant because the participant is not invokable and the review stage has no completed decision or live reviewer run.",
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
        let body = build_execution_review_participant_unavailable_comment(&view(None, None));
        assert!(
            body.starts_with("Paperclip cannot continue the pending execution-review participant")
        );
        assert!(body.contains("participant is not invokable"));
        assert!(!body.contains("withheld"));
    }

    #[test]
    fn failed_run_includes_failure_summary() {
        let body = build_execution_review_participant_unavailable_comment(&view(Some("err"), None));
        assert!(body.contains("withheld"));
    }

    #[test]
    fn body_contains_recovery_owner_guidance() {
        let body = build_execution_review_participant_unavailable_comment(&view(None, None));
        assert!(body.contains("Moving it to `blocked`"));
        assert!(body.contains("source-scoped recovery action"));
        assert!(body.contains("repair the reviewer runtime"));
        assert!(body.contains("restore the review stage"));
        assert!(body.contains("manual resolution"));
    }

    #[test]
    fn diff_from_recovery_comment_starts_with_cannot_continue() {
        // sanity check：与 build_execution_review_participant_recovery_comment 区分
        let body = build_execution_review_participant_unavailable_comment(&view(None, None));
        assert!(
            !body.starts_with("Paperclip retried the pending execution-review participant once")
        );
        assert!(body.starts_with("Paperclip cannot continue"));
    }
}
