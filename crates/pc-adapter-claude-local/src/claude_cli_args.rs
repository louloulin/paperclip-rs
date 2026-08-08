//! Claude CLI args 完整构造（对齐 Node `buildClaudeArgs` execute.ts L831-870）。
//!
//! 在 `build_claude_exec_args`（仅 adapter_config 视角）基础上，引入 context 决策：
//! - `--chrome` — 配置 chrome
//! - `--max-turns` — 仅当 max_turns > 0
//! - `--strict-mcp-config` — 仅当提供 mcp_config 时
//! - `--model` — 在 Bedrock auth 模式下，仅当 model 为 Bedrock-native 才传
//! - `--append-system-prompt-file` — resume session 时跳过（Node L852）
//!
//! 提供：
//! - `ClaudeArgsInput` — 输入参数聚合
//! - `build_claude_args_v2` — 完整构造 Vec<String>
//! - `should_pass_model_for_bedrock` — Bedrock 决策（独立可测）

use crate::claude_models::is_bedrock_model_id;

/// `buildClaudeArgs` 的输入参数（对齐 Node L831-870 闭包依赖）。
#[derive(Debug, Clone)]
pub struct ClaudeArgsInput<'a> {
    /// 是否 resume（resume 时跳过 --append-system-prompt-file）
    pub resume_session_id: Option<&'a str>,
    /// 系统 prompt 注入文件路径（resume 时不传）
    pub attempt_instructions_file_path: Option<&'a str>,
    /// 配置的 model（None 或空 → 不传 --model）
    pub model: Option<&'a str>,
    /// 是否启用 chrome
    pub chrome: bool,
    /// effort 等级（None → 不传 --effort）
    pub effective_effort: Option<&'a str>,
    /// 最大轮数（<= 0 → 不传 --max-turns）
    pub max_turns: i32,
    /// 运行时 MCP config 路径（Some → 同时传 --mcp-config + --strict-mcp-config）
    pub effective_mcp_config_path: Option<&'a str>,
    /// add-dir 目标目录（None → 不传 --add-dir）
    pub effective_prompt_bundle_add_dir: Option<&'a str>,
    /// 是否跳过 permissions 检查
    pub dangerously_skip_permissions: bool,
    /// 是否远程 target（影响 permission args）
    pub target_is_remote: bool,
    /// Bedrock auth（决定 --model 是否传）
    pub is_bedrock_auth: bool,
    /// 额外参数透传
    pub extra_args: &'a [String],
}

/// 是否应该在 Bedrock auth 模式下传递 `--model`。
///
/// Bedrock 不接受 Anthropic 风格 ID（如 `claude-opus-4-6`），
/// 只接受 region-qualified IDs（如 `us.anthropic.claude-opus-4-8-v1`）或 ARN。
#[must_use]
pub fn should_pass_model_for_bedrock(model: Option<&str>, is_bedrock_auth: bool) -> bool {
    let Some(m) = model.filter(|m| !m.is_empty()) else {
        return false;
    };
    if !is_bedrock_auth {
        return true;
    }
    // Bedrock 模式：仅当 model 是 Bedrock-native 才传
    is_bedrock_model_id(m)
}

/// 构造完整 Claude CLI 参数（对齐 Node `buildClaudeArgs`）。
#[must_use]
pub fn build_claude_args_v2(input: &ClaudeArgsInput<'_>) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    // 必选协议 flag（按 Node 顺序）
    args.push("--print".to_owned());
    args.push("--output-format".to_owned());
    args.push("stream-json".to_owned());
    args.push("--verbose".to_owned());

    if let Some(sid) = input.resume_session_id {
        args.push("--resume".to_owned());
        args.push(sid.to_owned());
    }

    // Permission args（简化版：dangerouslySkipPermissions → --dangerously-skip-permissions）
    if input.dangerously_skip_permissions {
        args.push("--dangerously-skip-permissions".to_owned());
    }

    if input.chrome {
        args.push("--chrome".to_owned());
    }

    if should_pass_model_for_bedrock(input.model, input.is_bedrock_auth) {
        args.push("--model".to_owned());
        args.push(input.model.unwrap_or_default().to_owned());
    }

    if let Some(effort) = input.effective_effort.filter(|e| !e.is_empty()) {
        args.push("--effort".to_owned());
        args.push(effort.to_owned());
    }

    if input.max_turns > 0 {
        args.push("--max-turns".to_owned());
        args.push(input.max_turns.to_string());
    }

    // Node L852: resume 时跳过 instructions（已经在 session cache 里）
    if let Some(path) = input.attempt_instructions_file_path {
        if input.resume_session_id.is_none() {
            args.push("--append-system-prompt-file".to_owned());
            args.push(path.to_owned());
        }
    }

    if let Some(mcp_path) = input.effective_mcp_config_path {
        args.push("--mcp-config".to_owned());
        args.push(mcp_path.to_owned());
        args.push("--strict-mcp-config".to_owned());
    }

    if let Some(add_dir) = input.effective_prompt_bundle_add_dir {
        args.push("--add-dir".to_owned());
        args.push(add_dir.to_owned());
    }

    for extra in input.extra_args {
        args.push(extra.clone());
    }

    args
}


/// 从 adapter_config + context 构造 `ClaudeArgsInput`。
///
/// 把 Node `buildClaudeArgs` 闭包的所有外部依赖都打包到这个函数中，
/// 调用方只需要提供 config + context 即可。
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_claude_args_input_from_context<'a>(
    config: &'a serde_json::Value,
    effective_execution_cwd: &'a str,
    effective_effort: Option<&'a str>,
    effective_mcp_config_path: Option<&'a str>,
    effective_prompt_bundle_add_dir: Option<&'a str>,
    max_turns: i32,
    resume_session_id: Option<&'a str>,
    attempt_instructions_file_path: Option<&'a str>,
    extra_args: &'a [String],
    is_bedrock_auth: bool,
    target_is_remote: bool,
) -> ClaudeArgsInput<'a> {
    let model = config
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let chrome = config
        .get("chrome")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let dangerously_skip_permissions = config
        .get("dangerouslySkipPermissions")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let _ = effective_execution_cwd; // 不直接使用，预留给未来

    ClaudeArgsInput {
        resume_session_id,
        attempt_instructions_file_path,
        model,
        chrome,
        effective_effort,
        max_turns,
        effective_mcp_config_path,
        effective_prompt_bundle_add_dir,
        dangerously_skip_permissions,
        target_is_remote,
        is_bedrock_auth,
        extra_args,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_input() -> ClaudeArgsInput<'static> {
        ClaudeArgsInput {
            resume_session_id: None,
            attempt_instructions_file_path: None,
            model: None,
            chrome: false,
            effective_effort: None,
            max_turns: 0,
            effective_mcp_config_path: None,
            effective_prompt_bundle_add_dir: None,
            dangerously_skip_permissions: false,
            target_is_remote: false,
            is_bedrock_auth: false,
            extra_args: &[],
        }
    }

    #[test]
    fn base_input_produces_minimal_args() {
        let args = build_claude_args_v2(&base_input());
        assert_eq!(
            args,
            vec!["--print", "--output-format", "stream-json", "--verbose"]
        );
    }

    #[test]
    fn resume_session_id_adds_resume_flag() {
        let mut input = base_input();
        input.resume_session_id = Some("abc-123");
        let args = build_claude_args_v2(&input);
        assert!(args.windows(2).any(|w| w[0] == "--resume" && w[1] == "abc-123"));
    }

    #[test]
    fn chrome_flag_added_when_enabled() {
        let mut input = base_input();
        input.chrome = true;
        let args = build_claude_args_v2(&input);
        assert!(args.contains(&"--chrome".to_owned()));
    }

    #[test]
    fn chrome_flag_absent_by_default() {
        let input = base_input();
        let args = build_claude_args_v2(&input);
        assert!(!args.contains(&"--chrome".to_owned()));
    }

    #[test]
    fn max_turns_added_when_positive() {
        let mut input = base_input();
        input.max_turns = 10;
        let args = build_claude_args_v2(&input);
        let idx = args.iter().position(|a| a == "--max-turns").unwrap();
        assert_eq!(args[idx + 1], "10");
    }

    #[test]
    fn max_turns_skipped_when_zero() {
        let input = base_input();
        let args = build_claude_args_v2(&input);
        assert!(!args.contains(&"--max-turns".to_owned()));
    }

    #[test]
    fn max_turns_skipped_when_negative() {
        let mut input = base_input();
        input.max_turns = -1;
        let args = build_claude_args_v2(&input);
        assert!(!args.contains(&"--max-turns".to_owned()));
    }

    #[test]
    fn mcp_config_paired_with_strict_mcp_config() {
        let mut input = base_input();
        input.effective_mcp_config_path = Some("/path/to/mcp.json");
        let args = build_claude_args_v2(&input);
        let mcp_idx = args.iter().position(|a| a == "--mcp-config").unwrap();
        assert_eq!(args[mcp_idx + 1], "/path/to/mcp.json");
        assert_eq!(args[mcp_idx + 2], "--strict-mcp-config");
    }

    #[test]
    fn mcp_config_absent_means_no_strict_flag() {
        let input = base_input();
        let args = build_claude_args_v2(&input);
        assert!(!args.contains(&"--mcp-config".to_owned()));
        assert!(!args.contains(&"--strict-mcp-config".to_owned()));
    }

    #[test]
    fn bedrock_auth_skips_anthropic_short_model() {
        let mut input = base_input();
        input.is_bedrock_auth = true;
        input.model = Some("claude-opus-4-6");
        let args = build_claude_args_v2(&input);
        assert!(!args.contains(&"--model".to_owned()));
    }

    #[test]
    fn bedrock_auth_keeps_bedrock_native_model() {
        let mut input = base_input();
        input.is_bedrock_auth = true;
        input.model = Some("us.anthropic.claude-opus-4-8-v1");
        let args = build_claude_args_v2(&input);
        let idx = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[idx + 1], "us.anthropic.claude-opus-4-8-v1");
    }

    #[test]
    fn bedrock_auth_keeps_arn_model() {
        let mut input = base_input();
        input.is_bedrock_auth = true;
        input.model = Some("arn:aws:bedrock:us-east-1:123:inference-profile/abc");
        let args = build_claude_args_v2(&input);
        assert!(args.contains(&"--model".to_owned()));
    }

    #[test]
    fn non_bedrock_always_passes_model() {
        let mut input = base_input();
        input.is_bedrock_auth = false;
        input.model = Some("claude-opus-4-6");
        let args = build_claude_args_v2(&input);
        assert!(args.contains(&"--model".to_owned()));
    }

    #[test]
    fn empty_model_is_never_passed() {
        let mut input = base_input();
        input.model = Some("");
        let args = build_claude_args_v2(&input);
        assert!(!args.contains(&"--model".to_owned()));
    }

    #[test]
    fn append_system_prompt_skipped_on_resume() {
        let mut input = base_input();
        input.resume_session_id = Some("abc");
        input.attempt_instructions_file_path = Some("/path/to/instructions.md");
        let args = build_claude_args_v2(&input);
        assert!(!args.contains(&"--append-system-prompt-file".to_owned()));
    }

    #[test]
    fn append_system_prompt_added_when_no_resume() {
        let mut input = base_input();
        input.resume_session_id = None;
        input.attempt_instructions_file_path = Some("/path/to/instructions.md");
        let args = build_claude_args_v2(&input);
        let idx = args
            .iter()
            .position(|a| a == "--append-system-prompt-file")
            .unwrap();
        assert_eq!(args[idx + 1], "/path/to/instructions.md");
    }

    #[test]
    fn add_dir_added_when_provided() {
        let mut input = base_input();
        input.effective_prompt_bundle_add_dir = Some("/path/to/bundle");
        let args = build_claude_args_v2(&input);
        let idx = args.iter().position(|a| a == "--add-dir").unwrap();
        assert_eq!(args[idx + 1], "/path/to/bundle");
    }

    #[test]
    fn dangerously_skip_permissions_added() {
        let mut input = base_input();
        input.dangerously_skip_permissions = true;
        let args = build_claude_args_v2(&input);
        assert!(args.contains(&"--dangerously-skip-permissions".to_owned()));
    }

    #[test]
    fn extra_args_appended_at_end() {
        let mut input = base_input();
        let extras = vec!["--foo".to_owned(), "--bar".to_owned()];
        input.extra_args = &extras;
        let args = build_claude_args_v2(&input);
        let last_two = &args[args.len() - 2..];
        assert_eq!(last_two, &["--foo", "--bar"]);
    }

    #[test]
    fn args_order_matches_node_baseline() {
        let mut input = base_input();
        input.model = Some("claude-opus-4-6");
        input.chrome = true;
        input.effective_effort = Some("high");
        input.max_turns = 50;
        input.effective_mcp_config_path = Some("/mcp.json");
        input.effective_prompt_bundle_add_dir = Some("/bundle");
        input.dangerously_skip_permissions = true;
        input.attempt_instructions_file_path = Some("/instr.md");
        let args = build_claude_args_v2(&input);

        // 验证顺序：print, output-format, stream-json, verbose, dangerously-skip, chrome, model, effort, max-turns, append, mcp-config, strict-mcp-config, add-dir
        let expected_prefix = vec![
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--dangerously-skip-permissions",
            "--chrome",
            "--model",
            "claude-opus-4-6",
            "--effort",
            "high",
            "--max-turns",
            "50",
            "--append-system-prompt-file",
            "/instr.md",
            "--mcp-config",
            "/mcp.json",
            "--strict-mcp-config",
            "--add-dir",
            "/bundle",
        ];
        assert_eq!(args, expected_prefix);
    }
}
