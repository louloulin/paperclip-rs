//! `buildRecoveryIssueInPlaceEscalationComment` —— Node `services/recovery/service.ts:3095`。
//!
//! 业务语义：
//! - 当 stranded-recovery 升级时（RecoveryInPlace 路径），在目标 issue 上写一条
//!   system 评论，说明：
//!   1) 这次升级是哪条 recovery issue 的（带 UI 链接）
//!   2) 升级前的状态 (previous status)
//!   3) 最近一次 run 的链接 / 状态
//!   4) retry reason
//!   5) 是否存在被 withhold 的 failure 摘要
//!   6) Guard 说明（recovery issue 不会再创建嵌套的 stranded_issue_recovery）
//!   7) Next action：人工解 blocked 的指引
//!
//! 设计意图：
//! - pure 函数：输入 view struct + 输出 String
//! - 复用 `summarize_run_failure_for_issue_comment`（已存在）
//! - 输入 view 独立定义（EscalationRunView），不依赖完整 HeartbeatRunRow
//! - 与 Node 完全对齐：相同的 bullet list / 链接格式 / 兜底（"none" / "unknown"）
//!
//! 调用方：
//! - `escalate_stranded_recovery_issue_in_place`（escalate_db.rs）—— 升级时写 system 评论
//! - 任何需要给用户/操作员解释 "为什么 recovery issue 被锁死在 blocked" 的场景

use crate::recovery::summarize_run_failure::{
    summarize_run_failure_for_issue_comment, RunFailureView,
};
use serde_json::Value;
use uuid::Uuid;

/// 稳定的 in-place 升级评论前缀，用于 dedup 判定（body 含此 marker 即跳过）。
pub const IN_PLACE_ESCALATION_MARKER: &str =
    "Paperclip stopped automatic stranded-work recovery for this recovery issue.";

/// Latest run 的最少化 view（用于避免强依赖完整 HeartbeatRunRow）。
///
/// `agent_id` 为 Option 以匹配 Node `runUiLink` 在缺失时的可空语义（fallback）。
/// `error` / `error_code` 直接用作 `summarize_run_failure_for_issue_comment` 的输入。
/// `context_snapshot` 持有 owned `Value` 以简化调用方 ergonomics。
#[derive(Debug, Clone)]
pub struct EscalationRunView {
    pub id: Uuid,
    pub agent_id: Option<Uuid>,
    pub status: String,
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub context_snapshot: Option<Value>,
}

/// `buildRecoveryIssueInPlaceEscalationComment` 的输入 view。
///
/// 与 Node 输入对齐：
/// - `issue_identifier` 可空（与 Node `identifier: string | null` 一致）
/// - `issue_id` 必有（UI link fallback）
/// - `previous_status` 必有
/// - `latest_run` 可空
/// - `prefix` 必有（company 的 issue_prefix）
#[derive(Debug, Clone)]
pub struct BuildRecoveryIssueInPlaceEscalationCommentInput {
    pub issue_identifier: Option<String>,
    pub issue_id: Uuid,
    pub previous_status: String,
    pub latest_run: Option<EscalationRunView>,
    pub prefix: String,
}

/// Node `buildRecoveryIssueInPlaceEscalationComment` 的 Rust 等价。
///
/// 输出固定为 markdown 文本，含 8 行 bullet + 标题 + Next action。
pub fn build_recovery_issue_in_place_escalation_comment(
    input: &BuildRecoveryIssueInPlaceEscalationCommentInput,
) -> String {
    let issue_label = input
        .issue_identifier
        .clone()
        .unwrap_or_else(|| input.issue_id.to_string());
    let issue_link = format!(
        "[{}](/{}/issues/{})",
        issue_label, input.prefix, issue_label
    );

    let (run_link, latest_status) = match input.latest_run.as_ref() {
        Some(run) => {
            let link = match run.agent_id {
                Some(agent_id) => format!(
                    "[{}](/{}/agents/{}/runs/{})",
                    run.id, input.prefix, agent_id, run.id
                ),
                None => format!("[{}](unknown agent)", run.id),
            };
            (link, run.status.clone())
        }
        None => ("none".to_owned(), "unknown".to_owned()),
    };

    let retry_reason = extract_retry_reason(
        input
            .latest_run
            .as_ref()
            .and_then(|r| r.context_snapshot.as_ref()),
    );

    let failure_summary = match input.latest_run.as_ref() {
        Some(run) => {
            let view = RunFailureView {
                error: run.error.as_deref(),
                error_code: run.error_code.as_deref(),
            };
            summarize_run_failure_for_issue_comment(Some(&view))
                .map(|s| format!("Latest retry failure details were withheld from the issue thread; inspect the linked run for evidence."))
                .unwrap_or_default()
        }
        None => String::new(),
    };

    let failure_line = if failure_summary.is_empty() {
        "- Failure: none recorded".to_owned()
    } else {
        // Node 拼成 "- Failure: <summary>"（summary 由 summarizeRunFailureForIssueComment 返回
        // 的字符串已含 leading space，trim 后再做拼接以保持干净的格式）
        format!("- Failure: {}", failure_summary.trim())
    };

    [
        IN_PLACE_ESCALATION_MARKER,
        "",
        &format!("- Recovery issue: {issue_link}"),
        &format!("- Previous status: `{}`", input.previous_status),
        &format!("- Latest run: {run_link}"),
        &format!("- Latest run status: `{latest_status}`"),
        &format!("- Retry reason: `{retry_reason}`"),
        &failure_line,
        "- Guard: recovery issues do not create nested `stranded_issue_recovery` issues.",
        "",
        "Next action: the current recovery owner should inspect the failed run evidence, restore a live execution path or record the manual resolution, then move this recovery issue out of `blocked`.",
    ]
    .join("\n")
}

/// 从 context_snapshot.retryReason 读取 trim 后非空字符串，否则返回 "none"。
///
/// 与 Node `readNonEmptyString(parseObject(contextSnapshot)?.retryReason) ?? "none"` 对齐。
fn extract_retry_reason(context_snapshot: Option<&Value>) -> String {
    let Some(snapshot) = context_snapshot else {
        return "none".to_owned();
    };
    let Some(obj) = snapshot.as_object() else {
        return "none".to_owned();
    };
    let Some(reason_value) = obj.get("retryReason") else {
        return "none".to_owned();
    };
    let Some(reason_str) = reason_value.as_str() else {
        return "none".to_owned();
    };
    let trimmed = reason_str.trim();
    if trimmed.is_empty() {
        "none".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 单元测试：retryReason 提取
    #[test]
    fn extract_retry_reason_handles_missing_snapshot() {
        assert_eq!(extract_retry_reason(None), "none");
    }

    #[test]
    fn extract_retry_reason_handles_non_object_snapshot() {
        let v = json!("not an object");
        assert_eq!(extract_retry_reason(Some(&v)), "none");
    }

    #[test]
    fn extract_retry_reason_handles_missing_key() {
        let v = json!({"other": "x"});
        assert_eq!(extract_retry_reason(Some(&v)), "none");
    }

    #[test]
    fn extract_retry_reason_handles_non_string_value() {
        let v = json!({"retryReason": 42});
        assert_eq!(extract_retry_reason(Some(&v)), "none");
    }

    #[test]
    fn extract_retry_reason_returns_trimmed_value() {
        let v = json!({"retryReason": "  issue_continuation_needed  "});
        assert_eq!(extract_retry_reason(Some(&v)), "issue_continuation_needed");
    }

    #[test]
    fn extract_retry_reason_returns_none_for_whitespace() {
        let v = json!({"retryReason": "   "});
        assert_eq!(extract_retry_reason(Some(&v)), "none");
    }

    /// 单元测试：完整路径不依赖 DB
    #[test]
    fn builds_full_body_with_all_fields() {
        let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
            issue_identifier: Some("PAP-1".to_owned()),
            issue_id: Uuid::nil(),
            previous_status: "in_progress".to_owned(),
            latest_run: Some(EscalationRunView {
                id: Uuid::nil(),
                agent_id: Some(Uuid::nil()),
                status: "failed".to_owned(),
                error: Some("boom".to_owned()),
                error_code: None,
                context_snapshot: Some(json!({"retryReason": "issue_continuation_needed"})),
            }),
            prefix: "PAP".to_owned(),
        };
        let body = build_recovery_issue_in_place_escalation_comment(&input);
        assert!(body.starts_with(IN_PLACE_ESCALATION_MARKER));
        assert!(body.contains("- Recovery issue: [PAP-1](/PAP/issues/PAP-1)"));
        assert!(body.contains("- Previous status: `in_progress`"));
        assert!(body.contains("- Retry reason: `issue_continuation_needed`"));
        assert!(body.contains("- Failure: Latest retry failure details were withheld"));
    }

    /// 单元测试：identifier 为 None 时用 uuid 兜底
    #[test]
    fn identifier_none_uses_uuid_as_link_label() {
        let issue_id = Uuid::from_u128(0xdeadbeef_u128);
        let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
            issue_identifier: None,
            issue_id,
            previous_status: "todo".to_owned(),
            latest_run: None,
            prefix: "PAP".to_owned(),
        };
        let body = build_recovery_issue_in_place_escalation_comment(&input);
        let expected_label = format!("{}", issue_id);
        assert!(body.contains(&format!(
            "[{}](/PAP/issues/{})",
            expected_label, expected_label
        )));
    }

    /// 单元测试：latest_run None
    #[test]
    fn missing_latest_run_renders_placeholders() {
        let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
            issue_identifier: Some("PAP-7".to_owned()),
            issue_id: Uuid::nil(),
            previous_status: "todo".to_owned(),
            latest_run: None,
            prefix: "ACME".to_owned(),
        };
        let body = build_recovery_issue_in_place_escalation_comment(&input);
        assert!(body.contains("- Latest run: none"));
        assert!(body.contains("- Latest run status: `unknown`"));
        assert!(body.contains("- Retry reason: `none`"));
        assert!(body.contains("- Failure: none recorded"));
    }

    /// 单元测试：latest_run.agent_id 为 None 时仍产出 run 链接
    #[test]
    fn latest_run_with_no_agent_id_still_renders_link() {
        let run_id = Uuid::from_u128(1234_u128);
        let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
            issue_identifier: Some("PAP-1".to_owned()),
            issue_id: Uuid::nil(),
            previous_status: "todo".to_owned(),
            latest_run: Some(EscalationRunView {
                id: run_id,
                agent_id: None,
                status: "failed".to_owned(),
                error: None,
                error_code: None,
                context_snapshot: Some(json!({})),
            }),
            prefix: "PAP".to_owned(),
        };
        let body = build_recovery_issue_in_place_escalation_comment(&input);
        // link 应该存在（带 fallback 文本）
        assert!(body.contains(&format!("- Latest run: [{run_id}]")));
        assert!(!body.contains("- Latest run: none"));
    }

    /// 单元测试：clean run（无 error / error_code）输出 "none recorded"
    #[test]
    fn clean_run_renders_none_recorded() {
        let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
            issue_identifier: Some("PAP-1".to_owned()),
            issue_id: Uuid::nil(),
            previous_status: "in_progress".to_owned(),
            latest_run: Some(EscalationRunView {
                id: Uuid::nil(),
                agent_id: Some(Uuid::nil()),
                status: "succeeded".to_owned(),
                error: None,
                error_code: None,
                context_snapshot: Some(json!({})),
            }),
            prefix: "PAP".to_owned(),
        };
        let body = build_recovery_issue_in_place_escalation_comment(&input);
        assert!(body.contains("- Failure: none recorded"));
        assert!(!body.contains("withheld"));
    }

    /// 单元测试：error_code alone 仍触发 failure summary
    #[test]
    fn error_code_alone_triggers_failure_summary() {
        let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
            issue_identifier: Some("PAP-1".to_owned()),
            issue_id: Uuid::nil(),
            previous_status: "in_progress".to_owned(),
            latest_run: Some(EscalationRunView {
                id: Uuid::nil(),
                agent_id: Some(Uuid::nil()),
                status: "failed".to_owned(),
                error: None,
                error_code: Some("adapter_failed".to_owned()),
                context_snapshot: Some(json!({})),
            }),
            prefix: "PAP".to_owned(),
        };
        let body = build_recovery_issue_in_place_escalation_comment(&input);
        assert!(body.contains("- Failure: Latest retry failure details were withheld"));
    }

    /// 单元测试：完整结构包含所有 Node 对齐的 key markers
    #[test]
    fn body_includes_guard_and_next_action() {
        let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
            issue_identifier: Some("PAP-1".to_owned()),
            issue_id: Uuid::nil(),
            previous_status: "todo".to_owned(),
            latest_run: None,
            prefix: "PAP".to_owned(),
        };
        let body = build_recovery_issue_in_place_escalation_comment(&input);
        assert!(body.contains(
            "- Guard: recovery issues do not create nested `stranded_issue_recovery` issues."
        ));
        assert!(body.contains("Next action:"));
        assert!(body.contains("move this recovery issue out of `blocked`"));
    }
}
