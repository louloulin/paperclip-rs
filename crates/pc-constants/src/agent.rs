//! Agent 域常量。
//!
//! 注：AGENT_ADAPTER_TYPES / AdapterType enum 已在 `pc-adapter-type` crate；本模块聚焦
//! agent 角色 / icon / 默认并发数等通用常量。

/// Agent 默认最大并发运行数。
pub const AGENT_DEFAULT_MAX_CONCURRENT_RUNS: u32 = 20;

/// Workspace branch 路由变量名。
pub const WORKSPACE_BRANCH_ROUTINE_VARIABLE: &str = "workspaceBranch";

/// Agent icon 名（与 Node `AGENT_ICON_NAMES` 对齐；用于 UI 渲染）。
pub const AGENT_ICON_NAMES: &[&str] = &[
    "bot",
    "sparkles",
    "rocket",
    "wrench",
    "hammer",
    "lightbulb",
    "search",
    "compass",
    "shield",
    "zap",
    "terminal",
    "code",
    "git-branch",
    "cpu",
    "layers",
    "package",
    "database",
    "workflow",
    "settings",
    "user",
];

/// Project icon 名（与 Node `PROJECT_ICON_NAMES` 对齐；UI 渲染）。
pub const PROJECT_ICON_NAMES: &[&str] = &[
    "folder",
    "folder-open",
    "file",
    "file-text",
    "package",
    "box",
    "archive",
    "tag",
    "bookmark",
    "star",
    "heart",
    "flag",
    "book-open",
    "code",
    "terminal",
    "database",
    "server",
    "cloud",
    "globe",
    "workflow",
];

/// Adapter-agnostic 配置 key（adapter provider 不关心的运行时配置）。
///
/// Node 上游在 adapter 加载时被忽略；保留用于跨 adapter 共享默认。
pub const ADAPTER_AGNOSTIC_KEYS: &[&str] = &[
    "autoRetry",
    "background",
    "cache",
    "debug",
    "env",
    "headers",
    "image",
    "labels",
    "logLevel",
    "network",
    "ports",
    "pullPolicy",
    "readOnly",
    "restart",
    "runtime",
    "sandbox",
    "secrets",
    "securityContext",
    "serviceAccount",
    "shell",
    "telemetry",
    "timeout",
    "user",
    "volumes",
    "workdir",
];

/// Model profile key（cheap / quality / etc.）。
pub const MODEL_PROFILE_KEYS: &[&str] = &["cheap"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_max_concurrent_runs_is_20() {
        assert_eq!(AGENT_DEFAULT_MAX_CONCURRENT_RUNS, 20);
    }

    #[test]
    fn workspace_branch_variable_name_stable() {
        assert_eq!(WORKSPACE_BRANCH_ROUTINE_VARIABLE, "workspaceBranch");
    }

    #[test]
    fn icon_names_non_empty() {
        assert!(!AGENT_ICON_NAMES.is_empty());
        assert!(!PROJECT_ICON_NAMES.is_empty());
    }

    #[test]
    fn adapter_agnostic_keys_contains_expected() {
        assert!(ADAPTER_AGNOSTIC_KEYS.contains(&"timeout"));
        assert!(ADAPTER_AGNOSTIC_KEYS.contains(&"secrets"));
    }

    #[test]
    fn model_profile_keys_match_node() {
        assert_eq!(MODEL_PROFILE_KEYS, &["cheap"]);
    }
}
