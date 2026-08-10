//! Markdown building helpers —— 与 Node `issue-continuation-summary.ts` 1:1 对齐。
//!
//! 设计：
//! - 所有 helper 都是纯函数（无 DB I/O）
//! - 复用 `crate::types` 中定义的常量
//! - 主入口：`build_continuation_summary_markdown`

use regex::Regex;
use std::sync::LazyLock;

use crate::types::{
    BuildContinuationSummaryInput, ContinuationSummaryMode, IssueSummaryInput, RunSummaryInput,
    ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS, PATH_CANDIDATE_RE,
    SUMMARY_SECTION_MAX_CHARS, WAITING_FOR_REVIEW_OR_APPROVAL_RE,
};

static MD_HEADING_RE: LazyLock<Regex> = LazyLock::new(|| {
    // Section extractor: ^##\s+heading\s*$([\s\S]*?)(?=^##\s+|(?![\s\S]))
    Regex::new(r"(?m)^##\s+([^$\n]+?)\s*$\n([\s\S]*?)(?=^##\s+|\z)")
        .expect("valid section regex")
});

/// Truncate text to max chars（与 Node `truncateText` 1:1 对齐）。
fn truncate_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let take = max_chars.saturating_sub(20);
    let prefix: String = trimmed.chars().take(take).collect();
    format!("{}\n[truncated]", prefix.trim_end())
}

/// 通用 truncate（针对 &str -> &str interface）。
fn truncate_str(value: &str, max_chars: usize) -> String {
    truncate_text(value, max_chars)
}

/// 把字符串中的非空字符串取出（与 Node `asNonEmptyString` 1:1 对齐）。
fn as_non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 从 resultJson 中提取 summary 字段（与 Node `readResultSummary` 1:1 对齐）。
///
/// 优先字段顺序：summary / result / message / error
pub fn read_result_summary(result_json: Option<&serde_json::Value>) -> Option<String> {
    let obj = result_json?;
    if !obj.is_object() {
        return None;
    }
    let obj = obj.as_object()?;
    for key in &["summary", "result", "message", "error"] {
        if let Some(s) = obj.get(*key).and_then(|v| v.as_str()) {
            if let Some(result) = as_non_empty_string(s) {
                return Some(result);
            }
        }
    }
    None
}

/// Extract a markdown section by heading (## Heading) — 与 Node `extractMarkdownSection` 1:1 对齐。
///
/// Rust regex crate doesn't support lookahead, so we parse sections manually.
pub fn extract_markdown_section(markdown: Option<&str>, heading: &str) -> Option<String> {
    let md = markdown?;
    let lines: Vec<&str> = md.lines().collect();
    let mut current_heading: Option<&str> = None;
    let mut current_body_start: usize = 0;
    let mut result: Option<String> = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(stripped) = trimmed.strip_prefix("## ") {
            // Push previous section
            if let Some(h) = current_heading {
                if h.eq_ignore_ascii_case(heading) && result.is_none() {
                    let body = lines[current_body_start..i].join("\n");
                    let body = body.trim();
                    if !body.is_empty() {
                        result = Some(truncate_str(body, SUMMARY_SECTION_MAX_CHARS));
                    }
                }
            }
            current_heading = Some(stripped.trim());
            current_body_start = i + 1;
        } else if trimmed.starts_with("#") && !trimmed.starts_with("##") {
            // Reset on # heading (h1)
            current_heading = None;
        }
    }
    // Final section
    if let Some(h) = current_heading {
        if h.eq_ignore_ascii_case(heading) && result.is_none() {
            let body = lines[current_body_start..].join("\n");
            let body = body.trim();
            if !body.is_empty() {
                result = Some(truncate_str(body, SUMMARY_SECTION_MAX_CHARS));
            }
        }
    }
    result
}

/// Extract file path candidates from text content (max 12) —— 与 Node `extractPathCandidates` 1:1 对齐。
pub fn extract_path_candidates<I, S>(texts: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for text in texts {
        let text = text.as_ref();
        if text.is_empty() {
            continue;
        }
        for cap in PATH_CANDIDATE_RE.captures_iter(text) {
            let raw = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            // strip trailing punctuation
            let path = raw.trim_end_matches(|c: char| "),.;:".contains(c));
            if path.is_empty() {
                continue;
            }
            if seen.insert(path.to_string()) {
                out.push(path.to_string());
            }
            if seen.len() >= 12 {
                return out;
            }
        }
        if seen.len() >= 12 {
            return out;
        }
    }
    out
}

/// Infer continuation mode（与 Node `inferMode` 1:1 对齐）。
pub fn infer_mode(issue: &IssueSummaryInput, run: &RunSummaryInput) -> ContinuationSummaryMode {
    if issue.status == "done" || issue.status == "in_review" {
        return ContinuationSummaryMode::Review;
    }
    if matches!(
        run.status.as_str(),
        "failed" | "timed_out" | "cancelled" | "interrupted"
    ) {
        return ContinuationSummaryMode::Implementation;
    }
    if issue.status == "backlog" || issue.status == "todo" {
        return ContinuationSummaryMode::Plan;
    }
    ContinuationSummaryMode::Implementation
}

/// Infer next action（与 Node `inferNextAction` 1:1 对齐）。
pub fn infer_next_action(
    issue: &IssueSummaryInput,
    run: &RunSummaryInput,
    previous_next_action: Option<&str>,
) -> String {
    if issue.status == "done" {
        return "Review the completed issue output and close any remaining follow-up comments."
            .to_string();
    }
    if issue.status == "in_review" {
        return "Wait for reviewer feedback or approval before continuing executor work.".to_string();
    }
    if matches!(run.status.as_str(), "failed" | "timed_out") {
        return "Inspect the failed run, fix the cause, and resume from the most recent concrete action above.".to_string();
    }
    if run.status == "cancelled" {
        return "Confirm the cancellation reason before starting another run.".to_string();
    }
    previous_next_action
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            "Resume implementation from the acceptance criteria, latest comments, and this summary."
                .to_string()
        })
}

/// Format bullet list（与 Node `bulletList` 1:1 对齐）。
fn bullet_list(items: &[String], empty: &str) -> String {
    if items.is_empty() {
        format!("- {empty}")
    } else {
        items
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Extract previous next action from a markdown body（与 Node `extractPreviousNextAction` 1:1 对齐）。
pub fn extract_previous_next_action(previous_body: Option<&str>) -> Option<String> {
    let section = extract_markdown_section(previous_body, "Next Action")?;
    section
        .lines()
        .map(|line| line.trim_start_matches(|c: char| c == '-' || c == '*').trim())
        .find(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Extract continuation summary next action（与 Node `extractContinuationSummaryNextAction` 1:1 对齐）。
pub fn extract_continuation_summary_next_action(body: Option<&str>) -> Option<String> {
    extract_previous_next_action(body)
}

/// Whether the continuation summary parks the executor（与 Node `continuationSummaryParksExecutor` 1:1 对齐）。
pub fn continuation_summary_parks_executor(body: Option<&str>) -> bool {
    let next_action = match extract_continuation_summary_next_action(body) {
        Some(s) => s,
        None => return false,
    };
    WAITING_FOR_REVIEW_OR_APPROVAL_RE.is_match(&next_action)
}

/// Main builder —— 与 Node `buildContinuationSummaryMarkdown` 1:1 对齐。
pub fn build_continuation_summary_markdown(input: &BuildContinuationSummaryInput) -> String {
    let issue = &input.issue;
    let run = &input.run;
    let agent = &input.agent;

    let result_summary = read_result_summary(input.run.result_json.as_ref());

    // Recent actions
    let mut recent_actions: Vec<String> = Vec::new();
    let run_finished_line = match run.finished_at {
        Some(t) => format!(
            "Run `{}` finished with status `{}` at {}.",
            run.id,
            run.status,
            t.to_rfc3339()
        ),
        None => format!("Run `{}` finished with status `{}`.", run.id, run.status),
    };
    recent_actions.push(run_finished_line);
    recent_actions.push(match result_summary.as_deref() {
        Some(s) => truncate_str(s, SUMMARY_SECTION_MAX_CHARS),
        None => "No adapter-provided result summary was captured for this run.".to_string(),
    });
    if let Some(err) = &run.error {
        let error_line = match run.error_code.as_deref() {
            Some(code) => format!(
                "Latest run error ({}): {}",
                code,
                truncate_str(err, 500)
            ),
            None => format!("Latest run error: {}", truncate_str(err, 500)),
        };
        recent_actions.push(error_line);
    }

    // Paths
    let path_inputs: Vec<Option<&str>> = vec![
        result_summary.as_deref(),
        run.stdout_excerpt.as_deref(),
        run.stderr_excerpt.as_deref(),
        input.previous_summary_body.as_deref(),
    ];
    let path_refs: Vec<&str> = path_inputs.iter().filter_map(|x| *x).collect();
    let paths = extract_path_candidates(path_refs);

    // Objective + acceptance criteria
    let objective = extract_markdown_section(issue.description.as_deref(), "Objective")
        .or_else(|| issue.description.as_deref().map(|d| d.trim().to_string()))
        .unwrap_or_else(|| "No objective captured.".to_string());
    let acceptance_criteria = extract_markdown_section(
        issue.description.as_deref(),
        "Acceptance Criteria",
    )
    .unwrap_or_else(|| "No explicit acceptance criteria captured.".to_string());

    let mode = infer_mode(issue, run);
    let next_action = infer_next_action(
        issue,
        run,
        extract_previous_next_action(input.previous_summary_body.as_deref()).as_deref(),
    );

    let sections: Vec<String> = vec![
        "# Continuation Summary".to_string(),
        String::new(),
        format!(
            "- Issue: {} — {}",
            issue.identifier.clone().unwrap_or_else(|| issue.id.clone()),
            issue.title
        ),
        format!("- Status: {}", issue.status),
        format!("- Priority: {}", issue.priority),
        format!("- Current mode: {}", mode.as_str()),
        format!("- Last updated by run: {}", run.id),
        format!(
            "- Agent: {} ({})",
            agent.name,
            agent.adapter_type.clone().unwrap_or_else(|| "unknown".to_string())
        ),
        String::new(),
        "## Objective".to_string(),
        String::new(),
        truncate_str(&objective, SUMMARY_SECTION_MAX_CHARS),
        String::new(),
        "## Acceptance Criteria".to_string(),
        String::new(),
        acceptance_criteria,
        String::new(),
        "## Recent Concrete Actions".to_string(),
        String::new(),
        bullet_list(&recent_actions, "No recent actions captured."),
        String::new(),
        "## Files / Routes Touched".to_string(),
        String::new(),
        bullet_list(
            &paths.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>(),
            "No file or route paths were detected in the captured run summary.",
        ),
        String::new(),
        "## Commands Run".to_string(),
        String::new(),
        bullet_list(
            &[
                format!(
                    "Heartbeat run `{}` invoked adapter `{}`.",
                    run.id,
                    agent.adapter_type.clone().unwrap_or_else(|| "unknown".to_string())
                ),
                "Detailed shell/tool commands remain in the run log and transcript.".to_string(),
            ],
            "No command metadata captured.",
        ),
        String::new(),
        "## Blockers / Decisions".to_string(),
        String::new(),
        bullet_list(
            &if let Some(err) = &run.error {
                vec![format!(
                    "Latest run ended with `{}`; inspect the error before continuing.",
                    run.status
                )]
            } else {
                vec!["No new blocker was recorded by the latest run.".to_string()]
            },
            "No blockers or decisions captured.",
        ),
        String::new(),
        "## Next Action".to_string(),
        String::new(),
        format!("- {next_action}"),
    ];

    let body = sections.join("\n");
    truncate_str(&body, ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS)
}
