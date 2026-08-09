//! Claude 权限参数构造（对齐 Node permissions.ts）。
//!
//! 决定 `--dangerously-skip-permissions` vs `--allowedTools <list>`：
//! - 本地：`--dangerously-skip-permissions`
//! - 远程：显式 allowlist（避免无人应答权限弹窗导致挂起）

/// 远程 / 沙箱模式下允许的 Claude Code 工具白名单。
///
/// 与 Node `SANDBOX_ALLOWED_TOOLS` 字符串完全一致（顺序与空格分隔），
/// 便于逐项对照 https://docs.claude.com/en/docs/claude-code/built-in-tools
/// 调整。
pub const SANDBOX_ALLOWED_TOOLS: &str = "Task AskUserQuestion Bash CronCreate CronDelete CronList Edit EnterPlanMode EnterWorktree ExitPlanMode ExitWorktree Glob Grep Monitor NotebookEdit PushNotification Read RemoteTrigger ScheduleWakeup Skill TaskOutput TaskStop TodoWrite ToolSearch WebFetch WebSearch Write";

#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudePermissionInput {
    pub dangerously_skip_permissions: bool,
    pub target_is_remote: bool,
}

/// 探测（probe）模式的权限参数构造。
///
/// - 未启用 `dangerously_skip_permissions` → 空数组（保留 CLI 默认行为）
/// - 远程 target → 显式 `--allowedTools <whitelist>`
/// - 本地 → `--dangerously-skip-permissions`
#[must_use]
pub fn build_claude_probe_permission_args(input: ClaudePermissionInput) -> Vec<String> {
    if !input.dangerously_skip_permissions {
        return Vec::new();
    }
    if input.target_is_remote {
        vec![
            "--allowedTools".to_string(),
            SANDBOX_ALLOWED_TOOLS.to_string(),
        ]
    } else {
        vec!["--dangerously-skip-permissions".to_string()]
    }
}

/// 执行模式的权限参数构造（对齐 Node `buildClaudeExecutionPermissionArgs`）。
#[must_use]
pub fn build_claude_execution_permission_args(input: ClaudePermissionInput) -> Vec<String> {
    if !input.dangerously_skip_permissions {
        return Vec::new();
    }
    if input.target_is_remote {
        vec![
            "--allowedTools".to_string(),
            SANDBOX_ALLOWED_TOOLS.to_string(),
        ]
    } else {
        vec!["--dangerously-skip-permissions".to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_empty_for_probe() {
        let args = build_claude_probe_permission_args(ClaudePermissionInput {
            dangerously_skip_permissions: false,
            target_is_remote: false,
        });
        assert!(args.is_empty());
    }

    #[test]
    fn disabled_returns_empty_for_execution() {
        let args = build_claude_execution_permission_args(ClaudePermissionInput {
            dangerously_skip_permissions: false,
            target_is_remote: true,
        });
        assert!(args.is_empty());
    }

    #[test]
    fn enabled_local_uses_dangerously_skip_for_probe() {
        let args = build_claude_probe_permission_args(ClaudePermissionInput {
            dangerously_skip_permissions: true,
            target_is_remote: false,
        });
        assert_eq!(args, vec!["--dangerously-skip-permissions"]);
    }

    #[test]
    fn enabled_local_uses_dangerously_skip_for_execution() {
        let args = build_claude_execution_permission_args(ClaudePermissionInput {
            dangerously_skip_permissions: true,
            target_is_remote: false,
        });
        assert_eq!(args, vec!["--dangerously-skip-permissions"]);
    }

    #[test]
    fn enabled_remote_uses_allowed_tools_for_probe() {
        let args = build_claude_probe_permission_args(ClaudePermissionInput {
            dangerously_skip_permissions: true,
            target_is_remote: true,
        });
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "--allowedTools");
        assert!(args[1].contains("Bash"));
        assert!(args[1].contains("Edit"));
        assert!(args[1].contains("Write"));
        assert!(args[1].contains("WebFetch"));
    }

    #[test]
    fn enabled_remote_uses_allowed_tools_for_execution() {
        let args = build_claude_execution_permission_args(ClaudePermissionInput {
            dangerously_skip_permissions: true,
            target_is_remote: true,
        });
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "--allowedTools");
        assert!(args[1].contains("Read"));
        assert!(args[1].contains("Grep"));
    }

    #[test]
    fn sandbox_allowed_tools_includes_documented_set() {
        for tool in [
            "Task",
            "AskUserQuestion",
            "Bash",
            "CronCreate",
            "CronDelete",
            "CronList",
            "Edit",
            "EnterPlanMode",
            "EnterWorktree",
            "ExitPlanMode",
            "ExitWorktree",
            "Glob",
            "Grep",
            "Monitor",
            "NotebookEdit",
            "PushNotification",
            "Read",
            "RemoteTrigger",
            "ScheduleWakeup",
            "Skill",
            "TaskOutput",
            "TaskStop",
            "TodoWrite",
            "ToolSearch",
            "WebFetch",
            "WebSearch",
            "Write",
        ] {
            assert!(
                SANDBOX_ALLOWED_TOOLS.split_whitespace().any(|t| t == tool),
                "tool {tool} missing from SANDBOX_ALLOWED_TOOLS"
            );
        }
    }
}
