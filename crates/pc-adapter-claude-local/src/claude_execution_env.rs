//! claude 执行 env 构建决策（R490）。
//!
//! 对齐 Node `claude-local/src/server/execute.ts` L679-692：远程 +
//! usesBridge 时启动 paperclip bridge 并把 bridge env 合并进子进程 env
//! （`Object.assign(env, paperclipBridge.env)`）。
//!
//! # 设计范围
//!
//! 本模块只包含 **纯决策函数**：从 adapter context 提取输入，调用
//! `pc-acpx::execution_target::merge_execution_bridge_env` 得到合并后的
//! 执行 env 与 bridge 启动计划。不启动真实 bridge server / worker
//! （真实 I/O 执行器在 `pc-acpx::sandbox_callback_bridge`，后续轮次接入
//! route 层）。

use pc_acpx::execution_target::{
    merge_execution_bridge_env, MergeExecutionBridgeEnvInput, MergedExecutionEnv,
};
use std::collections::BTreeMap;

/// claude 执行 env 构建输入（从 adapter context 提取）。
#[derive(Debug, Clone)]
pub struct ClaudeExecutionEnvInput<'a> {
    /// 本次 run id（`context.run_id`）。
    pub run_id: &'a str,
    /// route 层构建好的基础执行 env（`context.env`）。
    pub base_env: &'a BTreeMap<String, String>,
    /// execution target JSON（`context.execution_target`）。
    pub execution_target: Option<&'a serde_json::Value>,
    /// bridge runtime root dir；None 时回退到
    /// `<remoteCwd>/.paperclip-runtime/claude`。
    pub runtime_root_dir: Option<&'a str>,
    /// 超时秒（adapterConfig.timeoutSec，>0 生效）。
    pub timeout_sec: Option<f64>,
}

/// 构建 claude 执行 env（adapterKey 固定 `"claude"`）。
///
/// - 本地 target → 原样返回 `base_env`，无 bridge
/// - 远程 + usesBridge → `base_env` 缺 `PAPERCLIP_API_KEY` 报错（Node
///   throw）；否则合并 bridge 的 4 个 env 键并生成启动日志行
pub fn build_claude_execution_env(
    input: &ClaudeExecutionEnvInput<'_>,
) -> Result<MergedExecutionEnv, String> {
    merge_execution_bridge_env(&MergeExecutionBridgeEnvInput {
        run_id: input.run_id,
        base_env: input.base_env,
        execution_target: input.execution_target,
        runtime_root_dir: input.runtime_root_dir,
        adapter_key: "claude",
        timeout_sec: input.timeout_sec,
        host_api_url: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_acpx::execution_target::adapter_execution_target_from_remote_execution;
    use serde_json::json;

    fn ssh_target_value(remote_cwd: &str) -> serde_json::Value {
        let target = adapter_execution_target_from_remote_execution(
            &json!({
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
        env.insert("PAPERCLIP_API_URL".to_string(), "http://host:3100".to_string());
        env.insert("CLAUDE_CONFIG_DIR".to_string(), "/home/claude/config".to_string());
        env
    }

    fn input<'a>(
        base_env: &'a BTreeMap<String, String>,
        execution_target: Option<&'a serde_json::Value>,
    ) -> ClaudeExecutionEnvInput<'a> {
        ClaudeExecutionEnvInput {
            run_id: "run-1",
            base_env,
            execution_target,
            runtime_root_dir: None,
            timeout_sec: None,
        }
    }

    #[test]
    fn local_target_returns_base_env_unchanged() {
        let base = base_env();
        let merged = build_claude_execution_env(&input(&base, Some(&json!({ "kind": "local" }))))
            .expect("local no error");
        assert_eq!(merged.env, base);
        assert!(merged.bridge_plan.is_none());
        assert!(merged.start_log_line.is_none());
    }

    #[test]
    fn missing_target_is_treated_as_local() {
        let base = base_env();
        let merged = build_claude_execution_env(&input(&base, None)).expect("no error");
        assert!(merged.bridge_plan.is_none());
        assert_eq!(merged.env, base);
    }

    #[test]
    fn remote_target_merges_four_bridge_keys() {
        let base = base_env();
        let merged = build_claude_execution_env(&input(
            &base,
            Some(&ssh_target_value("/remote/workspace")),
        ))
        .expect("remote ok");
        let plan = merged.bridge_plan.as_ref().expect("plan present");
        assert_eq!(merged.env["PAPERCLIP_API_URL"], plan.host_api_url);
        assert_eq!(merged.env["PAPERCLIP_API_KEY"], plan.bridge_token);
        assert_eq!(merged.env["PAPERCLIP_API_BRIDGE_MODE"], "queue_v1");
        assert_eq!(
            merged.env["PAPERCLIP_BRIDGE_QUEUE_DIR"],
            plan.paths.queue_dir
        );
        assert_eq!(merged.env["CLAUDE_CONFIG_DIR"], "/home/claude/config");
        assert_eq!(merged.env.len(), 6);
    }

    #[test]
    fn remote_target_without_token_errors() {
        let mut base = base_env();
        base.remove("PAPERCLIP_API_KEY");
        let error = build_claude_execution_env(&input(
            &base,
            Some(&ssh_target_value("/remote/workspace")),
        ))
        .expect_err("token required");
        assert!(error.contains("Sandbox bridge mode requires"));
    }

    #[test]
    fn remote_target_log_line_and_timeout() {
        let base = base_env();
        let merged = build_claude_execution_env(&ClaudeExecutionEnvInput {
            run_id: "run-1",
            base_env: &base,
            execution_target: Some(&ssh_target_value("/remote/workspace")),
            runtime_root_dir: Some("/runtime"),
            timeout_sec: Some(45.0),
        })
        .expect("remote ok");
        let plan = merged.bridge_plan.as_ref().expect("plan present");
        assert_eq!(plan.timeout_ms, Some(45_000));
        assert_eq!(
            merged.start_log_line.as_deref(),
            Some(
                "[paperclip] Starting sandbox callback bridge for claude in \
                 /runtime/paperclip-bridge.\n"
                    .trim_start()
            )
        );
    }
}
