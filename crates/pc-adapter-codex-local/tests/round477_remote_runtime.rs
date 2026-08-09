//! R477 — codex-local 远程受管运行时全链路端到端验证。
//!
//! 把 R469-R476 模块串成 Node `codex-local/src/server/execute.ts` 的
//! 远程执行流程：
//! 1. 布局决策（prepareRemoteManagedRuntime）：workspaceRemoteDir /
//!    runtimeRootDir / home asset 目录
//! 2. bridge env 注入（startAdapterExecutionTargetPaperclipBridge）
//! 3. sessionParams 序列化（resolvedSessionParams 含 remoteExecution）
//! 4. resume 决策（canResumeSession + forceFreshSession + 日志）
//! 5. env 重写（PAPERCLIP_WORKSPACE_CWD → managedRemoteWorkspace）

use pc_acpx::execution_target::adapter_execution_target_from_remote_execution;
use pc_acpx::remote_managed_runtime::prepare_remote_managed_runtime_layout;
use pc_adapter_codex_local::codex_bridge_env::{
    bridge_env_from_handle, resolve_bridge_host_api_url, resolve_bridge_runtime_root_dir,
    should_start_paperclip_bridge,
};
use pc_adapter_codex_local::codex_remote_workspace::{
    managed_remote_runtime_workspace_dir, remote_codex_home_dir,
};
use pc_adapter_codex_local::codex_session_params::build_resolved_session_params;
use pc_adapter_codex_local::codex_session_resume::decide_codex_session_resume;
use serde_json::json;

fn ssh_target(remote_cwd: &str) -> pc_acpx::execution_target::AdapterExecutionTarget {
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

fn home_asset() -> pc_acpx::remote_managed_runtime::RemoteManagedRuntimeAsset {
    pc_acpx::remote_managed_runtime::RemoteManagedRuntimeAsset {
        key: "home".to_string(),
        local_dir: "/home/user/codex-home".to_string(),
        follow_symlinks: true,
        exclude: None,
        restore: true,
    }
}

#[test]
fn full_remote_layout_flow_matches_node_scenario() {
    // Node 测试 1：prepares the workspace, syncs CODEX_HOME, restores changes
    let managed = managed_remote_runtime_workspace_dir("/remote/workspace", "run-1");
    let layout = prepare_remote_managed_runtime_layout(
        "/remote/workspace",
        "run-1",
        "codex",
        true,
        &[home_asset()],
        &[],
    );
    assert_eq!(layout.workspace_remote_dir, managed);
    assert_eq!(
        layout.runtime_root_dir,
        format!("{managed}/.paperclip-runtime/codex")
    );
    assert_eq!(
        layout.asset_dirs.get("home").unwrap(),
        &remote_codex_home_dir(&managed)
    );
}

#[test]
fn bridge_env_flow_injects_into_execution_env() {
    // Node 测试 1 断言：PAPERCLIP_API_URL=http://127.0.0.1:4310,
    // PAPERCLIP_API_BRIDGE_MODE=queue_v1
    let target = ssh_target("/remote/workspace");
    assert!(should_start_paperclip_bridge(Some(&target)));
    let runtime_root = resolve_bridge_runtime_root_dir(None, Some(&target), "codex");
    assert_eq!(runtime_root, "/remote/workspace/.paperclip-runtime/codex");
    let host_api_url = resolve_bridge_host_api_url(None, None, Some("http://127.0.0.1:4310"));
    let bridge_env = bridge_env_from_handle(&host_api_url, "bridge-token", "/bridge/queue");
    assert_eq!(
        bridge_env.get("PAPERCLIP_API_URL").unwrap(),
        "http://127.0.0.1:4310"
    );
    assert_eq!(
        bridge_env.get("PAPERCLIP_API_BRIDGE_MODE").unwrap(),
        "queue_v1"
    );
}

#[test]
fn session_params_serialization_includes_remote_identity() {
    // Node：resolvedSessionParams 在远程时含 remoteExecution identity
    let managed = managed_remote_runtime_workspace_dir("/remote/workspace", "run-1");
    let identity = pc_acpx::execution_target::adapter_execution_target_session_identity(Some(
        &ssh_target(&managed),
    ));
    let params = build_resolved_session_params(
        &pc_adapter_codex_local::codex_session_params::ResolvedSessionParamsInput {
            session_id: Some("session-123"),
            cwd: &managed,
            execution_target_is_remote: true,
            remote_execution_identity: identity.map(serde_json::to_value).transpose().unwrap(),
            workspace_id: Some("workspace-1"),
            repo_url: Some("https://github.com/paperclipai/paperclip.git"),
            repo_ref: Some("main"),
        },
    )
    .unwrap();
    assert_eq!(
        pc_adapter_codex_local::codex_session_params::session_params_session_id(&params),
        Some("session-123")
    );
    assert_eq!(
        pc_adapter_codex_local::codex_session_params::session_params_cwd(&params),
        Some(managed.as_str())
    );
    let remote =
        pc_adapter_codex_local::codex_session_params::session_params_remote_execution(&params)
            .unwrap();
    assert_eq!(
        remote.get("host").and_then(serde_json::Value::as_str),
        Some("127.0.0.1")
    );
    assert_eq!(
        remote.get("port").and_then(serde_json::Value::as_u64),
        Some(2222)
    );
}

#[test]
fn resume_flow_allows_when_identity_matches() {
    // Node 测试 4：resumes saved Codex sessions when the remote identity matches
    let managed = managed_remote_runtime_workspace_dir("/remote/workspace", "run-ssh-resume");
    let target = ssh_target(&managed);
    let saved = json!({
        "transport": "ssh",
        "host": "127.0.0.1",
        "port": 2222,
        "username": "fixture",
        "remoteCwd": managed,
    });
    let input = pc_adapter_codex_local::codex_session_resume::CodexSessionResumeInput {
        runtime_session_id: "session-123",
        runtime_session_cwd: &managed,
        runtime_remote_execution: Some(&saved),
        effective_execution_cwd: &managed,
        execution_target_is_remote: true,
        execution_target: Some(&target),
        force_fresh_session: false,
    };
    let decision = decide_codex_session_resume(&input);
    assert!(decision.can_resume);
    assert_eq!(decision.session_id.as_deref(), Some("session-123"));
    assert!(decision.log_lines.is_empty());
}

#[test]
fn resume_flow_denies_without_identity() {
    // Node 测试 3：does not resume without a matching remote identity
    let target = ssh_target("/remote/workspace");
    let input = pc_adapter_codex_local::codex_session_resume::CodexSessionResumeInput {
        runtime_session_id: "session-123",
        runtime_session_cwd: "/remote/workspace",
        runtime_remote_execution: None,
        effective_execution_cwd: "/remote/workspace",
        execution_target_is_remote: true,
        execution_target: Some(&target),
        force_fresh_session: false,
    };
    let decision = decide_codex_session_resume(&input);
    assert!(!decision.can_resume);
    assert_eq!(decision.session_id, None);
    assert_eq!(decision.log_lines.len(), 1);
    assert!(decision.log_lines[0].contains("does not match the current remote execution identity"));
}

#[test]
fn env_cwd_rewrite_uses_managed_remote_dir() {
    // Node：PAPERCLIP_WORKSPACE_CWD → managedRemoteWorkspace
    let managed = managed_remote_runtime_workspace_dir("/remote/workspace", "run-1");
    assert_eq!(
        managed,
        "/remote/workspace/.paperclip-runtime/runs/run-1/workspace"
    );
    // PAPERCLIP_WORKSPACE_WORKTREE_PATH 在远程被清除（shape 函数语义）
    assert!(remote_codex_home_dir(&managed).ends_with(".paperclip-runtime/codex/home"));
}
