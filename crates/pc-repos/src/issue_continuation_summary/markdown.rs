//! Continuation summary markdown 构造（纯逻辑，零 IO）。
//!
//! 单一职责：把 issue + run + agent 元数据拼装成 markdown 格式的 continuation summary。
//!
//! 与 Node `server/src/services/issue-continuation-summary.ts` 的纯函数部分 1:1 对齐：
//! - `truncateText` / `asNonEmptyString` / `readResultSummary`
//! - `extractMarkdownSection` / `extractPathCandidates`
//! - `inferMode` / `inferNextAction` / `bulletList`
//! - `extractContinuationSummaryNextAction` / `continuationSummaryParksExecutor`
//! - `buildContinuationSummaryMarkdown`（主入口）

use regex::Regex;

use super::types::{
    AgentSummaryInput, BuildSummaryInput, IssueSummaryInput, RunSummaryInput,
    ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS, SUMMARY_SECTION_MAX_CHARS,
};

// ============================================================================
// Regex patterns
// ============================================================================

/// 文件路径候选匹配（与 Node `PATH_CANDIDATE_RE` 1:1 对齐）：
/// 匹配 `(server|ui|packages|doc|scripts|.github)/...` 前缀的路径。
const PATH_CANDIDATE_RE_STR: &str =
    r#"(?:^|[\s`"'(])((?:server|ui|packages|doc|scripts|\.github)/[A-Za-z0-9._/-]+)"#;

/// "wait for review/approval/..." 匹配（与 Node `WAITING_FOR_REVIEW_OR_APPROVAL_RE` 1:1 对齐）。
const WAITING_FOR_REVIEW_OR_APPROVAL_RE_STR: &str =
    r"(?i)\bwait(?:ing)? for\b.{0,160}\b(?:review(?:er)?(?: feedback)?|approval|board|human|user|operator)\b";

// ============================================================================
// Trivial helpers
// ============================================================================

/// 截断文本（与 Node `truncateText` 1:1 对齐）。
///
/// - 长度 ≤ max_chars → 原样返回
/// - 长度 > max_chars → 截断 + 末尾 `\n[truncated]` 标记
pub fn truncate_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let keep = max_chars.saturating_sub(20);
    let truncated: String = trimmed.chars().take(keep).collect();
    format!("{}\n[truncated]", truncated.trim_end())
}

/// Non-empty trimmed string（与 Node `asNonEmptyString` 1:1 对齐）。
pub fn as_non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if !trimmed.is_empty() {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// 从 result_json 提取 result summary（与 Node `readResultSummary` 1:1 对齐）。
///
/// 按 `summary` / `result` / `message` / `error` 顺序找第一个非空字符串。
pub fn read_result_summary(result_json: Option<&serde_json::Value>) -> Option<String> {
    let v = result_json?;
    let obj = v.as_object()?;
    for key in ["summary", "result", "message", "error"] {
        if let Some(s) = obj.get(key).and_then(|x| x.as_str()) {
            if let Some(nes) = as_non_empty_string(s) {
                return Some(nes);
            }
        }
    }
    None
}

/// 提取 markdown `## Heading` 段（与 Node `extractMarkdownSection` 1:1 对齐）。
///
/// 匹配 `^## <Heading>\n...` 直到下一个 `^## ` 或字符串结尾；返回 truncateText 后的内容。
pub fn extract_markdown_section(markdown: Option<&str>, heading: &str) -> Option<String> {
    let md = markdown?;
    let heading_trim = heading.trim();
    let mut lines = md.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start_matches('#').trim_start();
        if let Some(rest) = trimmed.strip_prefix(heading_trim) {
            if rest.chars().all(|c| c.is_whitespace()) {
                let mut content = String::new();
                for next in lines.by_ref() {
                    if next.starts_with("## ") {
                        break;
                    }
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(next);
                }
                let section = content.trim();
                if section.is_empty() {
                    return None;
                }
                return Some(truncate_text(section, SUMMARY_SECTION_MAX_CHARS));
            }
        }
    }
    None
}

// ============================================================================
// Path candidate extraction
// ============================================================================

/// 从文本数组中提取路径候选（与 Node `extractPathCandidates` 1:1 对齐）。
///
/// 上限 12 个 distinct 路径；按出现顺序去重；清理尾部标点 `)`,`.`,`;`,`:`。
pub fn extract_path_candidates(texts: &[Option<&str>]) -> Vec<String> {
    let re = match Regex::new(PATH_CANDIDATE_RE_STR) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut seen: Vec<String> = Vec::new();
    for text in texts.iter().flatten() {
        for cap in re.captures_iter(text) {
            let raw = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let cleaned = raw.trim_end_matches(|c: char| matches!(c, ')' | ',' | '.' | ';' | ':'));
            if cleaned.is_empty() {
                continue;
            }
            if !seen.iter().any(|p| p == cleaned) {
                seen.push(cleaned.to_string());
            }
            if seen.len() >= 12 {
                return seen;
            }
        }
        if seen.len() >= 12 {
            return seen;
        }
    }
    seen
}

// ============================================================================
// Mode + next action inference
// ============================================================================

/// Summary mode 推断（与 Node `inferMode` 1:1 对齐）。
///
/// - issue `done` / `in_review` → `"review"`
/// - run `failed` / `timed_out` / `cancelled` / `interrupted` → `"implementation"`
/// - issue `backlog` / `todo` → `"plan"`
/// - 默认 → `"implementation"`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryMode {
    Review,
    Implementation,
    Plan,
}

impl SummaryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Implementation => "implementation",
            Self::Plan => "plan",
        }
    }
}

pub fn infer_mode(issue: &IssueSummaryInput, run: &RunSummaryInput) -> SummaryMode {
    if issue.status == "done" || issue.status == "in_review" {
        return SummaryMode::Review;
    }
    if matches!(
        run.status.as_str(),
        "failed" | "timed_out" | "cancelled" | "interrupted"
    ) {
        return SummaryMode::Implementation;
    }
    if issue.status == "backlog" || issue.status == "todo" {
        return SummaryMode::Plan;
    }
    SummaryMode::Implementation
}

/// Next action 推断（与 Node `inferNextAction` 1:1 对齐）。
pub fn infer_next_action(
    issue: &IssueSummaryInput,
    run: &RunSummaryInput,
    previous_next_action: Option<&str>,
) -> String {
    if issue.status == "done" {
        return "Review the completed issue output and close any remaining follow-up comments.".to_string();
    }
    if issue.status == "in_review" {
        return "Wait for reviewer feedback or approval before continuing executor work.".to_string();
    }
    if run.status == "failed" || run.status == "timed_out" {
        return "Inspect the failed run, fix the cause, and resume from the most recent concrete action above.".to_string();
    }
    if run.status == "cancelled" {
        return "Confirm the cancellation reason before starting another run.".to_string();
    }
    previous_next_action
        .map(str::to_string)
        .unwrap_or_else(|| {
            "Resume implementation from the acceptance criteria, latest comments, and this summary.".to_string()
        })
}

// ============================================================================
// Markdown composition helpers
// ============================================================================

/// Bullet list 渲染（与 Node `bulletList` 1:1 对齐）。
///
/// 空列表 → 单个 `- <empty>`；非空 → 每个一行 `- item`。
pub fn bullet_list(items: &[String], empty: &str) -> String {
    if items.is_empty() {
        return format!("- {}", empty);
    }
    items
        .iter()
        .map(|item| format!("- {}", item))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 从已存在 summary 提取 `## Next Action` 段的第一条（与 Node `extractPreviousNextAction` 1:1 对齐）。
pub fn extract_previous_next_action(previous_body: Option<&str>) -> Option<String> {
    let section = extract_markdown_section(previous_body, "Next Action")?;
    section
        .lines()
        .map(|line| line.trim_start_matches(|c: char| matches!(c, '-' | '*')).trim_start())
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

/// 从 summary body 提取 next action（与 Node `extractContinuationSummaryNextAction` 1:1 对齐）。
pub fn extract_continuation_summary_next_action(body: Option<&str>) -> Option<String> {
    extract_previous_next_action(body)
}

/// 判定 next action 是否阻塞 executor（与 Node `continuationSummaryParksExecutor` 1:1 对齐）。
///
/// 当 next action 包含 "wait for ... review/approval/board/human/user/operator" 时为 true。
pub fn continuation_summary_parks_executor(body: Option<&str>) -> bool {
    let Some(next_action) = extract_continuation_summary_next_action(body) else {
        return false;
    };
    let re = match Regex::new(WAITING_FOR_REVIEW_OR_APPROVAL_RE_STR) {
        Ok(r) => r,
        Err(_) => return false,
    };
    re.is_match(&next_action)
}

// ============================================================================
// Main builder
// ============================================================================

/// 构造 continuation summary markdown（与 Node `buildContinuationSummaryMarkdown` 1:1 对齐）。
///
/// 章节顺序：`# Continuation Summary` / 元数据 / `## Objective` / `## Acceptance Criteria` /
/// `## Recent Concrete Actions` / `## Files / Routes Touched` / `## Commands Run` /
/// `## Blockers / Decisions` / `## Next Action`。
///
/// 最终 body truncate 到 `ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS`。
pub fn build_continuation_summary_markdown(input: &BuildSummaryInput) -> String {
    let issue = &input.issue;
    let run = &input.run;
    let agent = &input.agent;

    let result_summary = read_result_summary(run.result_json.as_ref());
    let mut recent_actions: Vec<String> = vec![
        format!(
            "Run `{}` finished with status `{}`{}.",
            run.id,
            run.status,
            run.finished_at
                .map(|t| format!(" at {}", t.to_rfc3339()))
                .unwrap_or_default()
        ),
        result_summary
            .as_deref()
            .map(|s| truncate_text(s, SUMMARY_SECTION_MAX_CHARS))
            .unwrap_or_else(|| {
                "No adapter-provided result summary was captured for this run.".to_string()
            }),
    ];
    if let Some(err) = run.error.as_deref() {
        let prefix = if let Some(code) = run.error_code.as_deref() {
            format!("Latest run error ({}): ", code)
        } else {
            "Latest run error: ".to_string()
        };
        recent_actions.push(format!("{}{}", prefix, truncate_text(err, 500)));
    }

    let paths = extract_path_candidates(&[
        result_summary.as_deref(),
        run.stdout_excerpt.as_deref(),
        run.stderr_excerpt.as_deref(),
        input.previous_summary_body.as_deref(),
    ]);

    let objective = extract_markdown_section(issue.description.as_deref(), "Objective")
        .or_else(|| issue.description.as_deref().map(str::trim).map(str::to_string))
        .unwrap_or_else(|| "No objective captured.".to_string());

    let acceptance_criteria = extract_markdown_section(issue.description.as_deref(), "Acceptance Criteria")
        .unwrap_or_else(|| "No explicit acceptance criteria captured.".to_string());

    let mode = infer_mode(issue, run);
    let previous_next = extract_previous_next_action(input.previous_summary_body.as_deref());
    let next_action = infer_next_action(issue, run, previous_next.as_deref());

    let header = format!(
        "# Continuation Summary\n\n- Issue: {} — {}\n- Status: {}\n- Priority: {}\n- Current mode: {}\n- Last updated by run: {}\n- Agent: {} ({})",
        issue.identifier.as_deref().unwrap_or(&issue.id),
        issue.title,
        issue.status,
        issue.priority,
        mode.as_str(),
        run.id,
        agent.name,
        agent.adapter_type.as_deref().unwrap_or("unknown"),
    );

    let body = format!(
        "{}\n\n## Objective\n\n{}\n\n## Acceptance Criteria\n\n{}\n\n## Recent Concrete Actions\n\n{}\n\n## Files / Routes Touched\n\n{}\n\n## Commands Run\n\n{}\n\n## Blockers / Decisions\n\n{}\n\n## Next Action\n\n- {}",
        header,
        truncate_text(&objective, SUMMARY_SECTION_MAX_CHARS),
        acceptance_criteria,
        bullet_list(&recent_actions, "No recent actions captured."),
        bullet_list(
            &paths.iter().map(|p| format!("`{}`", p)).collect::<Vec<_>>(),
            "No file or route paths were detected in the captured run summary.",
        ),
        bullet_list(
            &[
                format!(
                    "Heartbeat run `{}` invoked adapter `{}`.",
                    run.id,
                    agent.adapter_type.as_deref().unwrap_or("unknown")
                ),
                "Detailed shell/tool commands remain in the run log and transcript.".to_string(),
            ],
            "No command metadata captured.",
        ),
        bullet_list(
            &run.error.as_deref().map(|_| {
                vec![format!(
                    "Latest run ended with `{}`; inspect the error before continuing.",
                    run.status
                )]
            }).unwrap_or_else(|| {
                vec!["No new blocker was recorded by the latest run.".to_string()]
            }),
            "No blockers or decisions captured.",
        ),
        next_action,
    );

    truncate_text(&body, ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ----- trivial helpers -----

    #[test]
    fn truncate_text_short_returns_as_is() {
        assert_eq!(truncate_text("hello", 10), "hello");
    }

    #[test]
    fn truncate_text_long_truncates_with_marker() {
        let long = "x".repeat(100);
        let out = truncate_text(&long, 20);
        assert!(out.ends_with("[truncated]"));
        assert!(out.chars().count() <= 20);
    }

    #[test]
    fn truncate_text_strips_whitespace() {
        assert_eq!(truncate_text("   hello   ", 100), "hello");
    }

    #[test]
    fn as_non_empty_string_filters_empty() {
        assert_eq!(as_non_empty_string(""), None);
        assert_eq!(as_non_empty_string("   "), None);
        assert_eq!(as_non_empty_string("  x  "), Some("x".to_string()));
    }

    #[test]
    fn read_result_summary_prefers_summary_field() {
        let v = serde_json::json!({"summary": "A", "result": "B"});
        assert_eq!(read_result_summary(Some(&v)), Some("A".to_string()));
    }

    #[test]
    fn read_result_summary_falls_back_through_fields() {
        assert_eq!(
            read_result_summary(Some(&serde_json::json!({"result": "R"}))),
            Some("R".to_string())
        );
        assert_eq!(
            read_result_summary(Some(&serde_json::json!({"message": "M"}))),
            Some("M".to_string())
        );
        assert_eq!(
            read_result_summary(Some(&serde_json::json!({"error": "E"}))),
            Some("E".to_string())
        );
    }

    #[test]
    fn read_result_summary_returns_none_for_empty_or_missing() {
        assert_eq!(read_result_summary(None), None);
        assert_eq!(read_result_summary(Some(&serde_json::json!({}))), None);
        assert_eq!(
            read_result_summary(Some(&serde_json::json!({"summary": ""}))),
            None
        );
    }

    #[test]
    fn extract_markdown_section_parses_heading() {
        let md = "# Title\n## Objective\nThis is the objective.\n## Other\nfoo";
        let sec = extract_markdown_section(Some(md), "Objective").unwrap();
        assert!(sec.contains("This is the objective"));
    }
    #[test]
    fn extract_markdown_section_missing_returns_none() {
        let md = "# Title\nNo sections here";
        assert_eq!(extract_markdown_section(Some(md), "Objective"), None);
    }

    #[test]
    fn extract_markdown_section_null_input_returns_none() {
        assert_eq!(extract_markdown_section(None, "Objective"), None);
    }

    // ----- path candidates -----

    #[test]
    fn extract_path_candidates_basic() {
        let paths = extract_path_candidates(&[Some("see server/src/foo.ts for details")]);
        assert!(paths.contains(&"server/src/foo.ts".to_string()));
    }

    #[test]
    fn extract_path_candidates_dedup() {
        let paths = extract_path_candidates(&[Some("server/src/foo.ts and server/src/foo.ts")]);
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn extract_path_candidates_strips_trailing_punct() {
        let paths = extract_path_candidates(&[Some("check ui/app.tsx).")]);
        assert!(paths.contains(&"ui/app.tsx".to_string()));
    }

    #[test]
    fn extract_path_candidates_caps_at_twelve() {
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&format!(" server/src/file{}.ts ", i));
        }
        let paths = extract_path_candidates(&[Some(&text)]);
        assert!(paths.len() <= 12);
    }

    #[test]
    fn extract_path_candidates_ignores_non_matching() {
        let paths = extract_path_candidates(&[Some("random text without paths")]);
        assert!(paths.is_empty());
    }

    // ----- infer_mode -----

    fn issue(status: &str) -> IssueSummaryInput {
        IssueSummaryInput {
            id: "i".into(),
            identifier: None,
            title: "t".into(),
            description: None,
            status: status.into(),
            priority: "p".into(),
        }
    }

    fn run(status: &str) -> RunSummaryInput {
        RunSummaryInput {
            id: "r".into(),
            status: status.into(),
            error: None,
            error_code: None,
            result_json: None,
            stdout_excerpt: None,
            stderr_excerpt: None,
            finished_at: None,
        }
    }

    #[test]
    fn infer_mode_done_or_in_review_returns_review() {
        assert_eq!(infer_mode(&issue("done"), &run("queued")), SummaryMode::Review);
        assert_eq!(infer_mode(&issue("in_review"), &run("queued")), SummaryMode::Review);
    }

    #[test]
    fn infer_mode_failed_runs_return_implementation() {
        for s in ["failed", "timed_out", "cancelled", "interrupted"] {
            assert_eq!(
                infer_mode(&issue("todo"), &run(s)),
                SummaryMode::Implementation,
                "run.status={s}"
            );
        }
    }

    #[test]
    fn infer_mode_backlog_or_todo_returns_plan() {
        assert_eq!(infer_mode(&issue("backlog"), &run("queued")), SummaryMode::Plan);
        assert_eq!(infer_mode(&issue("todo"), &run("queued")), SummaryMode::Plan);
    }

    #[test]
    fn infer_mode_default_implementation() {
        assert_eq!(infer_mode(&issue("in_progress"), &run("queued")), SummaryMode::Implementation);
    }

    // ----- infer_next_action -----

    #[test]
    fn infer_next_action_done_suggests_review() {
        let act = infer_next_action(&issue("done"), &run("succeeded"), None);
        assert!(act.to_lowercase().contains("review"));
    }

    #[test]
    fn infer_next_action_failed_suggests_inspect() {
        let act = infer_next_action(&issue("todo"), &run("failed"), None);
        assert!(act.to_lowercase().contains("inspect"));
    }

    #[test]
    fn infer_next_action_falls_back_to_previous() {
        let act = infer_next_action(&issue("in_progress"), &run("succeeded"), Some("Continue from X"));
        assert_eq!(act, "Continue from X");
    }

    #[test]
    fn infer_next_action_default_resume() {
        let act = infer_next_action(&issue("in_progress"), &run("succeeded"), None);
        assert!(act.to_lowercase().contains("resume"));
    }

    // ----- bullet_list -----

    #[test]
    fn bullet_list_empty_uses_empty_marker() {
        assert_eq!(bullet_list(&[], "nothing here"), "- nothing here");
    }

    #[test]
    fn bullet_list_items_each_on_line() {
        let items = vec!["a".to_string(), "b".to_string()];
        let out = bullet_list(&items, "x");
        assert_eq!(out, "- a\n- b");
    }

    // ----- extract_previous_next_action -----

    #[test]
    fn extract_previous_next_action_basic() {
        let body = "## Next Action\n- Continue from step 5.\n\n## Other";
        let act = extract_previous_next_action(Some(body)).unwrap();
        assert_eq!(act, "Continue from step 5.");
    }

    #[test]
    fn extract_previous_next_action_strips_bullet() {
        let body = "## Next Action\n* Step one\n";
        let act = extract_previous_next_action(Some(body)).unwrap();
        assert_eq!(act, "Step one");
    }

    #[test]
    fn extract_previous_next_action_missing_returns_none() {
        assert_eq!(extract_previous_next_action(Some("## Other\nfoo")), None);
        assert_eq!(extract_previous_next_action(None), None);
    }

    // ----- parks_executor -----

    #[test]
    fn parks_executor_detects_waiting_for_review() {
        let body = "## Next Action\n- Wait for reviewer feedback before continuing.";
        assert!(continuation_summary_parks_executor(Some(body)));
    }

    #[test]
    fn parks_executor_detects_waiting_for_approval() {
        let body = "## Next Action\n- Waiting for board approval.";
        assert!(continuation_summary_parks_executor(Some(body)));
    }

    #[test]
    fn parks_executor_false_for_normal_next_action() {
        let body = "## Next Action\n- Implement the feature now.";
        assert!(!continuation_summary_parks_executor(Some(body)));
    }

    #[test]
    fn parks_executor_false_for_missing_next_action() {
        assert!(!continuation_summary_parks_executor(Some("## Objective\nfoo")));
        assert!(!continuation_summary_parks_executor(None));
    }

    // ----- build_continuation_summary_markdown -----

    fn agent() -> AgentSummaryInput {
        AgentSummaryInput {
            id: "a-1".into(),
            name: "Test Agent".into(),
            adapter_type: Some("process".into()),
        }
    }

    #[test]
    fn build_summary_contains_all_sections() {
        let issue = IssueSummaryInput {
            id: "i-1".into(),
            identifier: Some("PAP-1".into()),
            title: "Add login".into(),
            description: Some("## Objective\nImplement OAuth login.\n## Acceptance Criteria\n- Works with Google\n".into()),
            status: "in_progress".into(),
            priority: "high".into(),
        };
        let run = RunSummaryInput {
            id: "r-1".into(),
            status: "succeeded".into(),
            error: None,
            error_code: None,
            result_json: Some(serde_json::json!({"summary": "Login flow completed"})),
            stdout_excerpt: Some("running server/src/auth.ts".into()),
            stderr_excerpt: None,
            finished_at: None,
        };
        let input = BuildSummaryInput {
            issue,
            run,
            agent: agent(),
            previous_summary_body: None,
        };
        let md = build_continuation_summary_markdown(&input);
        assert!(md.contains("# Continuation Summary"));
        assert!(md.contains("## Objective"));
        assert!(md.contains("## Acceptance Criteria"));
        assert!(md.contains("## Recent Concrete Actions"));
        assert!(md.contains("## Files / Routes Touched"));
        assert!(md.contains("## Commands Run"));
        assert!(md.contains("## Blockers / Decisions"));
        assert!(md.contains("## Next Action"));
        assert!(md.contains("server/src/auth.ts"));
        assert!(md.contains("Implement OAuth login"));
    }

    #[test]
    fn build_summary_truncates_to_max_body_chars() {
        let mut description = String::from("## Objective\n");
        description.push_str(&"x".repeat(ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS * 2));
        let issue = IssueSummaryInput {
            id: "i".into(),
            identifier: None,
            title: "t".into(),
            description: Some(description),
            status: "todo".into(),
            priority: "low".into(),
        };
        let run = run("queued");
        let input = BuildSummaryInput {
            issue,
            run,
            agent: agent(),
            previous_summary_body: None,
        };
        let md = build_continuation_summary_markdown(&input);
        assert!(md.chars().count() <= ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS);
    }

    #[test]
    fn build_summary_includes_run_error_in_actions() {
        let issue = issue("in_progress");
        let mut r = run("failed");
        r.error = Some("something broke".into());
        r.error_code = Some("E_FAIL".into());
        let input = BuildSummaryInput {
            issue,
            run: r,
            agent: agent(),
            previous_summary_body: None,
        };
        let md = build_continuation_summary_markdown(&input);
        assert!(md.contains("something broke"));
        assert!(md.contains("E_FAIL"));
    }

    #[test]
    fn build_summary_uses_previous_next_action_when_present() {
        let issue = issue("in_progress");
        let run = run("succeeded");
        let input = BuildSummaryInput {
            issue,
            run,
            agent: agent(),
            previous_summary_body: Some(
                "## Next Action\n- Continue from previous step.\n".to_string(),
            ),
        };
        let md = build_continuation_summary_markdown(&input);
        assert!(md.contains("Continue from previous step."));
    }
}
