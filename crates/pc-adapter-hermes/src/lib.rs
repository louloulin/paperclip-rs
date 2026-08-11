//! Hermes adapter 完整实现（对齐 Node `packages/adapters/hermes/src/server/execute.ts`）。
//!
//! 模块拆分（高内聚、低耦合）：
//! - [`constants`]        — 共享常量（VALID_PROVIDERS / MODEL_PREFIX_* / 正则）
//! - [`config_schema`]    — Paperclip UI 配置 schema（12 字段）
//! - [`detect_model`]     — 解析 `~/.hermes/config.yaml`
//! - [`resolve_provider`] — explicit → detected → inferred → auto 优先级链
//! - [`command_args`]     — CLI args 拼装（含 `--source tool --yolo` 默认）
//!                           + stderr reclassification（benign stderr → stdout）
//! - [`parse_output`]     — 提取 session_id / usage / cost / response / errorMessage
//!
//! execute 路径：
//! 1. 从 `adapter_config` 提取 user override（command / model / provider / timeoutSec / ...）
//! 2. 调用 `detect_model` 读取 `~/.hermes/config.yaml`
//! 3. 用 `resolve_provider` 决定最终 provider
//! 4. 调用 `build_hermes_command_args` 拼装 CLI args（包含 prompt via -q）
//! 5. 通过 `pc_adapter_process::execute_process_capture` spawn 子进程
//! 6. 用 `parse_hermes_output` 提取结构化结果
//! 7. 用 `reclassify_stderr` 把良性 stderr 重分类为 stdout events
//! 8. 填充 `AdapterExecutionResult`（session_id / usage / cost / summary / result_json）

#![forbid(unsafe_code)]

pub mod command_args;
pub mod config_schema;
pub mod constants;
pub mod detect_model;
pub mod parse_output;
pub mod prompt_template;
pub mod resolve_provider;
pub mod skills;
pub mod wake_prompt;

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult,
};
use pc_adapter_process::{execute_process_capture, ProcessSpec};
use serde_json::Value;

pub use constants::{ADAPTER_LABEL, ADAPTER_TYPE};

/// 真实 Hermes adapter（对齐 Node `execute` 函数）。
pub struct HermesAdapter;

impl HermesAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for HermesAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for HermesAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin(ADAPTER_TYPE, ADAPTER_LABEL)
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        let config = &context.adapter_config;

        // 1. 解析用户 override
        let hermes_cmd = resolve_command(config);
        let model =
            cfg_string(config.get("model")).unwrap_or_else(|| constants::DEFAULT_MODEL.to_string());
        let timeout_sec =
            cfg_u64(config.get("timeoutSec")).unwrap_or(constants::DEFAULT_TIMEOUT_SEC);
        let grace_sec = cfg_u64(config.get("graceSec")).unwrap_or(constants::DEFAULT_GRACE_SEC);
        let max_turns = cfg_u32(config.get("maxTurnsPerRun"));
        let toolsets = cfg_string(config.get("toolsets"));
        let worktree_mode = cfg_bool(config.get("worktreeMode")).unwrap_or(false);
        let checkpoints = cfg_bool(config.get("checkpoints")).unwrap_or(false);
        let quiet = cfg_bool(config.get("quiet")).unwrap_or(true);
        let verbose = cfg_bool(config.get("verbose")).unwrap_or(false);
        let persist_session = cfg_bool(config.get("persistSession")).unwrap_or(true);
        let explicit_provider = cfg_string(config.get("provider"));
        let extra_args = cfg_string_array(config.get("extraArgs")).unwrap_or_default();

        // 2. 探测 ~/.hermes/config.yaml（用户 override 跳过探测）
        let detected = if explicit_provider.is_none() {
            detect_model::detect_model().await
        } else {
            None
        };

        // 3. resolve provider
        let resolved = resolve_provider::resolve_provider(resolve_provider::ResolveProviderInput {
            explicit_provider: explicit_provider.as_deref(),
            detected: detected.as_ref(),
            model: &model,
        });

        // 4. 拼装 CLI args
        let cmd_options = command_args::HermesCommandOptions {
            model: Some(model.clone()),
            provider: Some(resolved.provider.clone()),
            toolsets,
            max_turns,
            worktree_mode,
            checkpoints,
            quiet,
            verbose,
            source: Some("tool".to_string()),
            yolo: true,
            resume_session: extract_session_id_from_params(context.session_params.as_ref()),
            extra_args,
            persist_session,
            timeout_sec,
            grace_sec,
        };
        let (program, args) = command_args::build_hermes_command_args(
            Some(&hermes_cmd),
            &context.prompt,
            &cmd_options,
        );

        // 5. 执行
        let spec = ProcessSpec::new(&program, &args)
            .with_timeout(std::time::Duration::from_secs(timeout_sec));
        let execution = execute_process_capture(&spec, &context, events.clone()).await?;

        // 6. stderr reclassification — 把 benign stderr 行也 emit 成 stdout
        let (benign_stdout, _real_stderr) = command_args::reclassify_stderr(&execution.stderr);
        for line in &benign_stdout {
            let _ = events
                .clone()
                .emit(pc_adapter_api::AdapterEvent::stdout(format!("{line}\n")))
                .await;
        }

        // 7. 解析输出
        let parsed = parse_output::parse_hermes_output(&execution.stdout, &execution.stderr);

        // 8. 构造 AdapterExecutionResult
        let mut result = execution.result;
        result.provider = Some(resolved.provider.clone());
        result.model = Some(model.clone());
        if let Some(usage) = parsed.usage {
            result.usage = Some(usage);
        }
        if let Some(cost) = parsed.cost_usd {
            result.cost_usd = Some(cost);
        }
        if let Some(session_id) = parsed.session_id.clone() {
            result.session_id = Some(session_id.clone());
            if persist_session {
                result.session_params = Some(serde_json::json!({"sessionId": session_id}));
                let display = session_id.chars().take(16).collect::<String>();
                result.session_display_id = Some(display);
            }
        }
        if let Some(error) = parsed.error_message.clone() {
            result.error_message = Some(error);
        } else if result.exit_code != Some(0) {
            // 非零退出 + 没有 parsed error → 用 stderr 最后几行
            let tail: Vec<&str> = execution.stderr.lines().rev().take(5).collect();
            let tail: Vec<String> = tail.into_iter().rev().map(String::from).collect();
            if !tail.is_empty() {
                result.error_message = Some(tail.join("\n"));
            }
        }
        result.summary = parsed
            .response
            .clone()
            .map(|s| s.chars().take(2000).collect::<String>());
        result.result_json = Some(serde_json::json!({
            "result": parsed.response.unwrap_or_default(),
            "session_id": parsed.session_id,
            "usage": result.usage.as_ref().map(|u| serde_json::json!({
                "inputTokens": u.input_tokens,
                "outputTokens": u.output_tokens,
            })),
            "cost_usd": result.cost_usd,
            "provider": resolved.provider,
            "resolvedFrom": resolved.source.as_str(),
        }));

        Ok(result)
    }
}

/// 从 `adapter_config.command` 解析 Hermes 命令路径。
fn resolve_command(config: &Value) -> String {
    cfg_string(config.get("hermesCommand"))
        .or_else(|| cfg_string(config.get("command")))
        .unwrap_or_else(|| constants::HERMES_CLI.to_string())
}


/// 如果 `adapterConfig.promptTemplate` 配置存在，使用 `prompt_template` 模块
/// 渲染；否则原样返回 `context.prompt`。
///
/// 同时合并 wake prompt + task markdown（如果上下文提供），对齐 Node
/// `buildPrompt` 的核心契约。
pub fn render_full_prompt(
    context_prompt: &str,
    config: &Value,
    wake_payload: Option<&Value>,
    task_markdown: Option<&str>,
    session_handoff_markdown: Option<&str>,
) -> String {
    let template = config
        .get("promptTemplate")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s: &&str| !s.is_empty());

    let wake_prompt_str = wake_prompt::render_wake_prompt(wake_payload, false);

    let mut sections: Vec<Option<&str>> = Vec::new();
    if !wake_prompt_str.is_empty() {
        sections.push(Some(&wake_prompt_str));
    }
    if let Some(handoff) = session_handoff_markdown {
        let trimmed = handoff.trim();
        if !trimmed.is_empty() {
            sections.push(Some(trimmed));
        }
    }
    if let Some(task) = task_markdown {
        let trimmed = task.trim();
        if !trimmed.is_empty() {
            sections.push(Some(trimmed));
        }
    }

    let rendered = match template {
        Some(template_str) => {
            // 模板 → 条件段 → 变量替换
            let conditional = prompt_template::render_conditional_sections(template_str, &Value::Object(Default::default()));
            let data = build_template_data(config, context_prompt);
            prompt_template::render_template(&conditional, &data)
        }
        None => context_prompt.to_string(),
    };
    sections.push(Some(&rendered));
    prompt_template::join_prompt_sections(&sections, "\n\n")
}

fn build_template_data(config: &Value, prompt: &str) -> Value {
    let model = cfg_string(config.get("model")).unwrap_or_default();
    let provider = cfg_string(config.get("provider")).unwrap_or_default();
    serde_json::json!({
        "agentName": cfg_string(config.get("agentName")).unwrap_or_default(),
        "model": model,
        "provider": provider,
        "prompt": prompt,
        "taskTitle": "",
        "taskBody": "",
        "noTask": Value::Null,
    })
}

fn cfg_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn cfg_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|v| v.as_u64()).filter(|n| *n > 0)
}

fn cfg_u32(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
}

fn cfg_bool(value: Option<&Value>) -> Option<bool> {
    value.and_then(|v| v.as_bool())
}

fn cfg_string_array(value: Option<&Value>) -> Option<Vec<String>> {
    let arr = value.and_then(|v| v.as_array())?;
    let strings: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    if strings.len() == arr.len() {
        Some(strings)
    } else {
        None
    }
}

fn extract_session_id_from_params(params: Option<&Value>) -> Option<String> {
    params
        .and_then(|v| v.get("sessionId"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_uses_constants() {
        let adapter = HermesAdapter::new();
        assert_eq!(adapter.descriptor().adapter_type, ADAPTER_TYPE);
        assert_eq!(adapter.descriptor().label, ADAPTER_LABEL);
    }

    #[test]
    fn resolve_command_falls_back_to_default() {
        assert_eq!(
            resolve_command(&serde_json::json!({})),
            constants::HERMES_CLI
        );
        assert_eq!(
            resolve_command(&serde_json::json!({"command": "/custom"})),
            "/custom"
        );
        assert_eq!(
            resolve_command(&serde_json::json!({"hermesCommand": "/p/h"})),
            "/p/h"
        );
    }

    #[test]
    fn cfg_helpers_handle_null_and_invalid() {
        assert!(cfg_string(None).is_none());
        assert!(cfg_string(Some(&serde_json::json!(""))).is_none());
        assert!(cfg_string(Some(&serde_json::json!("  "))).is_none());
        assert_eq!(
            cfg_string(Some(&serde_json::json!("ok"))).as_deref(),
            Some("ok")
        );
        assert!(cfg_u64(Some(&serde_json::json!(0))).is_none());
        assert_eq!(cfg_u64(Some(&serde_json::json!(42))), Some(42));
        assert!(cfg_bool(Some(&serde_json::json!("true"))).is_none());
        assert_eq!(cfg_bool(Some(&serde_json::json!(true))), Some(true));
    }


    #[test]
    fn render_full_prompt_no_template_returns_context_prompt() {
        let context_prompt = "Hello, Hermes";
        let config = serde_json::json!({});
        let rendered = render_full_prompt(context_prompt, &config, None, None, None);
        assert_eq!(rendered, "Hello, Hermes");
    }

    #[test]
    fn render_full_prompt_with_template_renders_variables() {
        let context_prompt = "context prompt";
        let config = serde_json::json!({
            "promptTemplate": "Agent={{agentName}} Model={{model}}",
            "agentName": "Hermes",
            "model": "auto"
        });
        let rendered = render_full_prompt(
            context_prompt,
            &config,
            None,
            None,
            None,
        );
        assert_eq!(rendered, "Agent=Hermes Model=auto");
    }

    #[test]
    fn render_full_prompt_joins_wake_and_task_sections() {
        let context_prompt = "the body";
        let config = serde_json::json!({});
        let wake = serde_json::json!({
            "reason": "issue_assigned",
            "issue": {"id": "T-1"}
        });
        let task = "## task
Do something";
        let handoff = "";
        let rendered = render_full_prompt(
            context_prompt,
            &config,
            Some(&wake),
            Some(task),
            Some(handoff),
        );
        assert!(rendered.contains("issue_assigned"));
        assert!(rendered.contains("Do something"));
        assert!(rendered.contains("the body"));
        // handoff is empty → must not introduce blank lines
        assert!(!rendered.contains("\n\n\n"));
    }

    #[test]
    fn extract_session_id_from_params_handles_null() {
        assert!(extract_session_id_from_params(None).is_none());
        let params = serde_json::json!({"sessionId": "sess-1"});
        assert_eq!(
            extract_session_id_from_params(Some(&params)).as_deref(),
            Some("sess-1")
        );
    }
}
