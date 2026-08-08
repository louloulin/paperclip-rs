//! R380 集成测试 — `pc-acpx` `prompt_compose` 模块的端到端组合。
//!
//! 范围:把 R380 暴露的 5 个纯函数 + `session_config_options` 里两个
//! 渲染函数(`render_paperclip_env_note` / `render_api_access_note`)组合
//! 成一个 `build_prompt_preview`,严格镜像 Node `buildPrompt` 7 段布局
//! (在 `acpx-engine/execute.ts` L2246)。Wake prompt 渲染 (`renderPaperclip
//! WakePrompt`) port 到 R381,现在使用真实的 `render_paperclip_wake_prompt`
//! 占位,只验证 R380 暴露的 4 个 prompt 段的真实端到端行为。
//!
//! 覆盖场景:
//! - Fresh session + issue_assigned wake → 全 7 段, taskContext=full,
//!   wake 注入, prompt template 完整渲染
//! - Resumed session + issue_assigned wake → wake 替代 prompt template,
//!   taskContext=full, instructions prefix 被空字符串覆盖
//! - Resumed session + issue_commented wake → wake 替代 prompt template,
//!   taskContext=compact
//! - Resumed session + recovery wake → wake 替代 prompt template,
//!   taskContext=full
//! - taskContext 缺失 → 该段被 join_prompt_sections 过滤
//! - Malformed template → render_template 保留 `{{var}}` 原样
//! - `ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS` 4 项已知
//! - 字符数 metrics (与 Node buildPrompt 命名一致) 全部对得上

use std::collections::{BTreeMap, HashMap};

use pc_acpx::{
    is_assignment_shaped_paperclip_wake_reason, is_paperclip_recovery_wake_payload,
    join_prompt_sections, render_api_access_note, render_paperclip_env_note,
    render_paperclip_wake_prompt, render_template, select_paperclip_task_markdown,
    RenderWakePromptOptions, SelectTaskMarkdownOptions, ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS,
};
use serde_json::{json, Value};

// =============================================================================
// BuildPromptPreview — 严格镜像 Node `buildPrompt` 7 段布局,只调用已
// 暴露的纯函数,无 I/O,无 mock 运行时。
// =============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BuildPromptPreviewMetrics {
    instructions_chars: usize,
    bootstrap_prompt_chars: usize,
    wake_prompt_chars: usize,
    session_handoff_chars: usize,
    task_context_chars: usize,
    runtime_note_chars: usize,
    heartbeat_prompt_chars: usize,
    prompt_chars: usize,
}

#[derive(Debug, Clone, Default)]
struct BuildPromptPreviewOutput {
    prompt: String,
    metrics: BuildPromptPreviewMetrics,
}

#[allow(clippy::too_many_arguments)]
fn build_prompt_preview(
    prompt_template: &str,
    bootstrap_prompt_template: &str,
    template_data: &Value,
    context: &Value,
    env: &BTreeMap<String, String>,
    resumed_session: bool,
    instructions_prefix: &str,
) -> BuildPromptPreviewOutput {
    let rendered_bootstrap = if !resumed_session && !bootstrap_prompt_template.trim().is_empty() {
        render_template(bootstrap_prompt_template, template_data)
            .trim()
            .to_string()
    } else {
        String::new()
    };
    let task_context_note = select_paperclip_task_markdown(
        Some(context),
        SelectTaskMarkdownOptions { resumed_session },
    );
    let wake_prompt = render_paperclip_wake_prompt(
        context.get("paperclipWake"),
        &RenderWakePromptOptions {
            resumed_session,
            include_execution_contract: true,
            suppress_issue_description: false,
        },
    );
    let should_use_resume_delta_prompt = resumed_session && !wake_prompt.is_empty();
    let prompt_instructions_prefix = if should_use_resume_delta_prompt {
        ""
    } else {
        instructions_prefix
    };
    let rendered_prompt = if should_use_resume_delta_prompt {
        String::new()
    } else {
        render_template(prompt_template, template_data)
    };
    let session_handoff_note = context
        .get("paperclipSessionHandoffMarkdown")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let paperclip_env_note = render_paperclip_env_note(env);
    let api_access_note = render_api_access_note(env);

    let prompt = join_prompt_sections(&[
        Some(prompt_instructions_prefix),
        Some(rendered_bootstrap.as_str()),
        Some(wake_prompt.as_str()),
        Some(session_handoff_note.as_str()),
        Some(task_context_note.as_str()),
        Some(paperclip_env_note.as_str()),
        Some(api_access_note.as_str()),
        Some(rendered_prompt.as_str()),
    ]);

    BuildPromptPreviewOutput {
        metrics: BuildPromptPreviewMetrics {
            instructions_chars: prompt_instructions_prefix.len(),
            bootstrap_prompt_chars: rendered_bootstrap.len(),
            wake_prompt_chars: wake_prompt.len(),
            session_handoff_chars: session_handoff_note.len(),
            task_context_chars: task_context_note.len(),
            runtime_note_chars: paperclip_env_note.len() + api_access_note.len(),
            heartbeat_prompt_chars: rendered_prompt.len(),
            prompt_chars: prompt.len(),
        },
        prompt,
    }
}

fn fixture_template_data() -> Value {
    json!({
        "agentId": "claude",
        "companyId": "co_pcx",
        "runId": "run_pcx_001",
        "company": { "id": "co_pcx" },
        "agent": { "id": "claude", "companyId": "co_pcx" },
        "run": { "id": "run_pcx_001", "source": "on_demand" },
    })
}

fn fixture_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        "PAPERCLIP_API_URL".to_string(),
        "https://api.paperclip.local".to_string(),
    );
    env.insert("PAPERCLIP_API_TOKEN".to_string(), "tok_pcx_xyz".to_string());
    env
}

// =============================================================================
// Tests
// =============================================================================

#[test]
fn fresh_session_with_assignment_wake_includes_all_seven_sections() {
    let template_data = fixture_template_data();
    let context = json!({
        "paperclipTaskMarkdown": "FULL_BRIEF",
        "paperclipTaskMarkdownCompact": "COMPACT_BRIEF",
        "paperclipSessionHandoffMarkdown": "HANDOFF_NOTE",
        "paperclipWake": { "reason": "issue_assigned", "comments": [] },
    });
    let env = fixture_env();
    let prompt_template = "AGENT_PROMPT for {{agentId}} run={{runId}}";
    let bootstrap = "BOOTSTRAP for {{company.id}}";

    let out = build_prompt_preview(
        prompt_template,
        bootstrap,
        &template_data,
        &context,
        &env,
        false,
        "INSTRUCTIONS_PREFIX",
    );

    // Fresh + assignment wake → wake_prompt gets injected (real
    // render_paperclip_wake_prompt emits the full body for this shape).
    assert!(
        out.prompt.contains("INSTRUCTIONS_PREFIX"),
        "instructions must lead"
    );
    assert!(
        out.prompt.contains("BOOTSTRAP for co_pcx"),
        "bootstrap rendered"
    );
    assert!(
        out.prompt.contains("- reason: issue_assigned"),
        "wake prompt has the reason line"
    );
    assert!(
        out.prompt.contains("HANDOFF_NOTE"),
        "session handoff included"
    );
    assert!(
        out.prompt.contains("FULL_BRIEF"),
        "fresh session picks FULL taskContext"
    );
    assert!(
        !out.prompt.contains("COMPACT_BRIEF"),
        "fresh session does NOT pick compact"
    );
    assert!(out
        .prompt
        .contains("AGENT_PROMPT for claude run=run_pcx_001"));
    assert!(out.metrics.heartbeat_prompt_chars > 0);
    assert!(out.metrics.wake_prompt_chars > 0);
    assert_eq!(out.metrics.task_context_chars, "FULL_BRIEF".len());
    // total chars equal prompt length
    assert_eq!(out.metrics.prompt_chars, out.prompt.len());
}

#[test]
fn resumed_session_with_assignment_wake_replaces_prompt_template() {
    let template_data = fixture_template_data();
    let context = json!({
        "paperclipTaskMarkdown": "FULL_BRIEF",
        "paperclipTaskMarkdownCompact": "COMPACT_BRIEF",
        "paperclipWake": { "reason": "issue_tree_restored", "comments": [] },
    });
    let env = fixture_env();

    let out = build_prompt_preview(
        "AGENT_PROMPT for {{agentId}}",
        "BOOTSTRAP for {{company.id}}",
        &template_data,
        &context,
        &env,
        true, // resumed
        "INSTRUCTIONS_PREFIX",
    );

    // shouldUseResumeDeltaPrompt=true → heartbeat template omitted AND
    // instructions prefix is dropped.
    assert!(
        !out.prompt.contains("INSTRUCTIONS_PREFIX"),
        "instructions prefix cleared when resume delta prompt wins"
    );
    assert!(
        !out.prompt.contains("AGENT_PROMPT for claude"),
        "heartbeat template not rendered on resume delta"
    );
    assert!(
        !out.prompt.contains("BOOTSTRAP for co_pcx"),
        "bootstrap suppressed on resumed session"
    );
    assert!(out.prompt.contains("- reason: issue_tree_restored"));
    assert!(
        out.prompt.contains("FULL_BRIEF"),
        "assignment-shaped resumed session picks FULL taskContext"
    );
    assert!(!out.prompt.contains("COMPACT_BRIEF"));
    assert_eq!(out.metrics.heartbeat_prompt_chars, 0);
    assert_eq!(out.metrics.bootstrap_prompt_chars, 0);
    assert_eq!(out.metrics.instructions_chars, 0);
}

#[test]
fn resumed_session_with_non_assignment_wake_picks_compact_task_context() {
    let template_data = fixture_template_data();
    let context = json!({
        "paperclipTaskMarkdown": "FULL_BRIEF",
        "paperclipTaskMarkdownCompact": "COMPACT_BRIEF",
        "paperclipWake": { "reason": "issue_commented", "comments": [] },
    });
    let env = fixture_env();

    let out = build_prompt_preview(
        "AGENT_PROMPT for {{agentId}}",
        "BOOTSTRAP for {{company.id}}",
        &template_data,
        &context,
        &env,
        true,
        "INSTRUCTIONS_PREFIX",
    );

    assert!(out.prompt.contains("- reason: issue_commented"));
    assert!(
        !out.prompt.contains("FULL_BRIEF"),
        "non-assignment resumed session skips FULL taskContext"
    );
    assert!(
        out.prompt.contains("COMPACT_BRIEF"),
        "non-assignment resumed session picks COMPACT taskContext"
    );
    assert!(!out.prompt.contains("AGENT_PROMPT for claude"));
}

#[test]
fn resumed_session_with_recovery_wake_picks_full_task_context() {
    let template_data = fixture_template_data();
    let context = json!({
        "paperclipTaskMarkdown": "FULL_BRIEF",
        "paperclipTaskMarkdownCompact": "COMPACT_BRIEF",
        "paperclipWake": {
            "reason": "issue_monitor_recovery",
            "recovery": { "cause": "process_lost" },
        },
    });
    let env = fixture_env();

    let out = build_prompt_preview(
        "AGENT_PROMPT for {{agentId}}",
        "BOOTSTRAP for {{company.id}}",
        &template_data,
        &context,
        &env,
        true,
        "INSTRUCTIONS_PREFIX",
    );

    assert!(out.prompt.contains("- reason: issue_monitor_recovery"));
    assert!(
        out.prompt.contains("FULL_BRIEF"),
        "recovery-shaped wake should pick FULL taskContext"
    );
    assert!(!out.prompt.contains("COMPACT_BRIEF"));
}

#[test]
fn missing_task_context_is_filtered_by_join_sections() {
    let template_data = fixture_template_data();
    let context = json!({
        // No paperclipTaskMarkdown at all
        "paperclipWake": { "reason": "issue_assigned" },
    });
    let env = fixture_env();

    let out = build_prompt_preview(
        "AGENT_PROMPT for {{agentId}}",
        "",
        &template_data,
        &context,
        &env,
        false,
        "INSTRUCTIONS_PREFIX",
    );

    assert_eq!(out.metrics.task_context_chars, 0);
    assert!(
        !out.prompt.contains("\n\n\n"),
        "missing taskContext should not produce a triple-newline gap"
    );
    // The rest of the pipeline still works.
    assert!(out.prompt.contains("INSTRUCTIONS_PREFIX"));
    assert!(out.prompt.contains("AGENT_PROMPT for claude"));
}

#[test]
fn malformed_template_keeps_placeholder_verbatim() {
    let template_data = fixture_template_data();
    let context = json!({
        "paperclipWake": { "reason": "issue_assigned" },
    });
    let env = fixture_env();

    let out = build_prompt_preview(
        "ok={{agentId}} broken={{not a path}} also={{ok}}",
        "",
        &template_data,
        &context,
        &env,
        false,
        "",
    );

    assert!(out.prompt.contains("ok=claude"));
    assert!(
        out.prompt.contains("{{not a path}}"),
        "malformed placeholder (spaces not in path alphabet) is preserved verbatim"
    );
    assert!(
        out.prompt.contains("also="),
        "missing key resolves to empty string (Node renderTemplate parity)"
    );
    assert!(
        !out.prompt.contains("also={{ok}}"),
        "missing key should NOT be preserved verbatim — it should resolve to empty"
    );
}

#[test]
fn assignment_shaped_reasons_constant_lists_all_four_node_values() {
    // Mirrors Node `ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS` exactly.
    assert_eq!(
        ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS,
        &[
            "issue_assigned",
            "issue_reopened_via_comment",
            "issue_recovery_action_restored",
            "issue_tree_restored",
        ],
    );
}

#[test]
fn wake_reason_helpers_match_node_behavior_on_real_json() {
    // Real shapes that Node adapters pass through to `paperclipWake`.
    let assigned = json!({ "reason": "issue_assigned", "comments": [] });
    let reopened = json!({ "reason": "issue_reopened_via_comment" });
    let tree_restored = json!({ "reason": "issue_tree_restored" });
    let recovery_action = json!({ "reason": "issue_recovery_action_restored" });
    let commented = json!({ "reason": "issue_commented" });
    let monitor_recovery = json!({
        "reason": "issue_monitor_recovery",
        "recovery": { "cause": "process_lost" },
    });
    let source_scoped = json!({ "reason": "source_scoped_recovery_action" });

    for assigned_value in [&assigned, &reopened, &tree_restored, &recovery_action] {
        let reason = assigned_value.get("reason").and_then(|v| v.as_str());
        assert!(
            is_assignment_shaped_paperclip_wake_reason(reason),
            "expected assignment-shaped for reason={:?}",
            reason,
        );
    }

    assert!(!is_assignment_shaped_paperclip_wake_reason(
        commented.get("reason").and_then(|v| v.as_str()),
    ));
    assert!(!is_assignment_shaped_paperclip_wake_reason(None));
    assert!(!is_assignment_shaped_paperclip_wake_reason(Some("")));

    // Recovery detection
    assert!(is_paperclip_recovery_wake_payload(Some(&monitor_recovery)));
    assert!(is_paperclip_recovery_wake_payload(Some(&source_scoped)));
    assert!(!is_paperclip_recovery_wake_payload(Some(&assigned)));
    assert!(!is_paperclip_recovery_wake_payload(Some(&commented)));
    assert!(!is_paperclip_recovery_wake_payload(None));
}

#[test]
fn template_data_dotted_paths_resolve_through_nested_objects() {
    let template_data = json!({
        "agentId": "codex",
        "company": { "id": "co_nested" },
        "run": { "id": "run_nested_42", "source": "schedule" },
    });
    let rendered = render_template(
        "agent={{agentId}} co={{company.id}} run={{run.id}} src={{run.source}}",
        &template_data,
    );
    assert_eq!(
        rendered,
        "agent=codex co=co_nested run=run_nested_42 src=schedule"
    );
}

#[test]
fn render_template_coerces_booleans_and_numbers() {
    let data = json!({ "flag": true, "count": 7, "missing": null });
    let rendered = render_template(
        "flag={{flag}} count={{count}} miss={{missing}} gone={{absent}}",
        &data,
    );
    assert_eq!(rendered, "flag=true count=7 miss= gone=");
}

#[test]
fn join_sections_dedupes_repeated_segments() {
    let joined = join_prompt_sections(&[Some("ALPHA"), Some("ALPHA"), Some("BETA")]);
    assert_eq!(joined, "ALPHA\n\nBETA");
}

#[test]
fn build_prompt_metrics_match_node_field_naming() {
    // Node buildPrompt returns promptMetrics with these exact keys; the
    // Rust preview must report identical characters for each.
    let template_data = fixture_template_data();
    let context = json!({
        "paperclipTaskMarkdown": "BRIEF_42",
        "paperclipTaskMarkdownCompact": "BRIEF_42C",
        "paperclipSessionHandoffMarkdown": "HANDOFF_9CHARS",
        "paperclipWake": { "reason": "issue_commented" },
    });
    let env = fixture_env();

    let out = build_prompt_preview(
        "P-{{agentId}}",
        "",
        &template_data,
        &context,
        &env,
        false,
        "I-",
    );

    assert_eq!(out.metrics.task_context_chars, "BRIEF_42".len());
    assert_eq!(out.metrics.session_handoff_chars, "HANDOFF_9CHARS".len());
    assert!(
        out.metrics.runtime_note_chars > 0,
        "runtime note = paperclipEnvNote + apiAccessNote should not be empty when env is populated"
    );
    assert_eq!(out.metrics.instructions_chars, "I-".len());
    assert_eq!(out.metrics.heartbeat_prompt_chars, "P-claude".len());
    assert!(out.metrics.wake_prompt_chars > 0);
    assert_eq!(out.metrics.prompt_chars, out.prompt.len());
}
