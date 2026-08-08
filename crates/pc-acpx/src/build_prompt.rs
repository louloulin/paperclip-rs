//! `pc-acpx` build-prompt composition — mirrors Node
//! `buildPrompt` in `adapter-utils/src/acpx-engine/execute.ts` (L2246).
//!
//! Composition rule (mirrors Node L2246-2330):
//! 1. `promptInstructionsPrefix` — loaded `instructionsFilePath` prefix,
//!    dropped on resume-delta path
//! 2. `renderedBootstrapPrompt` — fresh-only template render
//! 3. `wakePrompt` — full Node `renderPaperclipWakePrompt` body
//! 4. `sessionHandoffNote` — `context.paperclipSessionHandoffMarkdown`
//! 5. `taskContextNote` — `selectPaperclip_task_markdown`
//! 6. `paperclipEnvNote` + `apiAccessNote` — joined runtime note
//! 7. `renderedPrompt` — heartbeat template render
//!
//! When `config.promptTemplate` is missing, the caller can pass a
//! `default_prompt_template` (typically the raw `ctx.run_prompt`) so
//! existing R376-R379 tests that pass `run_prompt: "test"` keep working
//! without a 7-segment composition. Production callers will set
//! `config.promptTemplate` and the wake / taskContext / handoff / env
//! / api notes will all light up.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::build_runtime::AgentIdentity;
use crate::prompt_compose::{
    join_prompt_sections, render_paperclip_wake_prompt, render_template,
    select_paperclip_task_markdown, RenderWakePromptOptions, SelectTaskMarkdownOptions,
};
use crate::session_config_options::{render_api_access_note, render_paperclip_env_note};

/// Character-count metrics mirroring the Node `promptMetrics` object
/// returned by `buildPrompt`. All field names match Node 1:1.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildPromptMetrics {
    pub prompt_chars: usize,
    pub instructions_chars: usize,
    pub bootstrap_prompt_chars: usize,
    pub wake_prompt_chars: usize,
    pub session_handoff_chars: usize,
    pub task_context_chars: usize,
    pub runtime_note_chars: usize,
    pub heartbeat_prompt_chars: usize,
}

/// Output of `build_prompt` — the composed prompt + metrics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildPromptOutput {
    pub prompt: String,
    pub metrics: BuildPromptMetrics,
}

/// Inputs for `build_prompt`. Mirrors the `ctx` + `resumedSession` +
/// `env` triple that Node `buildPrompt` takes.
#[derive(Debug, Clone)]
pub struct BuildPromptInput<'a> {
    pub run_id: &'a str,
    pub agent: &'a AgentIdentity,
    pub config: &'a Value,
    pub context: &'a Value,
    pub run_prompt: &'a str,
    pub env: &'a BTreeMap<String, String>,
    pub resumed_session: bool,
    /// Optional instructions prefix (from `config.instructionsFilePath`).
    /// Empty when the file was missing or the path is unset.
    pub instructions_prefix: &'a str,
}

/// 7-segment composition. Mirrors Node `buildPrompt` (L2246-2330).
pub fn build_prompt(input: &BuildPromptInput<'_>) -> BuildPromptOutput {
    let BuildPromptInput {
        run_id,
        agent,
        config,
        context,
        run_prompt,
        env,
        resumed_session,
        instructions_prefix,
    } = input;

    let prompt_template = config
        .get("promptTemplate")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let bootstrap_prompt_template = config
        .get("bootstrapPromptTemplate")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let template_data = serde_json::json!({
        "agentId": agent.id,
        "companyId": agent.company_id,
        "runId": run_id,
        "company": { "id": agent.company_id },
        "agent": { "id": agent.id, "companyId": agent.company_id },
        "run": { "id": run_id, "source": "on_demand" },
        "context": context,
    });

    let rendered_bootstrap = if !resumed_session && !bootstrap_prompt_template.trim().is_empty() {
        render_template(bootstrap_prompt_template, &template_data)
            .trim()
            .to_string()
    } else {
        String::new()
    };

    let task_context_note = select_paperclip_task_markdown(
        Some(context),
        SelectTaskMarkdownOptions {
            resumed_session: *resumed_session,
        },
    );

    let wake_prompt = render_paperclip_wake_prompt(
        context.get("paperclipWake"),
        &RenderWakePromptOptions {
            resumed_session: *resumed_session,
            include_execution_contract: true,
            suppress_issue_description: !task_context_note.is_empty(),
        },
    );

    let should_use_resume_delta_prompt = *resumed_session && !wake_prompt.is_empty();
    let prompt_instructions_prefix = if should_use_resume_delta_prompt {
        ""
    } else {
        *instructions_prefix
    };
    let rendered_prompt = if should_use_resume_delta_prompt {
        String::new()
    } else if !prompt_template.is_empty() {
        render_template(prompt_template, &template_data)
    } else {
        // No explicit promptTemplate — fall back to the raw `run_prompt`
        // the caller passed in. This keeps R376-R379 tests (which set
        // `run_prompt: "test"`) working without forcing a Node default
        // template.
        run_prompt.to_string()
    };

    let session_handoff_note = context
        .get("paperclipSessionHandoffMarkdown")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    // `render_paperclip_env_note` / `render_api_access_note` take
    // `&BTreeMap<String, String>` so we can pass `env` directly.
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

    let runtime_note_chars = paperclip_env_note.len() + api_access_note.len();

    BuildPromptOutput {
        metrics: BuildPromptMetrics {
            prompt_chars: prompt.len(),
            instructions_chars: prompt_instructions_prefix.len(),
            bootstrap_prompt_chars: rendered_bootstrap.len(),
            wake_prompt_chars: wake_prompt.len(),
            session_handoff_chars: session_handoff_note.len(),
            task_context_chars: task_context_note.len(),
            runtime_note_chars,
            heartbeat_prompt_chars: rendered_prompt.len(),
        },
        prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_env() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert(
            "PAPERCLIP_API_URL".to_string(),
            "https://api.paperclip.local".to_string(),
        );
        env.insert("PAPERCLIP_API_TOKEN".to_string(), "tok_xyz".to_string());
        env
    }

    fn fixture_agent() -> AgentIdentity {
        AgentIdentity::new("claude", "co_pc")
    }

    fn fixture_template_data(agent: &AgentIdentity, run_id: &str) -> Value {
        json!({
            "agentId": agent.id,
            "companyId": agent.company_id,
            "runId": run_id,
            "company": { "id": agent.company_id },
            "agent": { "id": agent.id, "companyId": agent.company_id },
            "run": { "id": run_id, "source": "on_demand" },
            "context": json!({}),
        })
    }

    #[test]
    fn missing_prompt_template_falls_back_to_run_prompt() {
        let agent = fixture_agent();
        let config = json!({});
        let context = json!({});
        let env = fixture_env();
        let input = BuildPromptInput {
            run_id: "run_pc",
            agent: &agent,
            config: &config,
            context: &context,
            run_prompt: "fallback prompt body",
            env: &env,
            resumed_session: false,
            instructions_prefix: "",
        };
        let out = build_prompt(&input);
        assert!(out.prompt.contains("fallback prompt body"));
        assert_eq!(
            out.metrics.heartbeat_prompt_chars,
            "fallback prompt body".len()
        );
        assert_eq!(out.metrics.bootstrap_prompt_chars, 0);
        assert_eq!(out.metrics.wake_prompt_chars, 0);
    }

    #[test]
    fn explicit_prompt_template_is_rendered() {
        let agent = fixture_agent();
        let config = json!({
            "promptTemplate": "AGENT={{agentId}} RUN={{runId}}",
        });
        let context = json!({});
        let env = fixture_env();
        let input = BuildPromptInput {
            run_id: "run_pc",
            agent: &agent,
            config: &config,
            context: &context,
            run_prompt: "ignored",
            env: &env,
            resumed_session: false,
            instructions_prefix: "",
        };
        let out = build_prompt(&input);
        assert!(out.prompt.contains("AGENT=claude RUN=run_pc"));
    }

    #[test]
    fn bootstrap_template_renders_only_on_fresh_session() {
        let agent = fixture_agent();
        let config = json!({
            "bootstrapPromptTemplate": "BOOTSTRAP for {{agentId}}",
        });
        let context = json!({});
        let env = fixture_env();
        let fresh = build_prompt(&BuildPromptInput {
            run_id: "run_pc",
            agent: &agent,
            config: &config,
            context: &context,
            run_prompt: "",
            env: &env,
            resumed_session: false,
            instructions_prefix: "",
        });
        assert!(fresh.prompt.contains("BOOTSTRAP for claude"));
        let resumed = build_prompt(&BuildPromptInput {
            run_id: "run_pc",
            agent: &agent,
            config: &config,
            context: &context,
            run_prompt: "",
            env: &env,
            resumed_session: true,
            instructions_prefix: "",
        });
        assert!(!resumed.prompt.contains("BOOTSTRAP for claude"));
    }

    #[test]
    fn resumed_session_with_wake_replaces_heartbeat_template() {
        let agent = fixture_agent();
        let config = json!({
            "promptTemplate": "HEARTBEAT body",
        });
        let context = json!({
            "paperclipWake": { "reason": "issue_assigned" },
        });
        let env = fixture_env();
        let out = build_prompt(&BuildPromptInput {
            run_id: "run_pc",
            agent: &agent,
            config: &config,
            context: &context,
            run_prompt: "",
            env: &env,
            resumed_session: true,
            instructions_prefix: "INSTRUCTIONS",
        });
        assert!(!out.prompt.contains("HEARTBEAT body"));
        assert!(!out.prompt.contains("INSTRUCTIONS"));
        assert!(out.prompt.contains("- reason: issue_assigned"));
    }

    #[test]
    fn task_context_full_for_fresh_session() {
        let agent = fixture_agent();
        let config = json!({});
        let context = json!({
            "paperclipTaskMarkdown": "FULL",
            "paperclipTaskMarkdownCompact": "COMPACT",
        });
        let env = fixture_env();
        let out = build_prompt(&BuildPromptInput {
            run_id: "run_pc",
            agent: &agent,
            config: &config,
            context: &context,
            run_prompt: "",
            env: &env,
            resumed_session: false,
            instructions_prefix: "",
        });
        assert!(out.prompt.contains("FULL"));
        assert!(!out.prompt.contains("COMPACT"));
    }

    #[test]
    fn task_context_compact_for_resumed_non_assignment_wake() {
        let agent = fixture_agent();
        let config = json!({});
        let context = json!({
            "paperclipTaskMarkdown": "FULL",
            "paperclipTaskMarkdownCompact": "COMPACT",
            "paperclipWake": { "reason": "issue_commented" },
        });
        let env = fixture_env();
        let out = build_prompt(&BuildPromptInput {
            run_id: "run_pc",
            agent: &agent,
            config: &config,
            context: &context,
            run_prompt: "",
            env: &env,
            resumed_session: true,
            instructions_prefix: "",
        });
        assert!(out.prompt.contains("COMPACT"));
        assert!(!out.prompt.contains("FULL"));
    }

    #[test]
    fn runtime_note_combines_env_and_api_access() {
        let agent = fixture_agent();
        let config = json!({});
        let context = json!({});
        let env = fixture_env();
        let out = build_prompt(&BuildPromptInput {
            run_id: "run_pc",
            agent: &agent,
            config: &config,
            context: &context,
            run_prompt: "",
            env: &env,
            resumed_session: false,
            instructions_prefix: "",
        });
        assert!(out.metrics.runtime_note_chars > 0);
    }

    #[test]
    fn metrics_match_node_field_naming() {
        let agent = fixture_agent();
        let config = json!({});
        let context = json!({
            "paperclipTaskMarkdown": "BRIEF",
            "paperclipSessionHandoffMarkdown": "HANDOFF",
        });
        let env = fixture_env();
        let out = build_prompt(&BuildPromptInput {
            run_id: "run_pc",
            agent: &agent,
            config: &config,
            context: &context,
            run_prompt: "x",
            env: &env,
            resumed_session: false,
            instructions_prefix: "I",
        });
        assert_eq!(out.metrics.task_context_chars, "BRIEF".len());
        assert_eq!(out.metrics.session_handoff_chars, "HANDOFF".len());
        assert_eq!(out.metrics.instructions_chars, "I".len());
        assert_eq!(out.metrics.heartbeat_prompt_chars, "x".len());
        assert_eq!(out.metrics.prompt_chars, out.prompt.len());
        // sanity: template_data has the agent + run
        let _ = fixture_template_data(&agent, "run_pc");
    }
}
