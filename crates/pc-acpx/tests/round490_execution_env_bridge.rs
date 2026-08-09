//! R490 — 执行 env 与 bridge env 合并集成验证。
//!
//! 把 R486 的 bridge 计划决策接到主执行流程 env 构建（对齐 Node
//! codex execute.ts L891-907 / claude execute.ts L679-692）：
//! 1. 本地 target → 原样返回 base env
//! 2. 远程 + usesBridge → 合并 bridge 4 键 + 启动日志行
//! 3. 远程缺 PAPERCLIP_API_KEY → 报错
//! 4. host_api_url 从 base env 解析（PAPERCLIP_RUNTIME_API_URL 优先）

use pc_acpx::execution_target::*;
use std::collections::BTreeMap;

fn ssh_target_json(remote_cwd: &str) -> serde_json::Value {
    let target = adapter_execution_target_from_remote_execution(
        &serde_json::json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "port": 2222,
            "username": "fixture",
            "remoteWorkspacePath": remote_cwd,
            "remoteCwd": remote_cwd,
            "privateKey": "PRIVATE KEY",
            "knownHosts": "[127.0.0.1]:2222 ssh-ed25519 AAAA",
            "strictHostKeyChecking": true,
        }),
        None,
    )
    .expect("valid remote execution target");
    serde_json::to_value(target).expect("serialize target")
}

fn base_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PAPERCLIP_RUN_ID".to_string(), "run-1".to_string());
    env.insert("PAPERCLIP_API_KEY".to_string(), "host-token".to_string());
    env.insert(
        "PAPERCLIP_API_URL".to_string(),
        "http://host:3100".to_string(),
    );
    env.insert("CODEX_HOME".to_string(), "/home/codex".to_string());
    env
}

fn merge<'a>(
    base_env: &'a BTreeMap<String, String>,
    execution_target: Option<&'a serde_json::Value>,
    adapter_key: &'a str,
    timeout_sec: Option<f64>,
) -> Result<MergedExecutionEnv, String> {
    merge_execution_bridge_env(&MergeExecutionBridgeEnvInput {
        run_id: "run-1",
        base_env,
        execution_target,
        runtime_root_dir: None,
        adapter_key,
        timeout_sec,
        host_api_url: None,
    })
}

#[test]
fn local_target_passthrough() {
    let base = base_env();
    let merged = merge(
        &base,
        Some(&serde_json::json!({ "kind": "local" })),
        "codex",
        None,
    )
    .expect("local no error");
    assert_eq!(merged.env, base);
    assert!(merged.bridge_plan.is_none());
    assert!(merged.start_log_line.is_none());
}

#[test]
fn remote_merge_four_bridge_keys_with_log_line() {
    let base = base_env();
    let merged = merge(
        &base,
        Some(&ssh_target_json("/remote/workspace")),
        "codex",
        Some(45.0),
    )
    .expect("remote ok");
    let plan = merged.bridge_plan.as_ref().expect("plan present");
    assert_eq!(merged.env["PAPERCLIP_API_URL"], plan.host_api_url);
    assert_eq!(merged.env["PAPERCLIP_API_KEY"], plan.bridge_token);
    assert_eq!(merged.env["PAPERCLIP_API_BRIDGE_MODE"], "queue_v1");
    assert_eq!(
        merged.env["PAPERCLIP_BRIDGE_QUEUE_DIR"],
        plan.paths.queue_dir
    );
    assert_eq!(merged.env["CODEX_HOME"], "/home/codex");
    assert_eq!(merged.env.len(), 6);
    assert_eq!(plan.timeout_ms, Some(45_000));
    assert_eq!(
        merged.start_log_line.as_deref(),
        Some("[paperclip] Starting sandbox callback bridge for codex in /remote/workspace/.paperclip-runtime/codex/paperclip-bridge.\n")
    );
}

#[test]
fn remote_missing_token_errors() {
    let mut base = base_env();
    base.remove("PAPERCLIP_API_KEY");
    let error = merge(
        &base,
        Some(&ssh_target_json("/remote/workspace")),
        "claude",
        None,
    )
    .expect_err("token required");
    assert!(error.contains("Sandbox bridge mode requires"));
}

#[test]
fn host_api_url_prefers_runtime_api_url_from_base_env() {
    let mut base = base_env();
    base.insert(
        "PAPERCLIP_RUNTIME_API_URL".to_string(),
        "http://runtime:4000".to_string(),
    );
    let merged = merge(
        &base,
        Some(&ssh_target_json("/remote/workspace")),
        "claude",
        None,
    )
    .expect("remote ok");
    let plan = merged.bridge_plan.as_ref().expect("plan present");
    assert_eq!(plan.host_api_url, "http://runtime:4000");
    assert_eq!(merged.env["PAPERCLIP_API_URL"], "http://runtime:4000");
    assert_eq!(
        merged.start_log_line.as_deref(),
        Some("[paperclip] Starting sandbox callback bridge for claude in /remote/workspace/.paperclip-runtime/claude/paperclip-bridge.\n")
    );
}
