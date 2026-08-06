//! `workspace_runtime_env_builders` — workspace 命令/清理 env 构造 + 命令解析。
//!
//! 与 Node `buildWorkspaceCommandEnv` / `buildExecutionWorkspaceCleanupEnv` /
//! `resolveRepoManagedWorkspaceCommand` 1:1 对齐。
//!
//! 设计目标：纯函数模块，不读取真实环境变量；所有 env 输入由调用方提供。
//! `resolveRepoManagedWorkspaceCommand` 通过 `path_exists` 回调避免 IO 依赖。
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::workspace_runtime_string_utils::quote_shell_arg;

// ============================================================================
// buildWorkspaceCommandEnv
// ============================================================================

/// `ExecutionWorkspaceInput`：minimal subset for env builder。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceCommandEnvBase {
    pub base_cwd: String,
    pub source: String,
    pub repo_ref: Option<String>,
    pub repo_url: Option<String>,
    pub project_id: Option<String>,
    pub workspace_id: Option<String>,
}

/// `BuildWorkspaceCommandEnvInput`。
#[derive(Debug, Clone)]
pub struct BuildWorkspaceCommandEnvInput<'a> {
    pub base: &'a WorkspaceCommandEnvBase,
    pub repo_root: &'a str,
    pub worktree_path: &'a str,
    pub branch_name: &'a str,
    pub issue_work_mode: Option<&'a str>,
    pub issue_id: Option<&'a str>,
    pub issue_identifier: Option<&'a str>,
    pub issue_title: Option<&'a str>,
    pub agent_id: Option<&'a str>,
    pub agent_name: &'a str,
    pub company_id: &'a str,
    pub created: bool,
}

/// `buildWorkspaceCommandEnv(input)`：构造 PAPERCLIP_* 环境变量集。
///
/// 与 Node 1:1 对齐：
/// - 起始为 `process.env`（调用方传入），叠加一系列 PAPERCLIP_* 字段
/// - 所有 None 字段 → 空字符串
/// - created → "true" / "false"
pub fn build_workspace_command_env(input: BuildWorkspaceCommandEnvInput<'_>) -> MapStringString {
    let mut env: MapStringString = MapStringString::new();
    env.insert(
        "PAPERCLIP_WORKSPACE_CWD".into(),
        input.worktree_path.to_string(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_PATH".into(),
        input.worktree_path.to_string(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_WORKTREE_PATH".into(),
        input.worktree_path.to_string(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_BRANCH".into(),
        input.branch_name.to_string(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_BASE_CWD".into(),
        input.base.base_cwd.clone(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_REPO_ROOT".into(),
        input.repo_root.to_string(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_SOURCE".into(),
        input.base.source.clone(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_REPO_REF".into(),
        input.base.repo_ref.clone().unwrap_or_default(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_REPO_URL".into(),
        input.base.repo_url.clone().unwrap_or_default(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_CREATED".into(),
        if input.created { "true" } else { "false" }.to_string(),
    );
    env.insert(
        "PAPERCLIP_PROJECT_ID".into(),
        input.base.project_id.clone().unwrap_or_default(),
    );
    env.insert(
        "PAPERCLIP_PROJECT_WORKSPACE_ID".into(),
        input.base.workspace_id.clone().unwrap_or_default(),
    );
    env.insert(
        "PAPERCLIP_AGENT_ID".into(),
        input.agent_id.unwrap_or_default().to_string(),
    );
    env.insert("PAPERCLIP_AGENT_NAME".into(), input.agent_name.to_string());
    env.insert("PAPERCLIP_COMPANY_ID".into(), input.company_id.to_string());
    env.insert(
        "PAPERCLIP_ISSUE_ID".into(),
        input.issue_id.unwrap_or_default().to_string(),
    );
    env.insert(
        "PAPERCLIP_ISSUE_IDENTIFIER".into(),
        input.issue_identifier.unwrap_or_default().to_string(),
    );
    env.insert(
        "PAPERCLIP_ISSUE_TITLE".into(),
        input.issue_title.unwrap_or_default().to_string(),
    );
    env.insert(
        "PAPERCLIP_ISSUE_WORK_MODE".into(),
        input.issue_work_mode.unwrap_or_default().to_string(),
    );
    env
}

// ============================================================================
// buildExecutionWorkspaceCleanupEnv
// ============================================================================

/// `CleanupWorkspaceFields`：minimal subset for cleanup env builder。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupWorkspaceFields {
    pub cwd: Option<String>,
    pub provider_ref: Option<String>,
    pub branch_name: Option<String>,
    pub repo_url: Option<String>,
    pub base_ref: Option<String>,
    pub project_id: Option<String>,
    pub project_workspace_id: Option<String>,
    pub source_issue_id: Option<String>,
}

/// `buildExecutionWorkspaceCleanupEnv(input)`：
///
/// 与 Node 1:1 对齐：
/// - PAPERCLIP_WORKSPACE_CWD = workspace.cwd ?? ""
/// - PAPERCLIP_WORKSPACE_PATH = workspace.cwd ?? ""
/// - PAPERCLIP_WORKSPACE_WORKTREE_PATH = providerRef ?? cwd ?? ""
/// - PAPERCLIP_WORKSPACE_BASE_CWD = projectWorkspaceCwd ?? ""
/// - PAPERCLIP_WORKSPACE_REPO_ROOT = projectWorkspaceCwd ?? ""
/// - 其它字段同 Node
pub fn build_execution_workspace_cleanup_env(
    workspace: &CleanupWorkspaceFields,
    project_workspace_cwd: Option<&str>,
) -> MapStringString {
    let cwd = workspace.cwd.clone().unwrap_or_default();
    let provider_ref = workspace.provider_ref.clone();
    let worktree_path = provider_ref.unwrap_or_else(|| cwd.clone());

    let mut env = MapStringString::new();
    env.insert("PAPERCLIP_WORKSPACE_CWD".into(), cwd.clone());
    env.insert("PAPERCLIP_WORKSPACE_PATH".into(), cwd.clone());
    env.insert("PAPERCLIP_WORKSPACE_WORKTREE_PATH".into(), worktree_path);
    env.insert(
        "PAPERCLIP_WORKSPACE_BRANCH".into(),
        workspace.branch_name.clone().unwrap_or_default(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_BASE_CWD".into(),
        project_workspace_cwd.unwrap_or_default().to_string(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_REPO_ROOT".into(),
        project_workspace_cwd.unwrap_or_default().to_string(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_REPO_URL".into(),
        workspace.repo_url.clone().unwrap_or_default(),
    );
    env.insert(
        "PAPERCLIP_WORKSPACE_REPO_REF".into(),
        workspace.base_ref.clone().unwrap_or_default(),
    );
    env.insert(
        "PAPERCLIP_PROJECT_ID".into(),
        workspace.project_id.clone().unwrap_or_default(),
    );
    env.insert(
        "PAPERCLIP_PROJECT_WORKSPACE_ID".into(),
        workspace.project_workspace_id.clone().unwrap_or_default(),
    );
    env.insert(
        "PAPERCLIP_ISSUE_ID".into(),
        workspace.source_issue_id.clone().unwrap_or_default(),
    );
    env
}

// ============================================================================
// resolveRepoManagedWorkspaceCommand
// ============================================================================

/// 内部 Map 类型：使用 `std::collections::BTreeMap` 避免 serde_json::Map 对 String->String 的限制。
pub type MapStringString = std::collections::BTreeMap<String, String>;

/// `resolveRepoManagedWorkspaceCommand(command, repoRoot, pathExists)`：
///
/// 与 Node 1:1 对齐：
/// - patterns[0]: `(bash|sh|zsh)\s+["']?\./[^"'\s]+["']?(?:\s.*)?`
/// - patterns[1]: `["']?\./[^"'\s]+["']?(?:\s.*)?`
/// - 相对路径去掉 `./` 前缀，与 repoRoot 拼接
/// - 调用 pathExists 检查拼接后路径是否存在
/// - 存在：替换原相对路径为 quoted absolute path
/// - 不存在：返回原 command
pub fn resolve_repo_managed_workspace_command<F>(
    command: &str,
    repo_root: &str,
    path_exists: F,
) -> String
where
    F: Fn(&str) -> bool,
{
    for pattern in managed_command_patterns() {
        if let Some(caps) = pattern.captures(command) {
            let relative_path = match caps.name("relative") {
                Some(m) => m.as_str(),
                None => continue,
            };
            // relative path 形如 "./foo/bar.sh"
            let stripped = relative_path.strip_prefix("./").unwrap_or(relative_path);
            let joined = Path::new(repo_root).join(stripped);
            let joined_str = joined.to_string_lossy().to_string();
            if !path_exists(&joined_str) {
                continue;
            }
            let prefix = caps.name("prefix").map(|m| m.as_str()).unwrap_or("");
            let suffix = caps.name("suffix").map(|m| m.as_str()).unwrap_or("");
            return format!("{}{}{}", prefix, quote_shell_arg(&joined_str), suffix);
        }
    }
    command.to_string()
}

fn managed_command_patterns() -> Vec<regex::Regex> {
    use std::sync::OnceLock;
    static RE: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        vec![
            // bash|sh|zsh + relative
            regex::Regex::new(
                r#"^(?<prefix>(?:bash|sh|zsh)\s+)["']?(?<relative>\./[^"'\s]+)["']?(?<suffix>(?:\s.*)?)$"#,
            )
            .unwrap(),
            // 单独相对
            regex::Regex::new(
                r#"^["']?(?<relative>\./[^"'\s]+)["']?(?<suffix>(?:\s.*)?)$"#,
            )
            .unwrap(),
        ]
    })
    .clone()
}

// ============================================================================
// formatManagedGitWorktreeBranchInspection
// ============================================================================

/// `ManagedGitWorktreeBranchInspection`：输入类型（passthrough）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManagedGitWorktreeBranchInspection {
    pub valid: bool,
    pub reason: String,
    pub reason_code: String,
    pub repo_root: String,
    pub worktree_path: String,
    pub expected_branch_name: String,
    pub actual_branch_name: Option<String>,
}

/// `formatManagedGitWorktreeBranchInspection(input)`：passthrough 重新归一化字段。
///
/// 与 Node 1:1 对齐：只挑出展示需要的字段重新组装。
pub fn format_managed_git_worktree_branch_inspection(
    input: ManagedGitWorktreeBranchInspection,
) -> ManagedGitWorktreeBranchInspection {
    ManagedGitWorktreeBranchInspection {
        valid: input.valid,
        reason: input.reason,
        reason_code: input.reason_code,
        repo_root: input.repo_root,
        worktree_path: input.worktree_path,
        expected_branch_name: input.expected_branch_name,
        actual_branch_name: input.actual_branch_name,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ----- buildWorkspaceCommandEnv -----

    #[test]
    fn workspace_command_env_basic() {
        let base = WorkspaceCommandEnvBase {
            base_cwd: "/base".into(),
            source: "test".into(),
            repo_ref: Some("refs/x".into()),
            repo_url: Some("git@x".into()),
            project_id: Some("p-1".into()),
            workspace_id: Some("pws-1".into()),
        };
        let env = build_workspace_command_env(BuildWorkspaceCommandEnvInput {
            base: &base,
            repo_root: "/repo",
            worktree_path: "/wt",
            branch_name: "feat/x",
            issue_work_mode: Some("work"),
            issue_id: Some("iss-1"),
            issue_identifier: Some("PROJ-1"),
            issue_title: Some("fix"),
            agent_id: Some("a-1"),
            agent_name: "agent-1",
            company_id: "co-1",
            created: true,
        });
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_CWD").unwrap(), "/wt");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_PATH").unwrap(), "/wt");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_WORKTREE_PATH").unwrap(), "/wt");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_BRANCH").unwrap(), "feat/x");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_BASE_CWD").unwrap(), "/base");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_REPO_ROOT").unwrap(), "/repo");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_SOURCE").unwrap(), "test");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_REPO_REF").unwrap(), "refs/x");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_REPO_URL").unwrap(), "git@x");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_CREATED").unwrap(), "true");
        assert_eq!(env.get("PAPERCLIP_PROJECT_ID").unwrap(), "p-1");
        assert_eq!(env.get("PAPERCLIP_PROJECT_WORKSPACE_ID").unwrap(), "pws-1");
        assert_eq!(env.get("PAPERCLIP_AGENT_ID").unwrap(), "a-1");
        assert_eq!(env.get("PAPERCLIP_AGENT_NAME").unwrap(), "agent-1");
        assert_eq!(env.get("PAPERCLIP_COMPANY_ID").unwrap(), "co-1");
        assert_eq!(env.get("PAPERCLIP_ISSUE_ID").unwrap(), "iss-1");
        assert_eq!(env.get("PAPERCLIP_ISSUE_IDENTIFIER").unwrap(), "PROJ-1");
        assert_eq!(env.get("PAPERCLIP_ISSUE_TITLE").unwrap(), "fix");
        assert_eq!(env.get("PAPERCLIP_ISSUE_WORK_MODE").unwrap(), "work");
    }

    #[test]
    fn workspace_command_env_none_fields_empty() {
        let base = WorkspaceCommandEnvBase::default();
        let env = build_workspace_command_env(BuildWorkspaceCommandEnvInput {
            base: &base,
            repo_root: "/repo",
            worktree_path: "/wt",
            branch_name: "feat",
            issue_work_mode: None,
            issue_id: None,
            issue_identifier: None,
            issue_title: None,
            agent_id: None,
            agent_name: "agent-1",
            company_id: "co-1",
            created: false,
        });
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_CREATED").unwrap(), "false");
        assert_eq!(env.get("PAPERCLIP_REPO_REF").map(|s| s.as_str()), None);
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_REPO_REF").unwrap(), "");
        assert_eq!(env.get("PAPERCLIP_ISSUE_ID").unwrap(), "");
    }

    // ----- buildExecutionWorkspaceCleanupEnv -----

    #[test]
    fn cleanup_env_basic() {
        let w = CleanupWorkspaceFields {
            cwd: Some("/repo".into()),
            provider_ref: Some("/wt".into()),
            branch_name: Some("feat".into()),
            repo_url: Some("git@x".into()),
            base_ref: Some("main".into()),
            project_id: Some("p-1".into()),
            project_workspace_id: Some("pws-1".into()),
            source_issue_id: Some("iss-1".into()),
        };
        let env = build_execution_workspace_cleanup_env(&w, Some("/base"));
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_CWD").unwrap(), "/repo");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_PATH").unwrap(), "/repo");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_WORKTREE_PATH").unwrap(), "/wt");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_BRANCH").unwrap(), "feat");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_BASE_CWD").unwrap(), "/base");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_REPO_ROOT").unwrap(), "/base");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_REPO_URL").unwrap(), "git@x");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_REPO_REF").unwrap(), "main");
        assert_eq!(env.get("PAPERCLIP_PROJECT_ID").unwrap(), "p-1");
        assert_eq!(env.get("PAPERCLIP_PROJECT_WORKSPACE_ID").unwrap(), "pws-1");
        assert_eq!(env.get("PAPERCLIP_ISSUE_ID").unwrap(), "iss-1");
    }

    #[test]
    fn cleanup_env_provider_ref_missing_falls_back_to_cwd() {
        let w = CleanupWorkspaceFields {
            cwd: Some("/repo".into()),
            provider_ref: None,
            branch_name: None,
            repo_url: None,
            base_ref: None,
            project_id: None,
            project_workspace_id: None,
            source_issue_id: None,
        };
        let env = build_execution_workspace_cleanup_env(&w, None);
        assert_eq!(
            env.get("PAPERCLIP_WORKSPACE_WORKTREE_PATH").unwrap(),
            "/repo"
        );
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_BASE_CWD").unwrap(), "");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_BRANCH").unwrap(), "");
    }

    #[test]
    fn cleanup_env_all_none() {
        let w = CleanupWorkspaceFields::default();
        let env = build_execution_workspace_cleanup_env(&w, None);
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_CWD").unwrap(), "");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_PATH").unwrap(), "");
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_WORKTREE_PATH").unwrap(), "");
    }

    // ----- resolveRepoManagedWorkspaceCommand -----

    #[test]
    fn resolve_repo_managed_command_replaces_when_exists() {
        let resolved =
            resolve_repo_managed_workspace_command("bash ./scripts/setup.sh", "/repo", |p| {
                p == "/repo/scripts/setup.sh"
            });
        assert_eq!(resolved, "bash '/repo/scripts/setup.sh'");
    }

    #[test]
    fn resolve_repo_managed_command_keeps_when_missing() {
        let resolved =
            resolve_repo_managed_workspace_command("bash ./scripts/missing.sh", "/repo", |_| false);
        assert_eq!(resolved, "bash ./scripts/missing.sh");
    }

    #[test]
    fn resolve_repo_managed_command_no_prefix_with_suffix() {
        let resolved =
            resolve_repo_managed_workspace_command("./bin/run --port=3000", "/repo", |p| {
                p == "/repo/bin/run"
            });
        assert_eq!(resolved, "'/repo/bin/run' --port=3000");
    }

    #[test]
    fn resolve_repo_managed_command_non_relative_unchanged() {
        let resolved = resolve_repo_managed_workspace_command("pnpm install", "/repo", |_| true);
        assert_eq!(resolved, "pnpm install");
    }

    #[test]
    fn resolve_repo_managed_command_quoted_relative() {
        let resolved =
            resolve_repo_managed_workspace_command("sh './scripts/x.sh'", "/repo", |p| {
                p == "/repo/scripts/x.sh"
            });
        assert_eq!(resolved, "sh '/repo/scripts/x.sh'");
    }

    // ----- formatManagedGitWorktreeBranchInspection -----

    #[test]
    fn format_managed_inspection_passthrough() {
        let input = ManagedGitWorktreeBranchInspection {
            valid: true,
            reason: "ok".into(),
            reason_code: "ok_code".into(),
            repo_root: "/repo".into(),
            worktree_path: "/wt".into(),
            expected_branch_name: "main".into(),
            actual_branch_name: Some("feat".into()),
        };
        let out = format_managed_git_worktree_branch_inspection(input);
        assert!(out.valid);
        assert_eq!(out.reason, "ok");
        assert_eq!(out.reason_code, "ok_code");
        assert_eq!(out.expected_branch_name, "main");
        assert_eq!(out.actual_branch_name.as_deref(), Some("feat"));
    }
}
