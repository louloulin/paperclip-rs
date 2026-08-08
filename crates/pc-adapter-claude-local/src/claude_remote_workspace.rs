//! Claude 远程 SSH workspace 决策纯函数。
//!
//! 对齐 Node `claude-local/src/server/execute.ts` 的远程执行分支
//! （`prepareWorkspaceForSshExecution` / `restoreWorkspaceFromSshExecution` /
//! `syncDirectoryToSsh` / `startAdapterExecutionTargetPaperclipBridge`）。
//!
//! # 设计范围
//!
//! 本模块只包含 **纯决策函数**，不发起真实 SSH / 进程 / 网络 I/O：
//! - `resolve_remote_workspace_dir` — remoteDir 缺省回退到 remoteCwd
//! - `managed_remote_runtime_workspace_dir` — 计算 `.paperclip-runtime/runs/<runId>/workspace`
//! - `remote_execution_uses_paperclip_bridge` — 判定是否启动 bridge
//! - `remote_session_identity_matches` — 判定保存的 session 是否匹配当前 target
//! - `should_resume_remote_session` — 远程执行是否允许 resume（身份匹配 + 非 bridge 策略）
//! - `remote_env_replaces_workspace_cwd` — 环境变量重写决策
//! - `remote_sync_excludes` — SSH 同步排除项（git 快照 / 非 git）
//!
//! 真实 SSH 执行器（`syncDirectoryToSsh` / `importGitWorkspaceToSsh` /
//! `exportGitWorkspaceFromSsh` / `restoreWorkspaceFromSshExecution`）在
//! `pc-acpx::ssh` / `pc-acpx::git_workspace_sync` 中已提供基础；route 层
//! 组合本模块的决策函数 + pc-acpx 执行器。

use pc_acpx::execution_target::{
    adapter_execution_target_session_matches, adapter_execution_target_uses_paperclip_bridge,
    AdapterExecutionTarget,
};
use std::path::Path;

/// 解析远程 workspace 目录。`remoteDir` 缺省时回退到 spec.remoteCwd。
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

/// 判定远程 execution target 是否启动 paperclip bridge。
/// 对齐 Node `adapterExecutionTargetUsesPaperclipBridge(runtimeExecutionTarget)`。
#[must_use]
pub fn remote_execution_uses_paperclip_bridge(target: Option<&AdapterExecutionTarget>) -> bool {
    adapter_execution_target_uses_paperclip_bridge(target)
}

/// claude 主执行流程 bridge 计划决策（adapterKey 固定为 `"claude"`）。
/// 对齐 Node claude execute.ts L679-692：仅远程且 usesBridge 时启动；
/// host token 缺失时报错；返回计划后由调用方
/// `Object.assign(env, plan.env)` 合并。
pub fn decide_claude_execution_bridge_plan(
    run_id: &str,
    target: Option<&AdapterExecutionTarget>,
    runtime_root_dir: Option<&str>,
    timeout_sec: Option<f64>,
    env_paperclip_api_key: Option<&str>,
    host_api_url: Option<&str>,
) -> Result<Option<pc_acpx::execution_target::StartPaperclipBridgePlan>, String> {
    pc_acpx::execution_target::decide_execution_bridge_plan(
        run_id,
        target,
        runtime_root_dir,
        "claude",
        timeout_sec,
        env_paperclip_api_key,
        host_api_url,
    )
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
/// 对齐 Node `execute.ts` 的 resume 决策分支：
/// - 保存的 session identity 必须匹配当前 target（SSH 4 元组 / Sandbox 5 元组）
/// - 远程执行必须匹配（`executionTargetIsRemote`）
///
/// 返回 `(allow_resume, reason)`。`reason` 为 `None` 时表示允许 resume。
#[must_use]
pub fn should_resume_remote_session(
    session_id: Option<&str>,
    runtime_session_cwd: Option<&str>,
    effective_execution_cwd: Option<&str>,
    execution_target_is_remote: bool,
    prompt_bundle_key: Option<&str>,
    bundle_key: Option<&str>,
    mcp_server_identity: Option<&str>,
    mcp_identity: Option<&str>,
    mcp_server_count: usize,
    saved_remote_execution: Option<&serde_json::Value>,
    target: Option<&AdapterExecutionTarget>,
) -> (bool, Option<&'static str>) {
    let session_id = session_id.map(str::trim).filter(|s| !s.is_empty());
    if session_id.is_none() {
        return (false, Some("no saved session id"));
    }
    if !is_valid_uuid(session_id.unwrap_or_default()) {
        return (false, Some("session id is not a valid UUID"));
    }
    // hasMatchingPromptBundle: promptBundleKey 为空或与 bundleKey 相同
    let prompt_bundle_key = prompt_bundle_key.map(str::trim).unwrap_or("");
    let bundle_key = bundle_key.map(str::trim).unwrap_or("");
    if !prompt_bundle_key.is_empty() && prompt_bundle_key != bundle_key {
        return (false, Some("prompt bundle key does not match"));
    }
    // hasMatchingMcpServers: mcpServerIdentity 为空 → mcpServers 必须为空；
    // 非空 → 必须与 runtimeMcpIdentity 相同
    let mcp_server_identity = mcp_server_identity.map(str::trim).unwrap_or("");
    let mcp_identity = mcp_identity.map(str::trim).unwrap_or("");
    let mcp_matches = if mcp_server_identity.is_empty() {
        mcp_server_count == 0
    } else {
        mcp_server_identity == mcp_identity
    };
    if !mcp_matches {
        return (false, Some("MCP server identity does not match"));
    }
    // claudeSessionCwdMatchesExecutionTarget:
    // 远程 target 或 cwd 为空时恒 true；否则 resolve 后比较
    let runtime_session_cwd = runtime_session_cwd.map(str::trim).unwrap_or("");
    let effective_execution_cwd = effective_execution_cwd.map(str::trim).unwrap_or("");
    if !execution_target_is_remote
        && !runtime_session_cwd.is_empty()
        && canonicalize_like_resolve(runtime_session_cwd)
            != canonicalize_like_resolve(effective_execution_cwd)
    {
        return (false, Some("saved session cwd does not match execution cwd"));
    }
    let Some(saved) = saved_remote_execution else {
        return (false, Some("no saved remote execution identity"));
    };
    if !remote_session_identity_matches(saved, target) {
        return (false, Some("saved session identity does not match current target"));
    }
    (true, None)
}

/// 判断字符串是否为 UUID v4（对齐 Node `/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i`）。
#[must_use]
pub fn is_valid_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let mut groups = [0usize; 5];
    let mut group_idx = 0usize;
    let mut hex_digits = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'-' {
            if i == 8 || i == 13 || i == 18 || i == 23 {
                groups[group_idx] = hex_digits;
                group_idx += 1;
                hex_digits = 0;
            } else {
                return false;
            }
        } else if b.is_ascii_hexdigit() {
            hex_digits += 1;
        } else {
            return false;
        }
    }
    if group_idx != 4 {
        return false;
    }
    groups[4] = hex_digits;
    groups == [8, 4, 4, 4, 12]
}

/// 远程执行时环境变量 `*_WORKSPACE_CWD` 是否会被重写。
///
/// 对齐 Node `rewriteWorkspaceCwdEnvVarsForExecution`：
/// 仅当 `executionTargetIsRemote && localWorkspaceCwd && remoteWorkspaceCwd`
/// 全部满足时，值为本地 workspaceCwd 的 `*_WORKSPACE_CWD` 变量才会被重写。
#[must_use]
pub fn remote_env_replaces_workspace_cwd(
    env_value: &str,
    local_workspace_cwd: &str,
    remote_workspace_cwd: &str,
) -> bool {
    if local_workspace_cwd.trim().is_empty() || remote_workspace_cwd.trim().is_empty() {
        return false;
    }
    let trimmed = env_value.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Node 使用 path.resolve(trimmed) 与 path.resolve(localWorkspaceCwd) 比较。
    // 这里用归一化后的绝对路径语义近似：先 canonicalize 本地路径，
    // 远程路径按原样传递（远程 shell 路径不做 host 语义归一化）。
    let local_norm = canonicalize_like_resolve(local_workspace_cwd);
    let value_norm = canonicalize_like_resolve(trimmed);
    value_norm == local_norm
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

/// 启动 claude 执行的真实 paperclip bridge（R492，替换 R490 env-only 合并）。
///
/// - 本地 / 非 bridge target → `Ok(None)`
/// - 远程 SSH target → 真实启动完整 bridge（SSH runner + node server +
///   worker），返回 [`pc_acpx::bridge_executor::StartedAdapterBridge`]
///   供 execute 结束后 teardown；`on_log` 收到
///   `[paperclip] Starting sandbox callback bridge ...` 启动日志
/// - 远程 Sandbox target → `Ok(None)`（provider runner 未在 Rust 侧实现，
///   保持 R490 env-only 合并）
///
/// host token 从 `base_env.PAPERCLIP_API_KEY` 提取（Node
/// `hostApiToken: env.PAPERCLIP_API_KEY`）；缺失时报错（Node 在
/// `startAdapterExecutionTargetPaperclipBridge` 内 throw）。
pub async fn start_claude_execution_bridge(
    run_id: &str,
    base_env: &std::collections::BTreeMap<String, String>,
    execution_target: Option<&serde_json::Value>,
    timeout_sec: Option<f64>,
    on_log: Option<std::sync::Arc<dyn Fn(&str) + Send + Sync>>,
) -> Result<Option<pc_acpx::bridge_executor::StartedAdapterBridge>, String> {
    let target = execution_target
        .and_then(pc_acpx::execution_target::parse_adapter_execution_target);
    if !adapter_execution_target_uses_paperclip_bridge(target.as_ref()) {
        return Ok(None);
    }
    let host_api_token = base_env
        .get("PAPERCLIP_API_KEY")
        .map(String::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty());
    let host_api_url = base_env
        .get("PAPERCLIP_RUNTIME_API_URL")
        .or_else(|| base_env.get("PAPERCLIP_API_URL"))
        .map(String::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty());
    pc_acpx::bridge_executor::start_adapter_execution_bridge_for_target(
        &pc_acpx::bridge_executor::StartAdapterBridgeForTargetInput {
            run_id,
            target: target.as_ref(),
            runtime_root_dir: None,
            adapter_key: "claude",
            timeout_sec,
            host_api_token,
            host_api_url,
            on_log,
        },
    )
    .await
}

/// 是否启用 remote process session bridge（对齐 Node execute.ts
/// `useRemoteProcessSession` gate）：
///
/// ```ts
/// const useRemoteProcessSession =
///   executionTarget?.kind === "remote" &&
///   executionTarget.transport === "sandbox" &&
///   Boolean(executionTarget.runner) &&
///   Boolean(agentCommandShell);
/// ```
///
/// Rust 侧 `AdapterSandboxExecutionTarget` 尚无 provider runner 字段，
/// execute 调用时 `has_runner` 恒为 false（与 R492 paperclip bridge
/// sandbox 分支一致）；参数显式化以保留完整 gate 语义与测试路径，
/// 未来接入 provider runner 后自动生效。
#[must_use]
pub fn use_claude_remote_process_session(
    target: Option<&AdapterExecutionTarget>,
    has_runner: bool,
    has_agent_command_shell: bool,
) -> bool {
    matches!(
        target,
        Some(AdapterExecutionTarget::Remote(
            pc_acpx::execution_target::AdapterRemoteExecutionTarget::Sandbox(_)
        ))
    ) && has_runner
        && has_agent_command_shell
}

/// 启动 claude 执行的 process session bridge（R493，对齐 Node execute.ts
/// `startAdapterExecutionTargetProcessSessionBridge` 分支）。
///
/// - 非 sandbox 远程 target → `Ok(None)`（Node gate：仅 remote + sandbox）
/// - sandbox target 但 runner 缺失 → `Ok(None)`（Node 在
///   `requireSandboxRunner` 处 throw；Rust 侧 sandbox 尚无 provider
///   runner，与 R492 paperclip bridge 分支一致保持回退语义）
/// - sandbox target + runner → 真实启动 bridge（远端脚本 sha 门控同步 +
///   mkdir + nohup node + 本地 proxy），返回
///   [`pc_acpx::process_session_bridge::ProcessSessionBridgeHandle`]
///   供 execute 结束后 teardown
///
/// launch 参数对齐 Node：`command: "sh"`、`args: ["-lc", "exec <shell>"]`、
/// `cwd: sessionCwd`（sandbox 时即 target.remoteCwd，空串自动回退）；
/// launch env 由调用方在 paperclip bridge env 合并后传入（等价于 Node
/// env thunk 求值结果）。
pub async fn start_claude_process_session_bridge(
    run_id: &str,
    execution_target: Option<&serde_json::Value>,
    runtime_root_dir: Option<&str>,
    adapter_key: &str,
    agent_command_shell: &str,
    cwd: &str,
    launch_env: &std::collections::BTreeMap<String, String>,
    timeout_sec: Option<f64>,
    runner: Option<std::sync::Arc<dyn pc_acpx::bridge_executor::BridgeCommandRunner>>,
    on_log: Option<std::sync::Arc<dyn Fn(&str) + Send + Sync>>,
) -> Result<Option<pc_acpx::process_session_bridge::ProcessSessionBridgeHandle>, String> {
    let target = execution_target
        .and_then(pc_acpx::execution_target::parse_adapter_execution_target);
    let Some(runner) = runner else {
        return Ok(None);
    };
    let shell = agent_command_shell.trim();
    if !use_claude_remote_process_session(target.as_ref(), true, !shell.is_empty()) {
        return Ok(None);
    }
    let args = ["-lc".to_string(), format!("exec {shell}")];
    pc_acpx::process_session_bridge::start_adapter_execution_target_process_session_bridge(
        &pc_acpx::process_session_bridge::StartProcessSessionBridgeInput {
            run_id,
            target: target.as_ref(),
            runtime_root_dir,
            adapter_key,
            command: "sh",
            args: &args,
            cwd,
            launch_env,
            timeout_sec,
            runner,
            on_log,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_acpx::execution_target::{
        adapter_execution_target_from_remote_execution, AdapterRemoteExecutionTarget,
    };
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
        assert_eq!(resolve_remote_workspace_dir(None, "/remote/cwd"), "/remote/cwd");
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
        let target = ssh_target("/remote/workspace/.paperclip-runtime/runs/run-ssh-resume/workspace");
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
            Some("12345678-1234-4abc-9def-123456789012"),
            Some(managed),
            Some(managed),
            true,
            None,
            None,
            None,
            None,
            0,
            Some(&saved),
            Some(&target),
        );
        assert!(allow);
        assert!(reason.is_none());
    }

    #[test]
    fn should_resume_remote_session_denies_non_uuid_session_id() {
        let target = ssh_target("/remote/workspace");
        let saved = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "username": "fixture",
            "port": 2222,
            "remoteCwd": "/remote/workspace",
        });
        let (allow, reason) = should_resume_remote_session(
            Some("not-a-uuid"),
            Some("/remote/workspace"),
            Some("/remote/workspace"),
            true,
            None,
            None,
            None,
            None,
            0,
            Some(&saved),
            Some(&target),
        );
        assert!(!allow);
        assert_eq!(reason, Some("session id is not a valid UUID"));
    }

    #[test]
    fn should_resume_remote_session_denies_prompt_bundle_mismatch() {
        let target = ssh_target("/remote/workspace");
        let saved = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "username": "fixture",
            "port": 2222,
            "remoteCwd": "/remote/workspace",
        });
        let (allow, reason) = should_resume_remote_session(
            Some("12345678-1234-4abc-9def-123456789012"),
            Some("/remote/workspace"),
            Some("/remote/workspace"),
            true,
            Some("bundle-a"),
            Some("bundle-b"),
            None,
            None,
            0,
            Some(&saved),
            Some(&target),
        );
        assert!(!allow);
        assert_eq!(reason, Some("prompt bundle key does not match"));
    }

    #[test]
    fn should_resume_remote_session_denies_mcp_mismatch() {
        let target = ssh_target("/remote/workspace");
        let saved = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "username": "fixture",
            "port": 2222,
            "remoteCwd": "/remote/workspace",
        });
        let (allow, reason) = should_resume_remote_session(
            Some("12345678-1234-4abc-9def-123456789012"),
            Some("/remote/workspace"),
            Some("/remote/workspace"),
            true,
            None,
            None,
            Some("mcp-1"),
            Some("mcp-2"),
            0,
            Some(&saved),
            Some(&target),
        );
        assert!(!allow);
        assert_eq!(reason, Some("MCP server identity does not match"));
    }

    #[test]
    fn should_resume_remote_session_denies_local_cwd_mismatch() {
        let target = ssh_target("/remote/workspace");
        let saved = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "username": "fixture",
            "port": 2222,
            "remoteCwd": "/remote/workspace",
        });
        let (allow, reason) = should_resume_remote_session(
            Some("12345678-1234-4abc-9def-123456789012"),
            Some("/local/other"),
            Some("/local/workspace"),
            false,
            None,
            None,
            None,
            None,
            0,
            Some(&saved),
            Some(&target),
        );
        assert!(!allow);
        assert_eq!(reason, Some("saved session cwd does not match execution cwd"));
    }

    #[test]
    fn should_resume_remote_session_denies_missing_saved_identity() {
        let target = ssh_target("/remote/workspace");
        let (allow, reason) = should_resume_remote_session(
            Some("12345678-1234-4abc-9def-123456789012"),
            Some("/remote/workspace"),
            Some("/remote/workspace"),
            true,
            None,
            None,
            None,
            None,
            0,
            None,
            Some(&target),
        );
        assert!(!allow);
        assert_eq!(reason, Some("no saved remote execution identity"));
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
            Some("12345678-1234-4abc-9def-123456789012"),
            Some("/remote/workspace"),
            Some("/remote/workspace"),
            true,
            None,
            None,
            None,
            None,
            0,
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
    fn is_valid_uuid_accepts_uuid_v4() {
        assert!(is_valid_uuid("12345678-1234-4abc-9def-123456789012"));
        assert!(is_valid_uuid("12345678-1234-4ABC-9DEF-123456789012"));
    }

    #[test]
    fn is_valid_uuid_rejects_invalid() {
        assert!(!is_valid_uuid(""));
        assert!(!is_valid_uuid("not-a-uuid"));
        assert!(!is_valid_uuid("12345678-1234-4abc-9def-12345678901"));
        assert!(!is_valid_uuid("12345678-1234-4abc-9def-1234567890123"));
        assert!(!is_valid_uuid("12345678-1234-4abc-9def-12345678901x"));
        assert!(!is_valid_uuid("123456781234-4abc-9def-123456789012"));
    }

    #[test]
    fn remote_env_replaces_workspace_cwd_true_for_matching_value() {
        assert!(remote_env_replaces_workspace_cwd(
            "/local/workspace",
            "/local/workspace",
            "/remote/workspace/.paperclip-runtime/runs/run-1/workspace"
        ));
    }

    #[test]
    fn remote_env_replaces_workspace_cwd_false_for_different_value() {
        assert!(!remote_env_replaces_workspace_cwd(
            "/other/dir",
            "/local/workspace",
            "/remote/workspace"
        ));
    }

    #[test]
    fn remote_env_replaces_workspace_cwd_false_for_blank_remote() {
        assert!(!remote_env_replaces_workspace_cwd(
            "/local/workspace",
            "/local/workspace",
            "  "
        ));
    }

    #[test]
    fn remote_env_replaces_workspace_cwd_false_for_blank_local() {
        assert!(!remote_env_replaces_workspace_cwd(
            "/local/workspace",
            "  ",
            "/remote/workspace"
        ));
    }

    #[test]
    fn remote_env_replaces_workspace_cwd_false_for_blank_value() {
        assert!(!remote_env_replaces_workspace_cwd(
            "   ",
            "/local/workspace",
            "/remote/workspace"
        ));
    }

    #[test]
    fn remote_sync_excludes_git_backed() {
        assert_eq!(remote_sync_excludes(true), &[".git", ".paperclip-runtime"]);
    }

    #[test]
    fn remote_sync_excludes_non_git() {
        assert_eq!(remote_sync_excludes(false), &[".paperclip-runtime"]);
    }

    #[test]
    fn decide_claude_bridge_plan_returns_none_for_local() {
        let target = pc_acpx::execution_target::parse_adapter_execution_target(
            &serde_json::json!({ "kind": "local" }),
        )
        .expect("local target");
        let plan = decide_claude_execution_bridge_plan(
            "run-1",
            Some(&target),
            None,
            None,
            Some("tok"),
            None,
        )
        .expect("no error");
        assert!(plan.is_none());
    }

    #[test]
    fn decide_claude_bridge_plan_assembles_remote_handle() {
        let target = pc_acpx::execution_target::adapter_execution_target_from_remote_execution(
            &serde_json::json!({
                "transport": "ssh",
                "host": "h",
                "username": "u",
                "remoteWorkspacePath": "/w",
                "remoteCwd": "/w",
                "port": 2222,
            }),
            None,
        )
        .expect("ssh target");
        let plan = decide_claude_execution_bridge_plan(
            "run-1",
            Some(&target),
            None,
            Some(30.0),
            Some("tok"),
            None,
        )
        .expect("no error")
        .expect("remote plan");
        assert_eq!(
            plan.env["PAPERCLIP_BRIDGE_QUEUE_DIR"],
            "/w/.paperclip-runtime/claude/paperclip-bridge/queue"
        );
        assert_eq!(plan.timeout_ms, Some(30_000));
        assert_eq!(plan.env["PAPERCLIP_API_KEY"], plan.bridge_token);
    }

    #[test]
    fn decide_claude_bridge_plan_errors_without_token() {
        let target = pc_acpx::execution_target::adapter_execution_target_from_remote_execution(
            &serde_json::json!({
                "transport": "ssh",
                "host": "h",
                "username": "u",
                "remoteWorkspacePath": "/w",
                "remoteCwd": "/w",
                "port": 2222,
            }),
            None,
        )
        .expect("ssh target");
        let error = decide_claude_execution_bridge_plan(
            "run-1",
            Some(&target),
            None,
            None,
            None,
            None,
        )
        .expect_err("token required");
        assert!(error.contains("Sandbox bridge mode requires"));
    }

    fn sandbox_target(remote_cwd: &str) -> AdapterExecutionTarget {
        pc_acpx::execution_target::parse_adapter_execution_target(&json!({
            "kind": "remote",
            "transport": "sandbox",
            "providerKey": "local-test",
            "remoteCwd": remote_cwd,
            "timeoutMs": 30_000,
        }))
        .expect("valid sandbox target")
    }

    #[test]
    fn use_remote_process_session_gate_matches_node() {
        let sandbox = sandbox_target("/sandbox/w");
        assert!(use_claude_remote_process_session(Some(&sandbox), true, true));
        assert!(!use_claude_remote_process_session(Some(&sandbox), false, true));
        assert!(!use_claude_remote_process_session(Some(&sandbox), true, false));
        let ssh = ssh_target("/remote/workspace");
        assert!(!use_claude_remote_process_session(Some(&ssh), true, true));
        assert!(!use_claude_remote_process_session(None, true, true));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn start_process_session_bridge_returns_none_without_runner() {
        let sandbox = sandbox_target("/sandbox/w");
        let target = serde_json::to_value(&sandbox).expect("sandbox json");
        let env = std::collections::BTreeMap::new();
        let bridge = start_claude_process_session_bridge(
            "run-493",
            Some(&target),
            None,
            "claude",
            "node /sandbox/w/child.mjs",
            "/sandbox/w",
            &env,
            Some(5.0),
            None,
            None,
        )
        .await
        .expect("gate returns Ok");
        assert!(bridge.is_none(), "no provider runner ⇒ no bridge");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn start_process_session_bridge_returns_none_for_ssh_target() {
        let ssh = ssh_target("/remote/workspace");
        let target = serde_json::to_value(&ssh).expect("ssh json");
        let env = std::collections::BTreeMap::new();
        let runner: std::sync::Arc<dyn pc_acpx::bridge_executor::BridgeCommandRunner> =
            std::sync::Arc::new(pc_acpx::bridge_executor::LocalProcessBridgeRunner);
        let bridge = start_claude_process_session_bridge(
            "run-493",
            Some(&target),
            None,
            "claude",
            "claude-acp",
            "/remote/workspace",
            &env,
            Some(5.0),
            Some(runner),
            None,
        )
        .await
        .expect("gate returns Ok");
        assert!(bridge.is_none(), "ssh transport ⇒ no process session bridge");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn start_process_session_bridge_returns_none_without_shell() {
        let sandbox = sandbox_target("/sandbox/w");
        let target = serde_json::to_value(&sandbox).expect("sandbox json");
        let env = std::collections::BTreeMap::new();
        let runner: std::sync::Arc<dyn pc_acpx::bridge_executor::BridgeCommandRunner> =
            std::sync::Arc::new(pc_acpx::bridge_executor::LocalProcessBridgeRunner);
        let bridge = start_claude_process_session_bridge(
            "run-493",
            Some(&target),
            None,
            "claude",
            "   ",
            "/sandbox/w",
            &env,
            Some(5.0),
            Some(runner),
            None,
        )
        .await
        .expect("gate returns Ok");
        assert!(bridge.is_none(), "empty agentCommandShell ⇒ no bridge");
    }
}
