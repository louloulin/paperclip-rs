//! R469 — claude-local 远程 SSH workspace 决策端到端验证。
//!
//! 对齐 Node `claude-local/src/server/execute.remote.test.ts` 3 个场景：
//! 1. 受管远程运行目录 + env 重写（QA_PROJECT_WORKSPACE_CWD → managedRemoteWorkspace）
//! 2. 远程 session resume 决策（身份不匹配 → 拒绝；身份匹配 → 允许）
//! 3. bridge env 注入 + runtime root 解析

use pc_acpx::execution_target::adapter_execution_target_from_remote_execution;
use pc_adapter_claude_local::claude_remote_workspace::{
    managed_remote_runtime_workspace_dir, remote_env_replaces_workspace_cwd, remote_sync_excludes,
    resolve_remote_workspace_dir, should_resume_remote_session,
};
use serde_json::json;

fn parsed_target(remote_cwd: &str) -> pc_acpx::execution_target::AdapterExecutionTarget {
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
    adapter_execution_target_from_remote_execution(&value, None).expect("valid ssh target")
}

const UUID_V4: &str = "12345678-1234-4abc-9def-123456789012";

#[test]
fn managed_remote_workspace_matches_node_scenario() {
    // Node 测试场景：/remote/workspace/.paperclip-runtime/runs/run-1/workspace
    assert_eq!(
        managed_remote_runtime_workspace_dir("/remote/workspace", "run-1"),
        "/remote/workspace/.paperclip-runtime/runs/run-1/workspace"
    );
}

#[test]
fn remote_workspace_dir_falls_back_to_remote_cwd() {
    assert_eq!(
        resolve_remote_workspace_dir(None, "/remote/workspace"),
        "/remote/workspace"
    );
}

#[test]
fn env_cwd_rewrite_rewrites_matching_workspace_cwd() {
    // Node：QA_PROJECT_WORKSPACE_CWD=workspaceDir（本地）→ managedRemoteWorkspace
    // OTHER_ENV=workspaceDir 但键名不以 _WORKSPACE_CWD 结尾 → 不重写
    let managed = managed_remote_runtime_workspace_dir("/remote/workspace", "run-1");
    assert!(remote_env_replaces_workspace_cwd(
        "/local/workspace",
        "/local/workspace",
        &managed
    ));
    assert!(!remote_env_replaces_workspace_cwd(
        "/other/dir",
        "/local/workspace",
        &managed
    ));
}

#[test]
fn resume_denied_for_remote_without_matching_identity() {
    // Node 测试 2："does not resume saved Claude sessions for remote SSH
    // execution without a matching remote identity"
    let target = parsed_target("/remote/workspace");
    let (allow, reason) = should_resume_remote_session(
        Some(UUID_V4),
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
fn resume_allowed_for_remote_with_matching_identity() {
    // Node 测试 3："resumes saved Claude sessions for remote SSH execution
    // when the remote identity matches"
    let managed = managed_remote_runtime_workspace_dir("/remote/workspace", "run-ssh-resume");
    let target = parsed_target(&managed);
    let saved = json!({
        "transport": "ssh",
        "host": "127.0.0.1",
        "port": 2222,
        "username": "fixture",
        "remoteCwd": managed,
    });
    let (allow, reason) = should_resume_remote_session(
        Some(UUID_V4),
        Some(managed.as_str()),
        Some(managed.as_str()),
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
fn remote_sync_excludes_match_prepare_workspace() {
    assert_eq!(remote_sync_excludes(true), &[".git", ".paperclip-runtime"]);
    assert_eq!(remote_sync_excludes(false), &[".paperclip-runtime"]);
}
