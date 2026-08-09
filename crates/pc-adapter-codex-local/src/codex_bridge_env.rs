//! Codex paperclipBridge env 注入决策纯函数。
//!
//! 对齐 Node `codex-local/src/server/execute.ts` L891-907 的
//! `startAdapterExecutionTargetPaperclipBridge` 分支：
//!
//! ```ts
//! if (executionTargetIsRemote && adapterExecutionTargetUsesPaperclipBridge(runtimeExecutionTarget)) {
//!   paperclipBridge = await startAdapterExecutionTargetPaperclipBridge({
//!     runId,
//!     target: runtimeExecutionTarget,
//!     runtimeRootDir: preparedExecutionTargetRuntime?.runtimeRootDir,
//!     adapterKey: "codex",
//!     timeoutSec,
//!     hostApiToken: env.PAPERCLIP_API_KEY,
//!     onLog,
//!   });
//!   if (paperclipBridge) {
//!     Object.assign(env, paperclipBridge.env);
//!   }
//! }
//! ```
//!
//! # 设计范围
//!
//! 本模块只包含 **纯决策函数**，不启动真实 bridge server / worker：
//! - `should_start_paperclip_bridge` — 远程 target 才启动 bridge
//! - `resolve_bridge_runtime_root_dir` — runtimeRootDir 缺省回退到
//!   `<remoteCwd>/.paperclip-runtime/<adapterKey>`
//! - `resolve_bridge_host_api_url` — hostApiUrl > PAPERCLIP_RUNTIME_API_URL >
//!   PAPERCLIP_API_URL > 默认 URL
//! - `bridge_env_from_handle` — 注入 PAPERCLIP_API_URL / PAPERCLIP_API_KEY /
//!   PAPERCLIP_API_BRIDGE_MODE / PAPERCLIP_BRIDGE_QUEUE_DIR
//! - `merge_bridge_env` — Object.assign(env, paperclipBridge.env) 语义
//!
//! 真实 bridge server / worker（`startSandboxCallbackBridgeServer` /
//! `startSandboxCallbackBridgeWorker`）在 `pc-acpx::sandbox_callback_bridge`
//! 中已实现基础；route 层组合本模块的决策函数 + pc-acpx 执行器。

use pc_acpx::execution_target::{
    adapter_execution_target_remote_cwd, adapter_execution_target_uses_paperclip_bridge,
    AdapterExecutionTarget,
};
use std::collections::BTreeMap;
use std::sync::Arc;

/// 判定是否启动 paperclip bridge。
/// 对齐 Node `adapterExecutionTargetUsesPaperclipBridge(target)`：
/// 仅远程 target（SSH / Sandbox）返回 true。
#[must_use]
pub fn should_start_paperclip_bridge(target: Option<&AdapterExecutionTarget>) -> bool {
    adapter_execution_target_uses_paperclip_bridge(target)
}

/// 解析 bridge runtime root 目录。
///
/// `runtime_root_dir` 非空时原样返回（trim），否则回退到
/// `<remoteCwd>/.paperclip-runtime/<adapter_key>`（POSIX join）。
/// 对齐 Node `startAdapterExecutionTargetPaperclipBridge` 的
/// `path.posix.join(target.remoteCwd, ".paperclip-runtime", input.adapterKey)`。
#[must_use]
pub fn resolve_bridge_runtime_root_dir(
    runtime_root_dir: Option<&str>,
    target: Option<&AdapterExecutionTarget>,
    adapter_key: &str,
) -> String {
    if let Some(dir) = runtime_root_dir.map(str::trim).filter(|s| !s.is_empty()) {
        return dir.to_string();
    }
    let remote_cwd = adapter_execution_target_remote_cwd(target, "");
    let base = remote_cwd.trim_end_matches('/');
    format!("{base}/.paperclip-runtime/{adapter_key}")
}

/// 解析 bridge 转发的 host API URL。
///
/// 优先级：`host_api_url` > `PAPERCLIP_RUNTIME_API_URL` >
/// `PAPERCLIP_API_URL` > 默认 URL（`resolve_default_paperclip_api_url`）。
/// 对齐 Node L1759-1763。
#[must_use]
pub fn resolve_bridge_host_api_url(
    host_api_url: Option<&str>,
    runtime_api_url: Option<&str>,
    paperclip_api_url: Option<&str>,
) -> String {
    host_api_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            runtime_api_url
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            paperclip_api_url
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "http://localhost:3100".to_string())
}

/// 从 bridge handle 提取注入到子进程 env 的变量。
/// 对齐 Node `startAdapterExecutionTargetPaperclipBridge` 返回的
/// `{ env: { PAPERCLIP_API_URL, PAPERCLIP_API_KEY, PAPERCLIP_API_BRIDGE_MODE,
///           PAPERCLIP_BRIDGE_QUEUE_DIR } }`。
#[must_use]
pub fn bridge_env_from_handle(
    api_url: &str,
    bridge_token: &str,
    queue_dir: &str,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PAPERCLIP_API_URL".to_string(), api_url.to_string());
    env.insert("PAPERCLIP_API_KEY".to_string(), bridge_token.to_string());
    env.insert(
        "PAPERCLIP_API_BRIDGE_MODE".to_string(),
        "queue_v1".to_string(),
    );
    env.insert(
        "PAPERCLIP_BRIDGE_QUEUE_DIR".to_string(),
        queue_dir.to_string(),
    );
    env
}

/// 合并 bridge env 到子进程 env（Object.assign 语义：bridge env 覆盖）。
/// 对齐 Node `Object.assign(env, paperclipBridge.env)`。
pub fn merge_bridge_env(env: &mut BTreeMap<String, String>, bridge_env: &BTreeMap<String, String>) {
    for (key, value) in bridge_env {
        env.insert(key.clone(), value.clone());
    }
}

/// codex 主执行流程 bridge 计划决策（adapterKey 固定为 `"codex"`）。
/// 对齐 Node execute.ts L891-907：仅远程且 usesBridge 时启动；
/// host token 缺失时报错。
pub fn decide_codex_execution_bridge_plan(
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
        "codex",
        timeout_sec,
        env_paperclip_api_key,
        host_api_url,
    )
}

/// 启动 codex 执行的真实 paperclip bridge（R492，替换 R490 env-only 合并）。
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
pub async fn start_codex_execution_bridge(
    run_id: &str,
    base_env: &BTreeMap<String, String>,
    execution_target: Option<&serde_json::Value>,
    timeout_sec: Option<f64>,
    on_log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
) -> Result<Option<pc_acpx::bridge_executor::StartedAdapterBridge>, String> {
    let target =
        execution_target.and_then(pc_acpx::execution_target::parse_adapter_execution_target);
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
            adapter_key: "codex",
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
pub fn use_codex_remote_process_session(
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

/// 启动 codex 执行的 process session bridge（R493，对齐 Node execute.ts
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
pub async fn start_codex_process_session_bridge(
    run_id: &str,
    execution_target: Option<&serde_json::Value>,
    runtime_root_dir: Option<&str>,
    adapter_key: &str,
    agent_command_shell: &str,
    cwd: &str,
    launch_env: &BTreeMap<String, String>,
    timeout_sec: Option<f64>,
    runner: Option<Arc<dyn pc_acpx::bridge_executor::BridgeCommandRunner>>,
    on_log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
) -> Result<Option<pc_acpx::process_session_bridge::ProcessSessionBridgeHandle>, String> {
    let target =
        execution_target.and_then(pc_acpx::execution_target::parse_adapter_execution_target);
    let Some(runner) = runner else {
        return Ok(None);
    };
    let shell = agent_command_shell.trim();
    if !use_codex_remote_process_session(target.as_ref(), true, !shell.is_empty()) {
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
    fn should_start_paperclip_bridge_for_remote_ssh_target() {
        let target = ssh_target("/remote/workspace");
        assert!(should_start_paperclip_bridge(Some(&target)));
    }

    #[test]
    fn should_start_paperclip_bridge_false_for_local_target() {
        assert!(!should_start_paperclip_bridge(None));
    }

    #[test]
    fn resolve_bridge_runtime_root_dir_uses_provided_dir() {
        let target = ssh_target("/remote/workspace");
        assert_eq!(
            resolve_bridge_runtime_root_dir(
                Some("/remote/.paperclip-runtime/runs/run-1"),
                Some(&target),
                "codex"
            ),
            "/remote/.paperclip-runtime/runs/run-1"
        );
    }

    #[test]
    fn resolve_bridge_runtime_root_dir_falls_back_to_remote_cwd() {
        let target = ssh_target("/remote/workspace");
        assert_eq!(
            resolve_bridge_runtime_root_dir(None, Some(&target), "codex"),
            "/remote/workspace/.paperclip-runtime/codex"
        );
    }

    #[test]
    fn resolve_bridge_runtime_root_dir_handles_trailing_slash() {
        let target = ssh_target("/remote/workspace/");
        assert_eq!(
            resolve_bridge_runtime_root_dir(None, Some(&target), "claude"),
            "/remote/workspace/.paperclip-runtime/claude"
        );
    }

    #[test]
    fn resolve_bridge_host_api_url_prefers_host_api_url() {
        assert_eq!(
            resolve_bridge_host_api_url(
                Some("http://api.example.com"),
                Some("http://runtime.example.com"),
                Some("http://paperclip.example.com")
            ),
            "http://api.example.com"
        );
    }

    #[test]
    fn resolve_bridge_host_api_url_prefers_runtime_api_url() {
        assert_eq!(
            resolve_bridge_host_api_url(
                None,
                Some("http://runtime.example.com"),
                Some("http://paperclip.example.com")
            ),
            "http://runtime.example.com"
        );
    }

    #[test]
    fn resolve_bridge_host_api_url_prefers_paperclip_api_url() {
        assert_eq!(
            resolve_bridge_host_api_url(None, None, Some("http://paperclip.example.com")),
            "http://paperclip.example.com"
        );
    }

    #[test]
    fn resolve_bridge_host_api_url_defaults_to_localhost() {
        assert_eq!(
            resolve_bridge_host_api_url(None, None, None),
            "http://localhost:3100"
        );
    }

    #[test]
    fn resolve_bridge_host_api_url_trims_blank_values() {
        assert_eq!(
            resolve_bridge_host_api_url(Some("   "), Some("http://runtime.example.com"), None),
            "http://runtime.example.com"
        );
    }

    #[test]
    fn bridge_env_from_handle_injects_all_vars() {
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
    fn merge_bridge_env_overrides_existing_keys() {
        let mut env = BTreeMap::new();
        env.insert("PAPERCLIP_API_URL".to_string(), "old".to_string());
        env.insert("CODEX_HOME".to_string(), "/home/codex".to_string());
        let bridge_env =
            bridge_env_from_handle("http://127.0.0.1:4310", "bridge-token", "/bridge/queue");
        merge_bridge_env(&mut env, &bridge_env);
        assert_eq!(
            env.get("PAPERCLIP_API_URL").unwrap(),
            "http://127.0.0.1:4310"
        );
        assert_eq!(env.get("CODEX_HOME").unwrap(), "/home/codex");
        assert_eq!(env.len(), 5);
    }

    #[test]
    fn merge_bridge_env_preserves_non_conflicting_keys() {
        let mut env = BTreeMap::new();
        env.insert(
            "PAPERCLIP_WORKSPACE_CWD".to_string(),
            "/remote/workspace".to_string(),
        );
        let bridge_env =
            bridge_env_from_handle("http://127.0.0.1:4310", "bridge-token", "/bridge/queue");
        merge_bridge_env(&mut env, &bridge_env);
        assert_eq!(
            env.get("PAPERCLIP_WORKSPACE_CWD").unwrap(),
            "/remote/workspace"
        );
        assert_eq!(env.len(), 5);
    }

    #[test]
    fn decide_codex_bridge_plan_returns_none_for_local() {
        let target =
            pc_acpx::execution_target::parse_adapter_execution_target(&json!({ "kind": "local" }))
                .expect("local target");
        let plan = decide_codex_execution_bridge_plan(
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
    fn decide_codex_bridge_plan_assembles_remote_handle() {
        let target = adapter_execution_target_from_remote_execution(
            &json!({
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
        let plan = decide_codex_execution_bridge_plan(
            "run-1",
            Some(&target),
            Some("/runtime"),
            Some(45.0),
            Some("tok"),
            Some("http://host:3100"),
        )
        .expect("no error")
        .expect("remote plan");
        assert_eq!(plan.env["PAPERCLIP_API_KEY"], plan.bridge_token);
        assert_eq!(
            plan.env["PAPERCLIP_BRIDGE_QUEUE_DIR"],
            "/runtime/paperclip-bridge/queue"
        );
        assert_eq!(plan.env["PAPERCLIP_API_URL"], "http://host:3100");
        assert!(!plan.has_run_log_tail);
    }

    #[test]
    fn decide_codex_bridge_plan_errors_without_token() {
        let target = adapter_execution_target_from_remote_execution(
            &json!({
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
        let error =
            decide_codex_execution_bridge_plan("run-1", Some(&target), None, None, None, None)
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
        // Node gate：remote + sandbox + runner + agentCommandShell 全满足。
        assert!(use_codex_remote_process_session(Some(&sandbox), true, true));
        // runner 缺失 → false（Rust adapter 现状）。
        assert!(!use_codex_remote_process_session(
            Some(&sandbox),
            false,
            true
        ));
        // agentCommandShell 缺失 → false。
        assert!(!use_codex_remote_process_session(
            Some(&sandbox),
            true,
            false
        ));
        // SSH / 本地 → false。
        let ssh = ssh_target("/remote/workspace");
        assert!(!use_codex_remote_process_session(Some(&ssh), true, true));
        assert!(!use_codex_remote_process_session(None, true, true));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn start_process_session_bridge_returns_none_without_runner() {
        let sandbox = sandbox_target("/sandbox/w");
        let target = serde_json::to_value(&sandbox).expect("sandbox json");
        let env = BTreeMap::new();
        let bridge = start_codex_process_session_bridge(
            "run-493",
            Some(&target),
            None,
            "codex",
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
        let env = BTreeMap::new();
        let runner: Arc<dyn pc_acpx::bridge_executor::BridgeCommandRunner> =
            Arc::new(pc_acpx::bridge_executor::LocalProcessBridgeRunner);
        let bridge = start_codex_process_session_bridge(
            "run-493",
            Some(&target),
            None,
            "codex",
            "codex-acp",
            "/remote/workspace",
            &env,
            Some(5.0),
            Some(runner),
            None,
        )
        .await
        .expect("gate returns Ok");
        assert!(
            bridge.is_none(),
            "ssh transport ⇒ no process session bridge"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn start_process_session_bridge_returns_none_without_shell() {
        let sandbox = sandbox_target("/sandbox/w");
        let target = serde_json::to_value(&sandbox).expect("sandbox json");
        let env = BTreeMap::new();
        let runner: Arc<dyn pc_acpx::bridge_executor::BridgeCommandRunner> =
            Arc::new(pc_acpx::bridge_executor::LocalProcessBridgeRunner);
        let bridge = start_codex_process_session_bridge(
            "run-493",
            Some(&target),
            None,
            "codex",
            "  ",
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
