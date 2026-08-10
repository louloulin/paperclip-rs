//! Run liveness classifier —— 与 Node `run-liveness.ts` 1:1 对齐。

use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

use crate::types::{
    RunLivenessActionability, RunLivenessClassification, RunLivenessClassificationInput,
    RunLivenessEvidenceInput, RunLivenessIssueInput, RunLivenessState,
    APPROVAL_REQUIRED_RE, BLOCKER_RE, EXTERNAL_BLOCKER_RE, MANAGER_REVIEW_RE,
    NEGATED_BLOCKER_RE, NEXT_STEPS_RE, PLAN_TASK_DESCRIPTION_RE, PLAN_TASK_TITLE_RE,
    PLANNING_ONLY_RE, RUNNABLE_RE, UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON,
    UNMANAGED_BACKGROUND_TASK_STOP_REASON,
};

static NOISY_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:command|status|exit_code|tool|tool_call|tool_result|stdout|stderr|event|payload|session|cwd|ref_id)\s*:"
    ).expect("valid noisy line regex")
});

static NOISY_JSON_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:\{|\[).{0,80}(?:tool|event|stdout|stderr|cmd|command|payload)")
        .expect("valid noisy json regex")
});

static NOISY_SHELL_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\$?\s*(?:rg|sed|cat|ls|git|pnpm|npm|yarn|curl|node|python)\b")
        .expect("valid shell prefix regex")
});

static NEXT_ACTION_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*next(?:\s+steps?|\s+action)?\s*:\s*(.*)$")
        .expect("valid next action line regex")
});

static MD_LIST_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:[-*]|\d+\.)\s+").expect("valid list prefix regex")
});

const REASON_MAX_LEN: usize = 500;
const NEXT_ACTION_MAX_LEN: usize = 500;

/// Compact reason string to max length (与 Node `compactReason` 1:1 对齐).
fn compact_reason(reason: &str) -> String {
    if reason.chars().count() <= REASON_MAX_LEN {
        return reason.to_string();
    }
    let take = REASON_MAX_LEN.saturating_sub(3);
    let prefix: String = reason.chars().take(take).collect();
    format!("{}...", prefix)
}

/// Normalize count to non-negative integer (与 Node `normalizeCount` 1:1 对齐).
fn normalize_count(value: Option<i64>) -> i64 {
    value.map(|v| v.max(0)).unwrap_or(0)
}

/// Normalize continuation attempt (与 Node `normalizeContinuationAttempt` 1:1 对齐).
fn normalize_continuation_attempt(value: Option<i64>) -> i64 {
    value.map(|v| v.max(0)).unwrap_or(0)
}

/// Read text field from JSON.
fn read_text(v: Option<&str>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn read_text_value(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    read_text(Some(s))
}

/// Detect unmanaged background task evidence (与 Node `hasUnmanagedBackgroundTaskEvidence` 1:1 对齐).
fn has_unmanaged_background_task_evidence(result_json: Option<&Value>) -> bool {
    let Some(obj) = result_json.and_then(|v| v.as_object()) else {
        return false;
    };
    if obj.get("stopReason").and_then(|v| v.as_str()) == Some(UNMANAGED_BACKGROUND_TASK_STOP_REASON) {
        return true;
    }
    if let Some(evidence) = obj.get("unmanagedBackgroundTask").and_then(|v| v.as_object()) {
        let stopped = evidence.get("stopped").and_then(|v| v.as_bool()) == Some(true);
        let stop_reason = evidence.get("stopReason").and_then(|v| v.as_str());
        let reason = evidence.get("reason").and_then(|v| v.as_str());
        if stopped
            && (stop_reason == Some(UNMANAGED_BACKGROUND_TASK_STOP_REASON)
                || reason == Some(UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON))
        {
            return true;
        }
    }
    false
}

/// Final structured result text (nextAction / summary / result / message / error).
fn result_final_text(result_json: Option<&Value>) -> String {
    let Some(obj) = result_json.and_then(|v| v.as_object()) else {
        return String::new();
    };
    let fields = ["nextAction", "summary", "result", "message", "error"];
    fields
        .iter()
        .filter_map(|k| read_text_value(obj.get(*k)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Raw stdout/stderr text.
fn result_raw_text(result_json: Option<&Value>) -> String {
    let Some(obj) = result_json.and_then(|v| v.as_object()) else {
        return String::new();
    };
    ["stdout", "stderr"]
        .iter()
        .filter_map(|k| read_text_value(obj.get(*k)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// High-signal sources (issue comments + final result + continuation summary).
fn high_signal_sources(input: &RunLivenessClassificationInput) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(bodies) = &input.issue_comment_bodies {
        for b in bodies {
            if let Some(s) = read_text(Some(b)) {
                out.push(s);
            }
        }
    }
    let final_text = result_final_text(input.result_json.as_ref());
    if let Some(s) = read_text(Some(&final_text)) {
        out.push(s);
    }
    if let Some(s) = read_text(input.continuation_summary_body.as_deref()) {
        out.push(s);
    }
    out
}

/// Raw sources (stdout/stderr + run error).
fn raw_sources(input: &RunLivenessClassificationInput) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let raw_text = result_raw_text(input.result_json.as_ref());
    if let Some(s) = read_text(Some(&raw_text)) {
        out.push(strip_noisy_transcript_lines(&s));
    }
    if let Some(s) = read_text(input.stdout_excerpt.as_deref()) {
        out.push(strip_noisy_transcript_lines(&s));
    }
    if let Some(s) = read_text(input.stderr_excerpt.as_deref()) {
        out.push(strip_noisy_transcript_lines(&s));
    }
    if let Some(s) = read_text(input.error.as_deref()) {
        out.push(strip_noisy_transcript_lines(&s));
    }
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

/// Combined output (high signal + raw).
fn combined_output(input: &RunLivenessClassificationInput) -> String {
    let mut out: Vec<String> = Vec::new();
    out.extend(high_signal_sources(input));
    out.extend(raw_sources(input));
    out.join("\n").trim().to_string()
}

/// Actionability text (high signal preferred; fallback to raw).
fn actionability_text(input: &RunLivenessClassificationInput) -> String {
    let high_signal = high_signal_sources(input).join("\n").trim().to_string();
    if !high_signal.is_empty() {
        return high_signal;
    }
    raw_sources(input).join("\n").trim().to_string()
}

/// Whether input has any useful output.
pub fn has_useful_output(input: &RunLivenessClassificationInput) -> bool {
    !combined_output(input).is_empty()
}

/// Whether input declares a blocker (与 Node `declaredBlocker` 1:1 对齐).
pub fn declared_blocker(input: &RunLivenessClassificationInput) -> bool {
    if input.issue.as_ref().map(|i| i.status.as_str()) == Some("blocked") {
        return true;
    }
    let actionability = classify_run_actionability(input);
    matches!(
        actionability,
        RunLivenessActionability::BlockedExternal | RunLivenessActionability::ApprovalRequired
    )
}

/// Whether input looks like planning only (与 Node `looksLikePlanningOnly` 1:1 对齐).
pub fn looks_like_planning_only(input: &RunLivenessClassificationInput) -> bool {
    let text = actionability_text(input);
    if text.is_empty() {
        return false;
    }
    if PLANNING_ONLY_RE.is_match(&text) || NEXT_STEPS_RE.is_match(&text) {
        return true;
    }
    // "next: ..." / "next steps: ..." / "next action: ..." (case-insensitive)
    Regex::new(r"(?im)^\s*next(?:\s+steps?|\s+action)?\s*:\s*(.+)$")
        .map(|re| re.is_match(&text))
        .unwrap_or(false)
}

/// Whether issue is planning or document task (与 Node `isPlanningOrDocumentTask` 1:1 对齐).
pub fn is_planning_or_document_task(issue: Option<&RunLivenessIssueInput>) -> bool {
    let Some(issue) = issue else {
        return false;
    };
    if PLAN_TASK_TITLE_RE.is_match(&issue.title) {
        return true;
    }
    let desc = issue.description.as_deref().unwrap_or("");
    PLAN_TASK_DESCRIPTION_RE.is_match(desc)
}

/// Normalize evidence (与 Node `normalizeEvidence` 1:1 对齐).
fn normalize_evidence(
    evidence: Option<&RunLivenessEvidenceInput>,
) -> RunLivenessEvidenceInput {
    match evidence {
        Some(e) => RunLivenessEvidenceInput {
            issue_comments_created: normalize_count(Some(e.issue_comments_created)),
            document_revisions_created: normalize_count(Some(e.document_revisions_created)),
            plan_document_revisions_created: normalize_count(Some(e.plan_document_revisions_created)),
            work_products_created: normalize_count(Some(e.work_products_created)),
            workspace_operations_created: normalize_count(Some(e.workspace_operations_created)),
            activity_events_created: normalize_count(Some(e.activity_events_created)),
            tool_or_action_events_created: normalize_count(Some(e.tool_or_action_events_created)),
            latest_evidence_at: e.latest_evidence_at,
        },
        None => RunLivenessEvidenceInput {
            issue_comments_created: 0,
            document_revisions_created: 0,
            plan_document_revisions_created: 0,
            work_products_created: 0,
            workspace_operations_created: 0,
            activity_events_created: 0,
            tool_or_action_events_created: 0,
            latest_evidence_at: None,
        },
    }
}

/// Whether evidence contains concrete actions (与 Node `hasConcreteActionEvidence` 1:1 对齐).
pub fn has_concrete_action_evidence(evidence: Option<&RunLivenessEvidenceInput>) -> bool {
    let n = normalize_evidence(evidence);
    n.issue_comments_created
        + n.document_revisions_created
        + n.work_products_created
        + n.activity_events_created
        + n.tool_or_action_events_created
        > 0
}

/// Evidence reason string.
fn evidence_reason(evidence: &RunLivenessEvidenceInput) -> String {
    let mut parts: Vec<String> = Vec::new();
    if evidence.issue_comments_created > 0 {
        parts.push(format!("{} issue comment(s)", evidence.issue_comments_created));
    }
    if evidence.document_revisions_created > 0 {
        parts.push(format!("{} document revision(s)", evidence.document_revisions_created));
    }
    if evidence.work_products_created > 0 {
        parts.push(format!("{} work product(s)", evidence.work_products_created));
    }
    if evidence.workspace_operations_created > 0 {
        parts.push(format!("{} workspace operation(s)", evidence.workspace_operations_created));
    }
    if evidence.activity_events_created > 0 {
        parts.push(format!("{} activity event(s)", evidence.activity_events_created));
    }
    if evidence.tool_or_action_events_created > 0 {
        parts.push(format!("{} tool/action event(s)", evidence.tool_or_action_events_created));
    }
    parts.join(", ")
}

/// Check if line is noisy (transcript metadata).
fn is_noisy_transcript_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    NOISY_LINE_RE.is_match(trimmed)
        || NOISY_JSON_LINE_RE.is_match(trimmed)
        || NOISY_SHELL_PREFIX_RE.is_match(trimmed)
}

/// Strip noisy transcript lines.
fn strip_noisy_transcript_lines(text: &str) -> String {
    text.lines()
        .map(|line| line.trim())
        .filter(|line| !is_noisy_transcript_line(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Strip markdown list prefix.
fn strip_markdown_list_prefix(line: &str) -> String {
    MD_LIST_PREFIX_RE.replace(line, "").trim().to_string()
}

/// Find next non-noise line.
fn next_non_noise_line(lines: &[&str], start_index: usize) -> Option<String> {
    for (i, line) in lines.iter().enumerate().skip(start_index + 1) {
        let cleaned = strip_markdown_list_prefix(line);
        if cleaned.is_empty() || is_noisy_transcript_line(&cleaned) {
            continue;
        }
        return Some(cleaned);
    }
    None
}

/// Extract next action from a single text block.
fn extract_next_action_from_text(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(|l| l.trim()).collect();
    for (i, raw_line) in lines.iter().enumerate() {
        if raw_line.is_empty() || is_noisy_transcript_line(raw_line) {
            continue;
        }
        let line = strip_markdown_list_prefix(raw_line);
        if let Some(caps) = NEXT_ACTION_LINE_RE.captures(&line) {
            let same_line = strip_markdown_list_prefix(caps.get(1).map(|m| m.as_str()).unwrap_or(""));
            if !same_line.is_empty() {
                return Some(same_line);
            }
            if let Some(next) = next_non_noise_line(&lines, i) {
                return Some(next);
            }
        }
        if PLANNING_ONLY_RE.is_match(&line) {
            return Some(line);
        }
    }
    None
}

/// Extract next action from input (与 Node `extractNextAction` 1:1 对齐).
fn extract_next_action(input: &RunLivenessClassificationInput) -> Option<String> {
    let structured = input
        .result_json
        .as_ref()
        .and_then(|v| v.as_object())
        .and_then(|o| read_text_value(o.get("nextAction")));

    let mut candidates: Vec<String> = Vec::new();
    if let Some(bodies) = &input.issue_comment_bodies {
        candidates.extend(bodies.iter().filter_map(|b| read_text(Some(b))));
    }
    if let Some(s) = structured {
        candidates.push(format!("Next action: {s}"));
    }
    let final_text = result_final_text(input.result_json.as_ref());
    if let Some(s) = read_text(Some(&final_text)) {
        candidates.push(s);
    }
    if let Some(s) = read_text(input.continuation_summary_body.as_deref()) {
        candidates.push(s);
    }
    for raw in raw_sources(input) {
        candidates.push(raw);
    }

    for candidate in candidates {
        if let Some(line) = extract_next_action_from_text(&candidate) {
            let truncated: String = if line.chars().count() <= NEXT_ACTION_MAX_LEN {
                line
            } else {
                let take = NEXT_ACTION_MAX_LEN.saturating_sub(3);
                let prefix: String = line.chars().take(take).collect();
                format!("{}...", prefix)
            };
            return Some(truncated);
        }
    }
    None
}

/// Classify run actionability (与 Node `classifyRunActionability` 1:1 对齐).
pub fn classify_run_actionability(
    input: &RunLivenessClassificationInput,
) -> RunLivenessActionability {
    let text = actionability_text(input);
    if text.is_empty() {
        return RunLivenessActionability::Unknown;
    }
    if NEGATED_BLOCKER_RE.is_match(&text) {
        return if RUNNABLE_RE.is_match(&text) {
            RunLivenessActionability::Runnable
        } else {
            RunLivenessActionability::Unknown
        };
    }
    if APPROVAL_REQUIRED_RE.is_match(&text) {
        return RunLivenessActionability::ApprovalRequired;
    }
    // External blocker: explicit EXTERNAL_BLOCKER_RE OR (BLOCKER_RE with cred/secret/etc keyword)
    let has_external_or_cred_blocker = EXTERNAL_BLOCKER_RE.is_match(&text)
        || (BLOCKER_RE.is_match(&text)
            && Regex::new(r"(?i)\b(?:credential|secret|api key|token|access|input|clarification)\b")
                .map(|re| re.is_match(&text))
                .unwrap_or(false));
    if has_external_or_cred_blocker {
        return RunLivenessActionability::BlockedExternal;
    }
    if MANAGER_REVIEW_RE.is_match(&text) {
        return RunLivenessActionability::ManagerReview;
    }
    if RUNNABLE_RE.is_match(&text) {
        return RunLivenessActionability::Runnable;
    }
    RunLivenessActionability::Unknown
}

/// Classify run liveness (与 Node `classifyRunLiveness` 1:1 对齐).
pub fn classify_run_liveness(
    input: &RunLivenessClassificationInput,
) -> RunLivenessClassification {
    let evidence = normalize_evidence(input.evidence.as_ref());
    let continuation_attempt = normalize_continuation_attempt(input.continuation_attempt);
    let actionability = classify_run_actionability(input);
    let next_action = extract_next_action(input);
    let issue_status = input.issue.as_ref().map(|i| i.status.clone());
    let useful_output = has_useful_output(input);
    let concrete_evidence = has_concrete_action_evidence(Some(&evidence));
    let plan_exempt = is_planning_or_document_task(input.issue.as_ref())
        || evidence.plan_document_revisions_created > 0;
    let last_useful_action_at: Option<DateTime<Utc>> = if concrete_evidence {
        evidence.latest_evidence_at
    } else {
        None
    };

    let output = |state: RunLivenessState,
                  reason: String,
                  next_action: Option<String>|
     -> RunLivenessClassification {
        let last = match state {
            RunLivenessState::Advanced
            | RunLivenessState::Completed
            | RunLivenessState::Blocked => last_useful_action_at,
            _ => None,
        };
        RunLivenessClassification {
            liveness_state: state,
            liveness_reason: compact_reason(&reason),
            continuation_attempt,
            last_useful_action_at: last,
            next_action,
            actionability,
        }
    };

    if input.run_status == "interrupted" {
        let reason = match &input.error_code {
            Some(code) => format!("Run interrupted ({code})"),
            None => "Run interrupted".to_string(),
        };
        return output(RunLivenessState::NeedsFollowup, reason, None);
    }

    if input.run_status != "succeeded" {
        if has_unmanaged_background_task_evidence(input.result_json.as_ref()) {
            return output(
                RunLivenessState::Failed,
                UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON.to_string(),
                None,
            );
        }
        let reason = match &input.error_code {
            Some(code) => format!("Run ended with {} ({})", input.run_status, code),
            None => format!("Run ended with {}", input.run_status),
        };
        return output(RunLivenessState::Failed, reason, None);
    }

    if let Some(status) = &issue_status {
        if status == "done" || status == "cancelled" {
            return output(
                RunLivenessState::Completed,
                format!("Issue is {status}"),
                None,
            );
        }
    }

    if declared_blocker(input) {
        let reason = if issue_status.as_deref() == Some("blocked") {
            "Issue status is blocked".to_string()
        } else {
            "Run output declared a concrete blocker".to_string()
        };
        return output(RunLivenessState::Blocked, reason, next_action);
    }

    if !useful_output && !concrete_evidence {
        return output(
            RunLivenessState::EmptyResponse,
            "Run succeeded without useful output or concrete action evidence".to_string(),
            None,
        );
    }

    if concrete_evidence {
        return output(
            RunLivenessState::Advanced,
            format!("Run produced concrete action evidence: {}", evidence_reason(&evidence)),
            None,
        );
    }

    if plan_exempt && useful_output {
        return output(
            RunLivenessState::Advanced,
            "Planning/document task produced useful output and is exempt from plan-only classification".to_string(),
            None,
        );
    }

    if looks_like_planning_only(input) || next_action.is_some() {
        return if actionability == RunLivenessActionability::Runnable {
            output(
                RunLivenessState::PlanOnly,
                "Run described runnable future work without concrete action evidence".to_string(),
                next_action,
            )
        } else {
            output(
                RunLivenessState::NeedsFollowup,
                "Run described future work that is not safe to auto-continue".to_string(),
                next_action,
            )
        };
    }

    if useful_output {
        return output(
            RunLivenessState::NeedsFollowup,
            "Run produced useful output but no concrete action evidence".to_string(),
            next_action,
        );
    }

    output(
        RunLivenessState::EmptyResponse,
        "Run succeeded without useful output".to_string(),
        None,
    )
}
