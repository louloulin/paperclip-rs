//! Codex 远程 SSH workspace 决策纯函数。
//!
//! 对齐 Node `codex-local/src/server/execute.ts` 的远程执行分支
//! （`prepareWorkspaceForSshExecution` / `restoreWorkspaceFromSshExecution` /
//! `syncDirectoryToSsh` / `stageCodexHomeForSync` /
//! `startAdapterExecutionTargetPaperclipBridge`）。
//!
//! # 设计范围
//!
//! 本模块只包含 **纯决策函数**，不发起真实 SSH / 进程 / 网络 I/O：
//! - `resolve_remote_workspace_dir` — remoteDir 缺省回退到 remoteCwd
//! - `managed_remote_runtime_workspace_dir` — 计算 `.paperclip-runtime/runs/<runId>/workspace`
//! - `remote_codex_home_dir` — 计算远程 `.paperclip-runtime/codex/home`
//! - `codex_home_sync_allowlist` — CODEX_SYNC_ALLOWLIST 白名单
//! - `remote_execution_uses_paperclip_bridge` — 判定是否启动 bridge
//! - `remote_session_identity_matches` — 判定保存的 session 是否匹配当前 target
//! - `should_resume_remote_session` — 远程执行是否允许 resume
//! - `remote_sync_excludes` — SSH 同步排除项
//!
//! 真实 SSH 执行器（`syncDirectoryToSsh` / `importGitWorkspaceToSsh` /
//! `restoreWorkspaceFromSshExecution`）在 `pc-acpx::ssh` 中已提供基础；
//! `stage_codex_home_for_sync` 在 `codex_home_staging.rs` 中已实现。route 层
//! 组合本模块的决策函数 + pc-acpx 执行器。

use pc_acpx::execution_target::{
    adapter_execution_target_session_matches, adapter_execution_target_uses_paperclip_bridge,
    AdapterExecutionTarget,
};
use std::path::Path;

/// 解析远程 workspace 目录。`remote_dir` 缺省时回退到 `remote_cwd`。
/// 对齐 Node `prepareWorkspaceForSshExecution` 的
/// `const remoteDir = input.remoteDir ?? input.spec.remoteCwd;`。
#[must_use]
pub fn resolve_remote_workspace_dir(remote_dir: Option<&str>, remote_cwd: &str) -> String {
    remote_dir
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| remote_cwd.to_string())
}

/// 计算受管远程运行目录：
/// `<remoteWorkspacePath>/.paperclip-runtime/runs/<runId>/workspace`。
///
/// 对齐 Node `managedRemoteWorkspace` 测试场景中的
/// `/remote/workspace/.paperclip-runtime/runs/run-1/workspace`。
#[must_use]
pub fn managed_remote_runtime_workspace_dir(remote_workspace_path: &str, run_id: &str) -> String {
    let base = remote_workspace_path.trim_end_matches('/');
    format!("{base}/.paperclip-runtime/runs/{run_id}/workspace")
}

/// 计算远程 Codex home 资产目录：
/// `<workspaceDir>/.paperclip-runtime/codex/home`。
///
/// 对齐 Node 测试场景中
/// `${managedRemoteWorkspace}/.paperclip-runtime/codex/home`。
#[must_use]
pub fn remote_codex_home_dir(workspace_dir: &str) -> String {
    let base = workspace_dir.trim_end_matches('/');
    format!("{base}/.paperclip-runtime/codex/home")
}

/// Codex home 同步白名单：与 Node `CODEX_SYNC_ALLOWLIST` 一致。
/// 派生自 seeding 常量（config.json / config.toml / instructions.md / auth.json / skills）。
#[must_use]
pub fn codex_home_sync_allowlist() -> &'static [&'static str] {
    crate::codex_home::CODEX_SYNC_ALLOWLIST
}

/// 判定远程 execution target 是否启动 paperclip bridge。
/// 对齐 Node `adapterExecutionTargetUsesPaperclipBridge(runtimeExecutionTarget)`。
#[must_use]
pub fn remote_execution_uses_paperclip_bridge(target: Option<&AdapterExecutionTarget>) -> bool {
    adapter_execution_target_uses_paperclip_bridge(target)
}

/// 判定保存的 session 是否匹配当前 execution target。
/// 对齐 Node `adapterExecutionTargetSessionMatches(saved, runtimeExecutionTarget)`。
#[must_use]
pub fn remote_session_identity_matches(
    saved: &serde_json::Value,
    target: Option<&AdapterExecutionTarget>,
) -> bool {
    adapter_execution_target_session_matches(saved, target)
}

/// 远程执行时是否允许 resume 已保存的 session。
///
/// 对齐 Node `codex-local/src/server/execute.ts` 的 `canResumeSession`：
///
/// ```ts
/// const canResumeSession =
///   runtimeSessionId.length > 0 &&
///   (runtimeSessionCwd.length === 0 ||
///     path.resolve(runtimeSessionCwd) === path.resolve(effectiveExecutionCwd)) &&
///   adapterExecutionTargetSessionMatches(runtimeRemoteExecution, runtimeExecutionTarget);
/// ```
///
/// 判定条件：
/// - `session_id` 非空
/// - `runtime_session_cwd` 为空 OR 与 `effective_execution_cwd` 路径归一化后相等
/// - 保存的 remoteExecution identity 匹配当前 target（SSH 4 元组 + remoteCwd）
///
/// 返回 `(allow_resume, reason)`。`reason` 为 `None` 时表示允许 resume。
#[must_use]
pub fn should_resume_remote_session(
    session_id: Option<&str>,
    runtime_session_cwd: Option<&str>,
    effective_execution_cwd: Option<&str>,
    saved_remote_execution: Option<&serde_json::Value>,
    target: Option<&AdapterExecutionTarget>,
) -> (bool, Option<&'static str>) {
    let session_id = session_id.map(str::trim).filter(|s| !s.is_empty());
    if session_id.is_none() {
        return (false, Some("no saved session id"));
    }
    let runtime_session_cwd = runtime_session_cwd.map(str::trim).unwrap_or("");
    let effective_execution_cwd = effective_execution_cwd.map(str::trim).unwrap_or("");
    let cwd_matches = runtime_session_cwd.is_empty()
        || canonicalize_like_resolve(runtime_session_cwd)
            == canonicalize_like_resolve(effective_execution_cwd);
    if !cwd_matches {
        return (
            false,
            Some("saved session cwd does not match execution cwd"),
        );
    }
    let Some(saved) = saved_remote_execution else {
        return (false, Some("no saved remote execution identity"));
    };
    if !remote_session_identity_matches(saved, target) {
        return (
            false,
            Some("saved session identity does not match current target"),
        );
    }
    (true, None)
}

/// SSH 同步排除项。git 快照路径排除 `.git` + `.paperclip-runtime`；
/// 非 git 路径只排除 `.paperclip-runtime`。
/// 对齐 Node `prepareWorkspaceForSshExecution` 的两个分支。
#[must_use]
pub fn remote_sync_excludes(git_backed: bool) -> &'static [&'static str] {
    if git_backed {
        &[".git", ".paperclip-runtime"]
    } else {
        &[".paperclip-runtime"]
    }
}

/// 归一化路径用于 `*_WORKSPACE_CWD` 比较。
/// 简化对齐 Node `path.resolve`：相对路径基于当前工作目录展开，
/// `..` / `.` 折叠。远程路径不做此处理（在调用方已传绝对远程路径）。
fn canonicalize_like_resolve(path_str: &str) -> String {
    let path = Path::new(path_str);
    if path.is_absolute() {
        let mut parts: Vec<&str> = Vec::new();
        for component in path.components() {
            use std::path::Component;
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    parts.pop();
                }
                Component::Normal(s) => parts.push(s.to_str().unwrap_or_default()),
                Component::RootDir | Component::Prefix(_) => {
                    parts.push(component.as_os_str().to_str().unwrap_or_default());
                }
            }
        }
        parts.join("/")
    } else {
        // 相对路径：基于当前工作目录展开（近似 Node path.resolve）
        let cwd = std::env::current_dir().unwrap_or_default();
        let joined = cwd.join(path_str);
        joined
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_acpx::execution_target::adapter_execution_target_from_remote_execution;
    use serde_json::json;

    fn ssh_target(remote_cwd: &str) -> AdapterExecutionTarget {
        let value = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "port": 2222,
            "username": "fixture",
            "remoteWorkspacePath": "/remote/workspace",
            "remoteCwd": remote_cwd,
            "privateKey": "PRIVATE KEY",
            "knownHosts": "[127.0.0.1]:2222 ssh-ed25519 AAAA",
            "strictHostKeyChecking": true,
        });
        adapter_execution_target_from_remote_execution(&value, None)
            .expect("valid remote execution target")
    }

    #[test]
    fn resolve_remote_workspace_dir_uses_remote_dir_when_present() {
        assert_eq!(
            resolve_remote_workspace_dir(Some("/remote/custom"), "/remote/cwd"),
            "/remote/custom"
        );
    }

    #[test]
    fn resolve_remote_workspace_dir_falls_back_to_remote_cwd() {
        assert_eq!(
            resolve_remote_workspace_dir(None, "/remote/cwd"),
            "/remote/cwd"
        );
    }

    #[test]
    fn resolve_remote_workspace_dir_trims_blank_remote_dir() {
        assert_eq!(
            resolve_remote_workspace_dir(Some("   "), "/remote/cwd"),
            "/remote/cwd"
        );
    }

    #[test]
    fn managed_remote_runtime_workspace_dir_builds_run_path() {
        assert_eq!(
            managed_remote_runtime_workspace_dir("/remote/workspace", "run-1"),
            "/remote/workspace/.paperclip-runtime/runs/run-1/workspace"
        );
    }

    #[test]
    fn managed_remote_runtime_workspace_dir_handles_trailing_slash() {
        assert_eq!(
            managed_remote_runtime_workspace_dir("/remote/workspace/", "run-1"),
            "/remote/workspace/.paperclip-runtime/runs/run-1/workspace"
        );
    }

    #[test]
    fn remote_codex_home_dir_builds_runtime_path() {
        assert_eq!(
            remote_codex_home_dir("/remote/workspace/.paperclip-runtime/runs/run-1/workspace"),
            "/remote/workspace/.paperclip-runtime/runs/run-1/workspace/.paperclip-runtime/codex/home"
        );
    }

    #[test]
    fn codex_home_sync_allowlist_matches_node() {
        assert_eq!(
            codex_home_sync_allowlist(),
            &[
                "config.json",
                "config.toml",
                "instructions.md",
                "auth.json",
                "skills"
            ]
        );
    }

    #[test]
    fn remote_execution_uses_paperclip_bridge_ssh_target() {
        let target = ssh_target("/remote/workspace");
        assert!(remote_execution_uses_paperclip_bridge(Some(&target)));
    }

    #[test]
    fn remote_execution_uses_paperclip_bridge_local_target() {
        assert!(!remote_execution_uses_paperclip_bridge(None));
    }

    #[test]
    fn remote_session_identity_matches_saved_ssh_identity() {
        let target =
            ssh_target("/remote/workspace/.paperclip-runtime/runs/run-ssh-resume/workspace");
        let saved = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "username": "fixture",
            "port": 2222,
            "remoteCwd": "/remote/workspace/.paperclip-runtime/runs/run-ssh-resume/workspace",
        });
        assert!(remote_session_identity_matches(&saved, Some(&target)));
    }

    #[test]
    fn remote_session_identity_mismatches_wrong_host() {
        let target = ssh_target("/remote/workspace");
        let saved = json!({
            "transport": "ssh",
            "host": "other-host",
            "username": "fixture",
            "port": 2222,
        });
        assert!(!remote_session_identity_matches(&saved, Some(&target)));
    }

    #[test]
    fn remote_session_identity_mismatches_empty_saved() {
        let target = ssh_target("/remote/workspace");
        assert!(!remote_session_identity_matches(&json!({}), Some(&target)));
    }

    #[test]
    fn should_resume_remote_session_allows_matching_identity() {
        let managed = "/remote/workspace/.paperclip-runtime/runs/run-ssh-resume/workspace";
        let target = ssh_target(managed);
        let saved = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "username": "fixture",
            "port": 2222,
            "remoteCwd": managed,
        });
        let (allow, reason) = should_resume_remote_session(
            Some("session-123"),
            Some(managed),
            Some(managed),
            Some(&saved),
            Some(&target),
        );
        assert!(allow);
        assert!(reason.is_none());
    }

    #[test]
    fn should_resume_remote_session_allows_empty_session_cwd() {
        let target = ssh_target("/remote/workspace");
        let saved = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "username": "fixture",
            "port": 2222,
            "remoteCwd": "/remote/workspace",
        });
        let (allow, reason) = should_resume_remote_session(
            Some("session-123"),
            None,
            Some("/remote/workspace"),
            Some(&saved),
            Some(&target),
        );
        assert!(allow);
        assert!(reason.is_none());
    }

    #[test]
    fn should_resume_remote_session_denies_without_session_id() {
        let target = ssh_target("/remote/workspace");
        let saved = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "username": "fixture",
            "port": 2222,
            "remoteCwd": "/remote/workspace",
        });
        let (allow, reason) = should_resume_remote_session(
            None,
            Some("/remote/workspace"),
            Some("/remote/workspace"),
            Some(&saved),
            Some(&target),
        );
        assert!(!allow);
        assert_eq!(reason, Some("no saved session id"));
    }

    #[test]
    fn should_resume_remote_session_denies_cwd_mismatch() {
        let target = ssh_target("/remote/workspace");
        let saved = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "username": "fixture",
            "port": 2222,
            "remoteCwd": "/remote/workspace",
        });
        let (allow, reason) = should_resume_remote_session(
            Some("session-123"),
            Some("/remote/workspace-other"),
            Some("/remote/workspace"),
            Some(&saved),
            Some(&target),
        );
        assert!(!allow);
        assert_eq!(
            reason,
            Some("saved session cwd does not match execution cwd")
        );
    }

    #[test]
    fn should_resume_remote_session_denies_identity_mismatch() {
        let target = ssh_target("/remote/workspace");
        let saved = json!({
            "transport": "ssh",
            "host": "wrong-host",
            "username": "fixture",
            "port": 2222,
            "remoteCwd": "/remote/workspace",
        });
        let (allow, reason) = should_resume_remote_session(
            Some("session-123"),
            Some("/remote/workspace"),
            Some("/remote/workspace"),
            Some(&saved),
            Some(&target),
        );
        assert!(!allow);
        assert_eq!(
            reason,
            Some("saved session identity does not match current target")
        );
    }

    #[test]
    fn should_resume_remote_session_denies_missing_saved_identity() {
        let target = ssh_target("/remote/workspace");
        let (allow, reason) = should_resume_remote_session(
            Some("session-123"),
            Some("/remote/workspace"),
            Some("/remote/workspace"),
            None,
            Some(&target),
        );
        assert!(!allow);
        assert_eq!(reason, Some("no saved remote execution identity"));
    }

    #[test]
    fn should_resume_remote_session_denies_remote_cwd_mismatch() {
        let target = ssh_target("/remote/workspace");
        let saved = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "username": "fixture",
            "port": 2222,
            "remoteCwd": "/remote/other",
        });
        let (allow, reason) = should_resume_remote_session(
            Some("session-123"),
            Some("/remote/workspace"),
            Some("/remote/workspace"),
            Some(&saved),
            Some(&target),
        );
        assert!(!allow);
        assert_eq!(
            reason,
            Some("saved session identity does not match current target")
        );
    }

    #[test]
    fn remote_sync_excludes_git_backed() {
        assert_eq!(remote_sync_excludes(true), &[".git", ".paperclip-runtime"]);
    }

    #[test]
    fn remote_sync_excludes_non_git() {
        assert_eq!(remote_sync_excludes(false), &[".paperclip-runtime"]);
    }
}
