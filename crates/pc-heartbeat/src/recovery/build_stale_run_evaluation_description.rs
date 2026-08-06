//! `buildStaleRunEvaluationDescription` —— Node `services/recovery/service.ts:1902`。
//!
//! 业务语义：
//! - 当 heartbeat 检测到 active run 输出沉默（suspicious 或 critical）时，生成 evaluation issue
//!   的 description 字段。
//! - 内容包含 Run / Last Output Excerpt / Recent Run Events / Related Work / Decision Checklist
//!   五段，便于人工决定 Continue / Cancel / False positive。
//!
//! 设计意图：
//! - pure 函数：输入 view struct 集合 + 输出 String
//! - 内部 helper `format_duration` 复用
//! - 与 Node 完全对齐：每行 bullet / section header 完全一致
//! - 输入 view 独立定义（避免循环依赖完整 row struct）

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Node 中 `ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS = 60min` 等价（来自 readiness.rs）。
/// 用于在 description 中渲染"thresholds" 段。
pub const ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS: i64 = 60 * 60 * 1_000;
/// Node 中 `ACTIVE_RUN_OUTPUT_CRITICAL_THRESHOLD_MS = suspicion × 4` 默认倍率。
pub const ACTIVE_RUN_OUTPUT_CRITICAL_THRESHOLD_MS: i64 =
    ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS * 4;

/// Heartbeat run 的最少化 view（避免强依赖完整 HeartbeatRow）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleRunView {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub invocation_source: String,
    pub trigger_detail: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub process_started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_output_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_output_seq: i32,
    pub process_pid: Option<i32>,
    pub process_group_id: Option<i32>,
}

/// Agent 的最少化 view。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleAgentView {
    pub id: Uuid,
    pub name: String,
    pub adapter_type: String,
}

/// Source issue 的最少化 view（None 表示 run 无 source issue）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleSourceIssueView {
    pub id: Uuid,
    pub identifier: Option<String>,
}

/// Recent run event view（来自 heartbeat_run_events 表）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleRunEventView {
    pub event_type: String,
    pub level: Option<String>,
    pub created_at: String,
    pub message: Option<String>,
}

/// Issue link view（用于 child issues / blockers 列表）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleIssueLinkView {
    pub id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
}

/// Evidence 集合（与 Node `collectStaleRunEvidence` 返回类型对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleRunEvidenceView {
    pub safe_tail: Option<String>,
    pub silence_age_ms: i64,
    pub recent_events: Vec<StaleRunEventView>,
    pub child_issues: Vec<StaleIssueLinkView>,
    pub blockers: Vec<StaleIssueLinkView>,
}

/// `buildStaleRunEvaluationDescription` 的输入。
#[derive(Debug, Clone)]
pub struct BuildStaleRunEvaluationDescriptionInput<'a> {
    pub run: &'a StaleRunView,
    pub running_agent: &'a StaleAgentView,
    pub source_issue: Option<&'a StaleSourceIssueView>,
    pub prefix: &'a str,
    pub evidence: &'a StaleRunEvidenceView,
    pub level: StaleEvaluationLevel,
    /// 可选 redaction 配置；传入后 safe_tail 与 event message 会被脱敏。
    pub redaction: Option<super::redact_watchdog_evidence_text::CurrentUserRedactionOptions>,
}

/// 评估等级（与 Node `level: "suspicious" | "critical"` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleEvaluationLevel {
    Suspicious,
    Critical,
}

impl StaleEvaluationLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Suspicious => "suspicious",
            Self::Critical => "critical",
        }
    }
}

/// Node `formatDuration` 的 Rust 等价。
///
/// - `None` / `null` → `"unknown"`
/// - < 60 min → `"Xm"`
/// - >= 60 min → `"Xh Ym"` (有剩余分钟) 或 `"Xh"`
pub fn format_duration(ms: Option<i64>) -> String {
    let Some(ms) = ms else {
        return "unknown".to_owned();
    };
    if ms < 0 {
        return "0m".to_owned();
    }
    let minutes = ms / 60_000;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;
    if remaining_minutes > 0 {
        format!("{hours}h {remaining_minutes}m")
    } else {
        format!("{hours}h")
    }
}

fn issue_link(view: &StaleIssueLinkView, prefix: &str) -> String {
    let label = view
        .identifier
        .clone()
        .unwrap_or_else(|| view.id.to_string());
    format!("[{label}](/{prefix}/issues/{label})")
}

fn run_link(view: &StaleRunView, prefix: &str) -> String {
    format!(
        "[{}](/{prefix}/agents/{}/runs/{})",
        view.id, view.agent_id, view.id
    )
}

/// Node `buildStaleRunEvaluationDescription` 的 Rust 等价。
pub fn build_stale_run_evaluation_description(
    input: &BuildStaleRunEvaluationDescriptionInput<'_>,
) -> String {
    let source_issue_link = input
        .source_issue
        .map(|s| {
            let label = s.identifier.clone().unwrap_or_else(|| s.id.to_string());
            format!("[{label}](/{}/issues/{label})", input.prefix)
        })
        .unwrap_or_else(|| "none".to_owned());

    let recent_events = if input.evidence.recent_events.is_empty() {
        "- none".to_owned()
    } else {
        input
            .evidence
            .recent_events
            .iter()
            .map(|event| {
                let level_suffix = event
                    .level
                    .as_deref()
                    .map(|l| format!(" {l}"))
                    .unwrap_or_default();
                let msg = event
                    .message
                    .clone()
                    .map(|m| apply_redaction(&m, input.redaction.as_ref()))
                    .unwrap_or_else(|| "(no message)".to_owned());
                format!(
                    "- {} `{}`{level_suffix}: {msg}",
                    event.created_at, event.event_type
                )
            })
            .collect::<Vec<_>>()
            .join(
                "
",
            )
    };

    let child_issues = if input.evidence.child_issues.is_empty() {
        "- none detected".to_owned()
    } else {
        input
            .evidence
            .child_issues
            .iter()
            .map(|issue| {
                format!(
                    "- {} `{}`: {}",
                    issue_link(issue, input.prefix),
                    issue.status,
                    issue.title
                )
            })
            .collect::<Vec<_>>()
            .join(
                "
",
            )
    };

    let blockers = if input.evidence.blockers.is_empty() {
        "- none detected".to_owned()
    } else {
        input
            .evidence
            .blockers
            .iter()
            .map(|issue| {
                format!(
                    "- {} `{}`: {}",
                    issue_link(issue, input.prefix),
                    issue.status,
                    issue.title
                )
            })
            .collect::<Vec<_>>()
            .join(
                "
",
            )
    };

    let last_output_line = input
        .run
        .last_output_at
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "none recorded".to_owned());

    let invocation_line = match input.run.trigger_detail.as_deref() {
        Some(detail) => format!("{} / {}", input.run.invocation_source, detail),
        None => input.run.invocation_source.clone(),
    };

    let tail_text = input
        .evidence
        .safe_tail
        .as_deref()
        .map(|t| apply_redaction(t, input.redaction.as_ref()));
    let tail_block = tail_text
        .as_ref()
        .map(|t| {
            format!(
                "```text
{t}
```"
            )
        })
        .unwrap_or_else(|| "_No run-log tail was available._".to_owned());

    [
        format!(
            "Paperclip detected {} output silence on an active heartbeat run.",
            input.level.as_str()
        ),
        String::new(),
        "## Run".to_owned(),
        String::new(),
        format!("- Run: {}", run_link(input.run, input.prefix)),
        format!(
            "- Agent: {} ({})",
            input.running_agent.name, input.running_agent.adapter_type
        ),
        format!("- Invocation: {invocation_line}"),
        format!("- Source issue: {source_issue_link}"),
        format!(
            "- Started at: {}",
            input
                .run
                .started_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "unknown".to_owned())
        ),
        format!(
            "- Process started at: {}",
            input
                .run
                .process_started_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "unknown".to_owned())
        ),
        format!("- Last output at: {last_output_line}"),
        format!("- Last output sequence: {}", input.run.last_output_seq),
        format!(
            "- Silent for: {}",
            format_duration(Some(input.evidence.silence_age_ms))
        ),
        format!(
            "- Thresholds: suspicious after {}, critical after {}",
            format_duration(Some(ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS)),
            format_duration(Some(ACTIVE_RUN_OUTPUT_CRITICAL_THRESHOLD_MS))
        ),
        format!(
            "- Process metadata: pid `{}`, process group `{}`, in-memory handle `unknown`",
            input
                .run
                .process_pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            input
                .run
                .process_group_id
                .map(|p| p.to_string())
                .unwrap_or_else(|| "unknown".to_owned())
        ),
        String::new(),
        "## Last Output Excerpt".to_owned(),
        String::new(),
        tail_block,
        String::new(),
        "## Recent Run Events".to_owned(),
        String::new(),
        recent_events,
        String::new(),
        "## Related Work".to_owned(),
        String::new(),
        "Active child issues:".to_owned(),
        child_issues,
        String::new(),
        "Current source blockers:".to_owned(),
        blockers,
        String::new(),
        "## Decision Checklist".to_owned(),
        String::new(),
        "- Continue or snooze if the run is intentionally quiet.".to_owned(),
        "- Ask the run owner for context if work may be delegated outside the transcript."
            .to_owned(),
        "- Preserve artifacts, branch state, and useful output before cancellation.".to_owned(),
        "- Cancel or recover through the explicit run recovery controls when authorized."
            .to_owned(),
        "- Close this issue as a false positive only after recording the reason.".to_owned(),
    ]
    .join(
        "
",
    )
}

fn apply_redaction(
    input: &str,
    options: Option<&super::redact_watchdog_evidence_text::CurrentUserRedactionOptions>,
) -> String {
    match options {
        Some(opts) => {
            super::redact_watchdog_evidence_text::redact_watchdog_evidence_text(input, opts.clone())
        }
        None => input.to_owned(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn uuid(seed: u8) -> Uuid {
        Uuid::from_bytes([seed; 16])
    }

    fn epoch(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn sample_run() -> StaleRunView {
        StaleRunView {
            id: uuid(1),
            agent_id: uuid(2),
            invocation_source: "manual".to_owned(),
            trigger_detail: Some("r334-trigger".to_owned()),
            started_at: Some(epoch(2024, 1, 1, 10, 0)),
            process_started_at: Some(epoch(2024, 1, 1, 10, 1)),
            last_output_at: Some(epoch(2024, 1, 1, 10, 30)),
            last_output_seq: 42,
            process_pid: Some(1234),
            process_group_id: Some(5678),
        }
    }

    fn sample_agent() -> StaleAgentView {
        StaleAgentView {
            id: uuid(2),
            name: "engineer-1".to_owned(),
            adapter_type: "process".to_owned(),
        }
    }

    fn sample_source() -> StaleSourceIssueView {
        StaleSourceIssueView {
            id: uuid(3),
            identifier: Some("ROOT-1".to_owned()),
        }
    }

    fn sample_evidence() -> StaleRunEvidenceView {
        StaleRunEvidenceView {
            safe_tail: Some("last output line".to_owned()),
            silence_age_ms: 30 * 60_000,
            recent_events: vec![
                StaleRunEventView {
                    event_type: "log".to_owned(),
                    level: Some("info".to_owned()),
                    created_at: "2024-01-01T10:30:00Z".to_owned(),
                    message: Some("started".to_owned()),
                },
                StaleRunEventView {
                    event_type: "log".to_owned(),
                    level: None,
                    created_at: "2024-01-01T10:31:00Z".to_owned(),
                    message: None,
                },
            ],
            child_issues: vec![StaleIssueLinkView {
                id: uuid(4),
                identifier: Some("CHILD-1".to_owned()),
                title: "child".to_owned(),
                status: "todo".to_owned(),
            }],
            blockers: vec![StaleIssueLinkView {
                id: uuid(5),
                identifier: None,
                title: "blocker".to_owned(),
                status: "blocked".to_owned(),
            }],
        }
    }

    // ===== format_duration =====
    #[test]
    fn format_duration_handles_none() {
        assert_eq!(format_duration(None), "unknown");
    }

    #[test]
    fn format_duration_handles_minutes() {
        assert_eq!(format_duration(Some(0)), "0m");
        assert_eq!(format_duration(Some(30 * 60_000)), "30m");
        assert_eq!(format_duration(Some(59 * 60_000)), "59m");
    }

    #[test]
    fn format_duration_handles_hours_only() {
        assert_eq!(format_duration(Some(60 * 60_000)), "1h");
        assert_eq!(format_duration(Some(120 * 60_000)), "2h");
    }

    #[test]
    fn format_duration_handles_hours_and_minutes() {
        assert_eq!(format_duration(Some(90 * 60_000)), "1h 30m");
        assert_eq!(format_duration(Some(125 * 60_000)), "2h 5m");
    }

    #[test]
    fn format_duration_handles_negative() {
        assert_eq!(format_duration(Some(-1)), "0m");
    }

    // ===== build_stale_run_evaluation_description =====
    fn build() -> String {
        let input = BuildStaleRunEvaluationDescriptionInput {
            redaction: None,
            run: &sample_run(),
            running_agent: &sample_agent(),
            source_issue: Some(&sample_source()),
            prefix: "PAP",
            evidence: &sample_evidence(),
            level: StaleEvaluationLevel::Critical,
        };
        build_stale_run_evaluation_description(&input)
    }

    #[test]
    fn description_starts_with_level_header() {
        let body = build();
        assert!(body
            .starts_with("Paperclip detected critical output silence on an active heartbeat run."));
    }

    #[test]
    fn description_includes_all_sections() {
        let body = build();
        assert!(body.contains("## Run"));
        assert!(body.contains("## Last Output Excerpt"));
        assert!(body.contains("## Recent Run Events"));
        assert!(body.contains("## Related Work"));
        assert!(body.contains("Active child issues:"));
        assert!(body.contains("Current source blockers:"));
        assert!(body.contains("## Decision Checklist"));
    }

    #[test]
    fn description_run_link_uses_prefix_and_agent() {
        let body = build();
        assert!(body.contains("- Run: [01010101-0101-0101-0101-010101010101](/PAP/agents/02020202-0202-0202-0202-020202020202/runs/01010101-0101-0101-0101-010101010101)"));
    }

    #[test]
    fn description_agent_line_has_name_and_adapter() {
        let body = build();
        assert!(body.contains("- Agent: engineer-1 (process)"));
    }

    #[test]
    fn description_invocation_includes_trigger_detail() {
        let body = build();
        assert!(body.contains("- Invocation: manual / r334-trigger"));
    }

    #[test]
    fn description_invocation_no_trigger_detail() {
        let mut run = sample_run();
        run.trigger_detail = None;
        let input = BuildStaleRunEvaluationDescriptionInput {
            redaction: None,
            run: &run,
            running_agent: &sample_agent(),
            source_issue: Some(&sample_source()),
            prefix: "PAP",
            evidence: &sample_evidence(),
            level: StaleEvaluationLevel::Suspicious,
        };
        let body = build_stale_run_evaluation_description(&input);
        assert!(body.contains("- Invocation: manual"));
        assert!(!body.contains("- Invocation: manual / "));
    }

    #[test]
    fn description_thresholds_use_constants() {
        let body = build();
        assert!(body.contains("- Thresholds: suspicious after 1h, critical after 4h"));
    }

    #[test]
    fn description_silent_for_uses_evidence_silence_age() {
        let body = build();
        assert!(body.contains("- Silent for: 30m"));
    }

    #[test]
    fn description_process_metadata_uses_run_pid_group() {
        let body = build();
        assert!(body.contains(
            "- Process metadata: pid `1234`, process group `5678`, in-memory handle `unknown`"
        ));
    }

    #[test]
    fn description_recent_events_render_messages() {
        let body = build();
        assert!(body.contains("2024-01-01T10:30:00Z `log` info: started"));
        assert!(body.contains("2024-01-01T10:31:00Z `log`: (no message)"));
    }

    #[test]
    fn description_child_issue_uses_identifier() {
        let body = build();
        assert!(body.contains("- [CHILD-1](/PAP/issues/CHILD-1) `todo`: child"));
    }

    #[test]
    fn description_blocker_without_identifier_uses_uuid() {
        let body = build();
        assert!(body.contains("- [05050505-0505-0505-0505-050505050505](/PAP/issues/05050505-0505-0505-0505-050505050505) `blocked`: blocker"));
    }

    #[test]
    fn description_empty_collections_render_placeholder() {
        let mut evidence = sample_evidence();
        evidence.recent_events.clear();
        evidence.child_issues.clear();
        evidence.blockers.clear();
        evidence.safe_tail = None;
        let input = BuildStaleRunEvaluationDescriptionInput {
            redaction: None,
            run: &sample_run(),
            running_agent: &sample_agent(),
            source_issue: None,
            prefix: "PAP",
            evidence: &evidence,
            level: StaleEvaluationLevel::Suspicious,
        };
        let body = build_stale_run_evaluation_description(&input);
        assert!(body.contains("- none"));
        assert!(body.contains("- none detected"));
        assert!(body.contains("_No run-log tail was available._"));
        assert!(body.contains("- Source issue: none"));
    }

    #[test]
    fn description_source_issue_none_renders_placeholder() {
        let evidence = sample_evidence();
        let input = BuildStaleRunEvaluationDescriptionInput {
            redaction: None,
            run: &sample_run(),
            running_agent: &sample_agent(),
            source_issue: None,
            prefix: "PAP",
            evidence: &evidence,
            level: StaleEvaluationLevel::Critical,
        };
        let body = build_stale_run_evaluation_description(&input);
        assert!(body.contains("- Source issue: none"));
    }

    #[test]
    fn description_decision_checklist_contains_all_five_items() {
        let body = build();
        assert!(body.contains("Continue or snooze"));
        assert!(body.contains("Ask the run owner for context"));
        assert!(body.contains("Preserve artifacts"));
        assert!(body.contains("Cancel or recover through the explicit run recovery controls"));
        assert!(body.contains("Close this issue as a false positive"));
    }
}
