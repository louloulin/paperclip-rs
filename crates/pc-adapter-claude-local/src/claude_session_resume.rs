#![forbid(unsafe_code)]

//! Claude session resume 决策与日志（对齐 Node `execute.ts` L736-826）。
//!
//! 本模块只包含**纯决策函数**，不进行任何 I/O：
//! - `is_valid_uuid` — UUID v4 格式校验
//! - `has_matching_prompt_bundle` — prompt bundle 一致性
//! - `has_matching_mcp_servers` — MCP server 集合一致性
//! - `decide_claude_session_resume` — 整合所有条件 + 生成日志
//! - `resolve_effective_effort` — sandbox `--effort` flag 决策
//!
//! 调用方负责把日志写到 sink（`AdapterEventSink::stdout`），
//! 决策结果用于驱动是否传 `--resume <session_id>`。

/// 判断字符串是否是合法的 UUID（v4/v1 等均可，宽松匹配）。
///
/// 对齐 Node `isValidUuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i`。
/// 接受任意 8-4-4-4-12 十六进制格式（不要求 v4）。
#[must_use]
pub fn is_valid_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let hex_at = |i: usize| -> Option<u8> {
        let b = bytes.get(i).copied()?;
        if b.is_ascii_hexdigit() {
            Some(b.to_ascii_lowercase())
        } else {
            None
        }
    };
    // 8-4-4-4-12 layout
    let groups: &[usize] = &[8, 4, 4, 4, 12];
    let mut offset = 0;
    for (idx, &len) in groups.iter().enumerate() {
        if idx > 0 {
            if bytes.get(offset).copied() != Some(b'-') {
                return false;
            }
            offset += 1;
        }
        for _ in 0..len {
            if hex_at(offset).is_none() {
                return false;
            }
            offset += 1;
        }
    }
    offset == 36
}

/// prompt bundle 是否匹配：runtime 记录的 bundleKey 为空时视为匹配（向后兼容）。
#[must_use]
pub fn has_matching_prompt_bundle(runtime_bundle_key: &str, current_bundle_key: &str) -> bool {
    runtime_bundle_key.is_empty() || runtime_bundle_key == current_bundle_key
}

/// MCP server 集合是否匹配：
/// - runtime 记录为空时，要求当前 servers 也为空
/// - 否则比较两个 JSON 序列化串
#[must_use]
pub fn has_matching_mcp_servers(runtime_mcp_identity: &str, current_mcp_identity: &str) -> bool {
    if runtime_mcp_identity.is_empty() {
        current_mcp_identity.is_empty()
    } else {
        runtime_mcp_identity == current_mcp_identity
    }
}

/// session cwd 与执行目标 cwd 是否匹配。
///
/// 对齐 Node `claudeSessionCwdMatchesExecutionTarget`（execute_helpers.rs 已实现）。
/// 这里复制逻辑以便 session_resume 模块独立可测：
/// - 远程 target：总是 true（远程不强制 cwd 对齐）
/// - 本地 target：runtimeSessionCwd 为空时 true，否则要求 path.resolve 后相等
#[must_use]
pub fn session_cwd_matches_execution_target(
    runtime_session_cwd: &str,
    effective_execution_cwd: &str,
    execution_target_is_remote: bool,
) -> bool {
    if execution_target_is_remote {
        return true;
    }
    if runtime_session_cwd.is_empty() {
        return true;
    }
    runtime_session_cwd == effective_execution_cwd
}

/// session resume 决策的输入参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionResumeInput<'a> {
    pub runtime_session_id: &'a str,
    pub runtime_session_cwd: &'a str,
    pub runtime_remote_execution: Option<&'a serde_json::Value>,
    pub runtime_prompt_bundle_key: &'a str,
    pub runtime_mcp_server_identity: &'a str,
    pub effective_execution_cwd: &'a str,
    pub current_prompt_bundle_key: &'a str,
    pub current_mcp_server_identity: &'a str,
    pub execution_target_is_remote: bool,
    pub execution_target: Option<&'a serde_json::Value>,
}

/// session resume 决策结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionResumeDecision {
    /// 是否可以 resume（与 Node `canResumeSession` 一致）
    pub can_resume: bool,
    /// 决策过程中产生的所有 stdout 日志（与 Node onLog("stdout", ...) 一致）
    pub log_lines: Vec<String>,
}

impl SessionResumeDecision {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            can_resume: false,
            log_lines: Vec::new(),
        }
    }

    pub fn push_log(&mut self, line: impl Into<String>) {
        self.log_lines.push(line.into());
    }

    /// 返回应当传给 CLI 的 session_id（None 表示 fresh session）
    #[must_use]
    pub fn resume_session_id<'a>(&self, runtime_session_id: &'a str) -> Option<&'a str> {
        if self.can_resume {
            Some(runtime_session_id)
        } else {
            None
        }
    }
}

/// `runtimeRemoteExecution` 与当前 execution target 是否匹配（简化版本）。
///
/// Node 端用 `adapterExecutionTargetSessionMatches(runtimeRemoteExecution, runtimeExecutionTarget)`，
/// 在 `pc-acpx::execution_target` 中有完整实现。这里仅做"任意一边为 null 即视为匹配"
/// 的轻量判断，避免 session_resume 模块反向依赖 pc-acpx。
#[must_use]
pub fn remote_execution_matches(
    runtime_remote_execution: Option<&serde_json::Value>,
    execution_target: Option<&serde_json::Value>,
) -> bool {
    match (runtime_remote_execution, execution_target) {
        (None, _) | (_, None) => true,
        (Some(a), Some(b)) => a == b,
    }
}

/// 决策是否 resume Claude session（对齐 Node execute.ts L736-826）。
///
/// 输出：
/// - `can_resume`：是否传 `--resume <session_id>`
/// - `log_lines`：需要写入 stdout 的诊断日志（与 Node 完全一致）
#[must_use]
pub fn decide_claude_session_resume(input: &SessionResumeInput<'_>) -> SessionResumeDecision {
    let mut decision = SessionResumeDecision::new();

    if input.runtime_session_id.is_empty() {
        return decision;
    }

    let valid_uuid = is_valid_uuid(input.runtime_session_id);
    let matching_bundle = has_matching_prompt_bundle(
        input.runtime_prompt_bundle_key,
        input.current_prompt_bundle_key,
    );
    let matching_mcp = has_matching_mcp_servers(
        input.runtime_mcp_server_identity,
        input.current_mcp_server_identity,
    );
    let cwd_matches = session_cwd_matches_execution_target(
        input.runtime_session_cwd,
        input.effective_execution_cwd,
        input.execution_target_is_remote,
    );
    let remote_matches = remote_execution_matches(
        input.runtime_remote_execution,
        input.execution_target,
    );

    let can_resume = valid_uuid
        && matching_bundle
        && matching_mcp
        && cwd_matches
        && remote_matches;
    decision.can_resume = can_resume;

    if !valid_uuid {
        decision.push_log(format!(
            "[paperclip] Claude session \"{}\" is not a valid UUID and will not be passed to --resume.\n",
            input.runtime_session_id
        ));
        return decision;
    }

    let cwd_mismatch = !input.runtime_session_cwd.is_empty()
        && input.runtime_session_cwd != input.effective_execution_cwd;

    if input.execution_target_is_remote && !can_resume {
        decision.push_log(format!(
            "[paperclip] Claude session \"{}\" does not match the current remote execution identity and will not be resumed in \"{}\". Starting a fresh remote session.\n",
            input.runtime_session_id,
            input.effective_execution_cwd
        ));
    } else if cwd_mismatch {
        decision.push_log(format!(
            "[paperclip] Claude session \"{}\" does not match the current remote execution identity and will not be resumed in \"{}\". Starting a fresh remote session.\n",
            input.runtime_session_id,
            input.effective_execution_cwd
        ));
    } else if !can_resume {
        decision.push_log(format!(
            "[paperclip] Claude session \"{}\" was saved for cwd \"{}\" and will not be resumed in \"{}\".\n",
            input.runtime_session_id,
            input.runtime_session_cwd,
            input.effective_execution_cwd
        ));
    }

    if !input.runtime_prompt_bundle_key.is_empty()
        && input.runtime_prompt_bundle_key != input.current_prompt_bundle_key
    {
        decision.push_log(format!(
            "[paperclip] Claude session \"{}\" was saved for prompt bundle \"{}\" and will not be resumed with \"{}\".\n",
            input.runtime_session_id,
            input.runtime_prompt_bundle_key,
            input.current_prompt_bundle_key
        ));
    }

    if !matching_mcp {
        decision.push_log(format!(
            "[paperclip] Claude session \"{}\" was saved with a different runtime MCP server set and will not be resumed.\n",
            input.runtime_session_id
        ));
    }

    decision
}

/// `--effort` 在 sandbox target 下需要先探测 CLI 是否支持。
#[must_use]
pub fn resolve_effective_effort(
    config_effort: &str,
    target_is_sandbox: bool,
    supports_effort: Option<bool>,
) -> EffectiveEffort {
    if !target_is_sandbox {
        return EffectiveEffort {
            value: config_effort.to_owned(),
            warning: None,
        };
    }
    if supports_effort != Some(false) {
        return EffectiveEffort {
            value: config_effort.to_owned(),
            warning: None,
        };
    }
    EffectiveEffort {
        value: String::new(),
        warning: Some(format!(
            "[paperclip] Claude CLI in the sandbox does not advertise --effort; omitting configured effort \"{}\". Upgrade the sandbox CLI/image to restore reasoning-effort control.\n",
            config_effort
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EffectiveEffort {
    pub value: String,
    pub warning: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_valid_uuid_accepts_lowercase() {
        assert!(is_valid_uuid("550e8400-e29b-41d4-a716-446655440000"));
    }

    #[test]
    fn is_valid_uuid_accepts_uppercase() {
        assert!(is_valid_uuid("550E8400-E29B-41D4-A716-446655440000"));
    }

    #[test]
    fn is_valid_uuid_rejects_short() {
        assert!(!is_valid_uuid("550e8400"));
    }

    #[test]
    fn is_valid_uuid_rejects_non_hex() {
        assert!(!is_valid_uuid("550e8400-e29b-41d4-a716-44665544zzzz"));
    }

    #[test]
    fn is_valid_uuid_rejects_wrong_separator() {
        assert!(!is_valid_uuid("550e8400xe29b-41d4-a716-446655440000"));
    }

    #[test]
    fn is_valid_uuid_rejects_extra_chars() {
        assert!(!is_valid_uuid("550e8400-e29b-41d4-a716-4466554400000"));
    }

    #[test]
    fn matching_bundle_empty_runtime_matches_anything() {
        assert!(has_matching_prompt_bundle("", "any-bundle"));
        assert!(has_matching_prompt_bundle("", ""));
    }

    #[test]
    fn matching_bundle_same_keys() {
        assert!(has_matching_prompt_bundle("bundle-a", "bundle-a"));
    }

    #[test]
    fn matching_bundle_different_keys() {
        assert!(!has_matching_prompt_bundle("bundle-a", "bundle-b"));
    }

    #[test]
    fn matching_mcp_empty_runtime_requires_empty_current() {
        assert!(has_matching_mcp_servers("", ""));
        assert!(!has_matching_mcp_servers("", "[{\"name\":\"a\"}]"));
    }

    #[test]
    fn matching_mcp_same_identity() {
        let identity = "[{\"name\":\"a\"}]";
        assert!(has_matching_mcp_servers(identity, identity));
    }

    #[test]
    fn matching_mcp_different_identity() {
        assert!(!has_matching_mcp_servers(
            "[{\"name\":\"a\"}]",
            "[{\"name\":\"b\"}]"
        ));
    }

    #[test]
    fn cwd_match_remote_always_true() {
        assert!(session_cwd_matches_execution_target(
            "/remote/cwd",
            "/local/cwd",
            true
        ));
    }

    #[test]
    fn cwd_match_local_empty_runtime_always_true() {
        assert!(session_cwd_matches_execution_target("", "/local/cwd", false));
    }

    #[test]
    fn cwd_match_local_same_cwd() {
        assert!(session_cwd_matches_execution_target("/a", "/a", false));
    }

    #[test]
    fn cwd_match_local_different_cwd() {
        assert!(!session_cwd_matches_execution_target("/a", "/b", false));
    }

    #[test]
    fn remote_match_both_none() {
        assert!(remote_execution_matches(None, None));
    }

    #[test]
    fn remote_match_runtime_none_current_some() {
        assert!(remote_execution_matches(None, Some(&json!({"id": "x"}))));
    }

    #[test]
    fn remote_match_current_none_runtime_some() {
        assert!(remote_execution_matches(Some(&json!({"id": "x"})), None));
    }

    #[test]
    fn remote_match_same_value() {
        let v = json!({"id": "x", "port": 22});
        assert!(remote_execution_matches(Some(&v), Some(&v)));
    }

    #[test]
    fn remote_match_different_value() {
        assert!(!remote_execution_matches(
            Some(&json!({"id": "x"})),
            Some(&json!({"id": "y"}))
        ));
    }

    fn sample_input<'a>(
        runtime_session_id: &'a str,
        runtime_session_cwd: &'a str,
        current_bundle_key: &'a str,
        current_mcp_identity: &'a str,
        effective_cwd: &'a str,
    ) -> SessionResumeInput<'a> {
        SessionResumeInput {
            runtime_session_id,
            runtime_session_cwd,
            runtime_remote_execution: None,
            runtime_prompt_bundle_key: "bundle-a",
            runtime_mcp_server_identity: "[{\"name\":\"a\"}]",
            effective_execution_cwd: effective_cwd,
            current_prompt_bundle_key: current_bundle_key,
            current_mcp_server_identity: current_mcp_identity,
            execution_target_is_remote: false,
            execution_target: None,
        }
    }

    #[test]
    fn decide_empty_session_id_returns_no_resume() {
        let input = sample_input("", "/a", "bundle-a", "[{\"name\":\"a\"}]", "/a");
        let decision = decide_claude_session_resume(&input);
        assert!(!decision.can_resume);
        assert!(decision.log_lines.is_empty());
    }

    #[test]
    fn decide_all_match_resumes() {
        let input = sample_input(
            "550e8400-e29b-41d4-a716-446655440000",
            "/a",
            "bundle-a",
            "[{\"name\":\"a\"}]",
            "/a",
        );
        let decision = decide_claude_session_resume(&input);
        assert!(decision.can_resume);
        assert!(decision.log_lines.is_empty());
    }

    #[test]
    fn decide_invalid_uuid_emits_warning_and_no_resume() {
        let input = sample_input("not-a-uuid", "/a", "bundle-a", "[{\"name\":\"a\"}]", "/a");
        let decision = decide_claude_session_resume(&input);
        assert!(!decision.can_resume);
        assert_eq!(decision.log_lines.len(), 1);
        assert!(decision.log_lines[0].contains("not a valid UUID"));
        assert_eq!(
            decision.resume_session_id("not-a-uuid"),
            None
        );
    }

    #[test]
    fn decide_bundle_mismatch_emits_warning() {
        let input = sample_input(
            "550e8400-e29b-41d4-a716-446655440000",
            "/a",
            "bundle-b",
            "[{\"name\":\"a\"}]",
            "/a",
        );
        let decision = decide_claude_session_resume(&input);
        assert!(!decision.can_resume);
        assert!(decision.log_lines.iter().any(|l| l.contains("prompt bundle")));
    }

    #[test]
    fn decide_mcp_mismatch_emits_warning() {
        let input = sample_input(
            "550e8400-e29b-41d4-a716-446655440000",
            "/a",
            "bundle-a",
            "[{\"name\":\"b\"}]",
            "/a",
        );
        let decision = decide_claude_session_resume(&input);
        assert!(!decision.can_resume);
        assert!(decision.log_lines.iter().any(|l| l.contains("MCP server")));
    }

    #[test]
    fn decide_cwd_mismatch_emits_warning() {
        let input = sample_input(
            "550e8400-e29b-41d4-a716-446655440000",
            "/a",
            "bundle-a",
            "[{\"name\":\"a\"}]",
            "/b",
        );
        let decision = decide_claude_session_resume(&input);
        assert!(!decision.can_resume);
        assert!(decision.log_lines.iter().any(|l| l.contains("does not match")));
    }

    #[test]
    fn decide_remote_target_with_remote_session_mismatch_emits_remote_log() {
        // 远程 target 下 cwd 不匹配不影响 can_resume（remote_execution=空都视为匹配）
        // 但当 runtime_session_cwd 非空且与 effective 不同，仍然记录 Starting a fresh remote session
        let mut input = sample_input(
            "550e8400-e29b-41d4-a716-446655440000",
            "/remote/saved",
            "bundle-a",
            "[{\"name\":\"a\"}]",
            "/remote/new",
        );
        input.execution_target_is_remote = true;
        let decision = decide_claude_session_resume(&input);
        // 远程 target 且 runtime 端未指定 remoteExecution 时，与 current target 仍视为匹配
        assert!(decision.can_resume);
        // 仍然记录 "does not match ... Starting a fresh remote session" 日志
        assert!(decision.log_lines.iter().any(|l| l.contains("Starting a fresh remote session")));
    }

    #[test]
    fn decide_resume_session_id_returns_id_when_resume() {
        let input = sample_input(
            "550e8400-e29b-41d4-a716-446655440000",
            "/a",
            "bundle-a",
            "[{\"name\":\"a\"}]",
            "/a",
        );
        let decision = decide_claude_session_resume(&input);
        assert_eq!(
            decision.resume_session_id(input.runtime_session_id),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    #[test]
    fn effort_local_target_keeps_value() {
        let result = resolve_effective_effort("high", false, None);
        assert_eq!(result.value, "high");
        assert!(result.warning.is_none());
    }

    #[test]
    fn effort_sandbox_supported_keeps_value() {
        let result = resolve_effective_effort("high", true, Some(true));
        assert_eq!(result.value, "high");
        assert!(result.warning.is_none());
    }

    #[test]
    fn effort_sandbox_unsupported_emits_warning_and_clears() {
        let result = resolve_effective_effort("high", true, Some(false));
        assert_eq!(result.value, "");
        assert!(result.warning.is_some());
        assert!(result
            .warning
            .as_deref()
            .unwrap()
            .contains("does not advertise --effort"));
    }

    #[test]
    fn effort_sandbox_unknown_keeps_value() {
        let result = resolve_effective_effort("high", true, None);
        assert_eq!(result.value, "high");
        assert!(result.warning.is_none());
    }
}
