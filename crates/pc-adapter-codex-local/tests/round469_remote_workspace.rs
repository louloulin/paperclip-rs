//! R469 — codex-local 远程 SSH workspace 决策端到端验证。
//!
//! 对齐 Node `codex-local/src/server/execute.remote.test.ts`：
//! 1. 受管远程运行目录计算
//! 2. 远程 Codex home 资产目录
//! 3. bridge env 注入（PAPERCLIP_API_URL / API_KEY / BRIDGE_MODE / QUEUE_DIR）
//! 4. 远程 session resume 决策（身份匹配 / 不匹配 / 本地执行）
//! 5. SSH 同步排除项（git 快照 / 非 git）

use pc_acpx::execution_target::adapter_execution_target_from_remote_execution;
use pc_adapter_codex_local::codex_bridge_env::{
    bridge_env_from_handle, merge_bridge_env, resolve_bridge_host_api_url,
    resolve_bridge_runtime_root_dir, should_start_paperclip_bridge,
};
use pc_adapter_codex_local::codex_remote_workspace::{
    codex_home_sync_allowlist, managed_remote_runtime_workspace_dir, remote_codex_home_dir,
    remote_sync_excludes, should_resume_remote_session,
};
use serde_json::json;
use std::collections::BTreeMap;

fn ssh_target(remote_cwd: &str) -> serde_json::Value {
    json!({
        "transport": "ssh",
        "host": "127.0.0.1",
        "port": 2222,
        "username": "fixture",
        "remoteWorkspacePath": "/remote/workspace",
        "remoteCwd": remote_cwd,
        "privateKey": "PRIVATE KEY",
        "knownHosts": "[127.0.0.1]:2222 ssh-ed25519 AAAA",
        "strictHostKeyChecking": true,
    })
}

fn parsed_target(remote_cwd: &str) -> pc_acpx::execution_target::AdapterExecutionTarget {
    adapter_execution_target_from_remote_execution(&ssh_target(remote_cwd), None)
        .expect("valid remote execution target")
}

#[test]
fn managed_remote_workspace_matches_node_scenario() {
    // Node 测试场景：/remote/workspace/.paperclip-runtime/runs/run-1/workspace
    assert_eq!(
        managed_remote_runtime_workspace_dir("/remote/workspace", "run-1"),
        "/remote/workspace/.paperclip-runtime/runs/run-1/workspace"
    );
}

#[test]
fn remote_codex_home_matches_node_scenario() {
    // Node 测试场景：${managedRemoteWorkspace}/.paperclip-runtime/codex/home
    let managed = managed_remote_runtime_workspace_dir("/remote/workspace", "run-1");
    assert_eq!(
        remote_codex_home_dir(&managed),
        "/remote/workspace/.paperclip-runtime/runs/run-1/workspace/.paperclip-runtime/codex/home"
    );
}

#[test]
fn bridge_env_injection_matches_node_handle() {
    // Node startAdapterExecutionTargetPaperclipBridge 返回 env：
    // PAPERCLIP_API_URL=http://127.0.0.1:4310
    // PAPERCLIP_API_KEY=bridge-token
    // PAPERCLIP_API_BRIDGE_MODE=queue_v1
    let env = bridge_env_from_handle("http://127.0.0.1:4310", "bridge-token", "/bridge/queue");
    assert_eq!(
        env.get("PAPERCLIP_API_URL").unwrap(),
        "http://127.0.0.1:4310"
    );
    assert_eq!(env.get("PAPERCLIP_API_KEY").unwrap(), "bridge-token");
    assert_eq!(env.get("PAPERCLIP_API_BRIDGE_MODE").unwrap(), "queue_v1");
    assert_eq!(
        env.get("PAPERCLIP_BRIDGE_QUEUE_DIR").unwrap(),
        "/bridge/queue"
    );
}

#[test]
fn bridge_env_merged_into_process_env_matches_object_assign() {
    let mut env = BTreeMap::new();
    env.insert("CODEX_HOME".to_string(), "/home/codex".to_string());
    env.insert(
        "PAPERCLIP_WORKSPACE_CWD".to_string(),
        "/remote/workspace".to_string(),
    );
    let bridge_env =
        bridge_env_from_handle("http://127.0.0.1:4310", "bridge-token", "/bridge/queue");
    merge_bridge_env(&mut env, &bridge_env);
    assert_eq!(
        env.get("PAPERCLIP_API_URL").unwrap(),
        "http://127.0.0.1:4310"
    );
    assert_eq!(env.get("CODEX_HOME").unwrap(), "/home/codex");
    assert_eq!(
        env.get("PAPERCLIP_WORKSPACE_CWD").unwrap(),
        "/remote/workspace"
    );
    assert_eq!(env.len(), 6);
}

#[test]
fn should_start_bridge_for_remote_ssh_target() {
    let target = parsed_target("/remote/workspace");
    assert!(should_start_paperclip_bridge(Some(&target)));
}

#[test]
fn bridge_runtime_root_defaults_to_remote_cwd_runtime() {
    let target = parsed_target("/remote/workspace");
    assert_eq!(
        resolve_bridge_runtime_root_dir(None, Some(&target), "codex"),
        "/remote/workspace/.paperclip-runtime/codex"
    );
}

#[test]
fn bridge_host_api_url_prefers_runtime_env() {
    // Node 优先级：hostApiUrl > PAPERCLIP_RUNTIME_API_URL > PAPERCLIP_API_URL
    assert_eq!(
        resolve_bridge_host_api_url(None, Some("http://runtime:4310"), Some("http://pc:3100")),
        "http://runtime:4310"
    );
}

#[test]
fn resume_denied_for_remote_without_matching_identity() {
    // Node 测试："does not resume saved Codex sessions for remote SSH execution
    // without a matching remote identity"（sessionParams.cwd=/remote/workspace，
    // 无 remoteExecution 身份 → 不匹配）
    let target = parsed_target("/remote/workspace");
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
fn resume_allowed_for_remote_with_matching_identity() {
    // Node 测试："resumes saved Codex sessions for remote SSH execution when
    // the remote identity matches"（sessionParams.remoteExecution 4-tuple）
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
        Some("session-123"),
        Some(managed.as_str()),
        Some(managed.as_str()),
        Some(&saved),
        Some(&target),
    );
    assert!(allow);
    assert!(reason.is_none());
}

#[test]
fn codex_home_sync_allowlist_matches_node() {
    // Node CODEX_SYNC_ALLOWLIST = COPIED_SHARED_FILES + SYMLINKED_SHARED_FILES + skills
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
fn remote_sync_excludes_match_prepare_workspace() {
    // Node：git 快照 → [".git", ".paperclip-runtime"]；非 git → [".paperclip-runtime"]
    assert_eq!(remote_sync_excludes(true), &[".git", ".paperclip-runtime"]);
    assert_eq!(remote_sync_excludes(false), &[".paperclip-runtime"]);
}
