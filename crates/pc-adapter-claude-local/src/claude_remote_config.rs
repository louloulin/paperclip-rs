//! Claude 远程配置物化。
//!
//! 对齐 Node `claude-config.ts` 的
//! `buildRemoteClaudeConfigMaterializationCommand` 与
//! `materializeRemoteClaudeConfig`：先把 Paperclip 准备好的 config seed
//! 复制到运行级目录，再从远端 `$HOME/.claude` 补齐两种凭据文件，但不
//! 覆盖 seed 中已经存在的凭据。
//!
//! 命令规划与 I/O 分离：命令构造是纯函数，执行通过
//! [`pc_acpx::bridge_executor::BridgeCommandRunner`] 注入，SSH、sandbox
//! 或测试 runner 可以复用同一份业务语义。

use pc_acpx::bridge_executor::{
    require_successful_result, run_shell, BridgeCommandRunner, RunnerCommandResult,
};
use pc_acpx::execution_target::{AdapterExecutionTarget, AdapterRemoteExecutionTarget};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// 远程 Claude 配置物化输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteClaudeConfigMaterializationInput {
    /// runner 执行命令时使用的远端工作目录。
    pub remote_cwd: String,
    /// 本次运行实际使用的 `CLAUDE_CONFIG_DIR`。
    pub remote_claude_config_dir: String,
    /// 已同步到远端的 Paperclip config seed 目录。
    pub remote_claude_config_seed_dir: String,
    /// 命令环境；其中 `HOME` 用于远端 operator 凭据回退。
    pub env: BTreeMap<String, String>,
    /// 物化命令超时，毫秒。
    pub timeout_ms: u64,
}

/// 构造远程 Claude 配置物化 shell 命令。
///
/// 所有调用方路径均经过 POSIX 单引号转义；`HOME` 与 `file` 保留为远端
/// shell 变量，在目标机器上展开。
#[must_use]
pub fn build_remote_claude_config_materialization_command(
    remote_claude_config_dir: &str,
    remote_claude_config_seed_dir: &str,
) -> String {
    let config_dir = pc_acpx::ssh::shell_quote(remote_claude_config_dir);
    let seed_contents = format!("{}/.", remote_claude_config_seed_dir.trim_end_matches('/'));
    let seed_dir = pc_acpx::ssh::shell_quote(remote_claude_config_seed_dir);
    let seed_contents = pc_acpx::ssh::shell_quote(&seed_contents);

    format!(
        "mkdir -p {config_dir} && \
         if [ -d {seed_dir} ]; then cp -R {seed_contents} {config_dir}/; fi; \
         for file in .credentials.json credentials.json; do \
         if [ -n \"${{HOME:-}}\" ] && \
         [ -f \"${{HOME}}/.claude/${{file}}\" ] && \
         [ ! -f {config_dir}/\"${{file}}\" ]; then \
         cp \"${{HOME}}/.claude/${{file}}\" {config_dir}/\"${{file}}\"; \
         fi; done"
    )
}

/// 使用调用方提供的 runner 真实物化远程 Claude 配置。
///
/// 非零退出码和超时均转换为包含 stdout/stderr 的明确错误；成功时返回
/// 原始执行结果，供上层记录日志或诊断。
pub async fn materialize_remote_claude_config_with_runner(
    runner: Arc<dyn BridgeCommandRunner>,
    input: &RemoteClaudeConfigMaterializationInput,
) -> Result<RunnerCommandResult, String> {
    let command = build_remote_claude_config_materialization_command(
        &input.remote_claude_config_dir,
        &input.remote_claude_config_seed_dir,
    );
    let inherited_env: BTreeMap<String, String> = std::env::vars().collect();
    let env =
        pc_acpx::remote_execution_env::sanitize_remote_execution_env(&input.env, &inherited_env);
    let result = run_shell(
        &runner,
        &input.remote_cwd,
        &command,
        "sh",
        None,
        env,
        input.timeout_ms.max(1),
    )
    .await?;
    require_successful_result("materialize remote Claude config", &result)?;
    Ok(result)
}

/// 根据 execution target 选择远程配置物化 runner。
///
/// SSH 使用现有真实 `SshCommandManagedRuntimeRunner`；sandbox 必须由上层
/// 注入 provider runner，目前显式返回错误而不是静默跳过；local target
/// 不允许调用该远程 API。
pub async fn materialize_remote_claude_config_for_target(
    target: &AdapterExecutionTarget,
    input: &RemoteClaudeConfigMaterializationInput,
) -> Result<RunnerCommandResult, String> {
    match target {
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(ssh)) => {
            let runner: Arc<dyn BridgeCommandRunner> = Arc::new(
                pc_acpx::ssh::SshCommandManagedRuntimeRunner::new(ssh.spec.clone(), None, None),
            );
            materialize_remote_claude_config_with_runner(runner, input).await
        }
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(_)) => Err(
            "cannot materialize remote Claude config: sandbox provider runner is not configured"
                .to_string(),
        ),
        AdapterExecutionTarget::Local(_) => {
            Err("cannot materialize remote Claude config for a local execution target".to_string())
        }
    }
}

/// 将本地 Claude config seed 同步到 SSH 远端后再完成物化。
///
/// 空或不存在的本地 seed 不会阻止执行：远端命令仍会创建目标目录并
/// 尝试从远端 operator home 导入凭据，和 Node 的 `if [ -d seed ]` 语义一致。
pub async fn stage_and_materialize_remote_claude_config(
    target: &AdapterExecutionTarget,
    local_seed_dir: &Path,
    input: &RemoteClaudeConfigMaterializationInput,
) -> Result<RunnerCommandResult, String> {
    if let AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(ssh)) = target {
        if tokio::fs::metadata(local_seed_dir)
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
        {
            pc_acpx::git_workspace_sync::sync_directory_to_ssh(
                &ssh.spec,
                local_seed_dir,
                &input.remote_claude_config_seed_dir,
                None,
                false,
                None,
            )
            .await
            .map_err(|error| format!("sync Claude config seed: {error}"))?;
        }
    }
    materialize_remote_claude_config_for_target(target, input).await
}
