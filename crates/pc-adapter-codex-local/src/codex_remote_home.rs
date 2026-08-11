//! Codex 远程 managed home staging。
//!
//! 对齐 Node `codex-local/src/server/execute.ts` 的
//! `stageCodexHomeForSync` + `prepareRemoteManagedRuntime` home asset：只把
//! Codex 白名单文件上传到 SSH 远端，并返回运行级 `CODEX_HOME` 路径。
//! 本模块不负责 auth copy-back；那是下一层生命周期 hook 的职责。

use pc_acpx::execution_target::{AdapterExecutionTarget, AdapterRemoteExecutionTarget};
use std::path::{Path, PathBuf};

/// 远程 Codex home staging 输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRemoteCodexHomeInput {
    pub local_home: PathBuf,
    pub remote_home: String,
    pub run_id: String,
}

/// 远程 staging 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRemoteCodexHomeResult {
    pub remote_home: String,
}

/// 将 Codex managed home 的白名单内容真实同步到 SSH target。
///
/// 本地临时 staging 在上传结束后立即清理；远端 home 由当前 run 目录隔离，
/// 不会污染宿主机的完整 Codex home。
pub async fn stage_remote_codex_home_for_target(
    target: &AdapterExecutionTarget,
    input: &StageRemoteCodexHomeInput,
) -> Result<StageRemoteCodexHomeResult, String> {
    let AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(ssh)) = target else {
        return Err(
            "cannot stage Codex managed home: only SSH targets have a Rust staging runner"
                .to_string(),
        );
    };
    if !tokio::fs::metadata(&input.local_home)
        .await
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
    {
        return Err(format!(
            "cannot stage Codex managed home: local home does not exist: {}",
            input.local_home.display()
        ));
    }

    let staged_home = pc_adapter_codex_home_stage(input).await?;
    let sync_result = pc_acpx::git_workspace_sync::sync_directory_to_ssh(
        &ssh.spec,
        &staged_home,
        &input.remote_home,
        None,
        true,
        None,
    )
    .await;
    pc_adapter_codex_local_teardown(&staged_home).await;
    sync_result.map_err(|error| format!("sync remote Codex home: {error}"))?;

    Ok(StageRemoteCodexHomeResult {
        remote_home: input.remote_home.clone(),
    })
}

/// 从 SSH 远端运行级 Codex home 读取 auth.json，供 teardown copy-back 使用。
pub async fn read_remote_codex_auth(
    target: &AdapterExecutionTarget,
    remote_home: &str,
) -> std::io::Result<Vec<u8>> {
    let AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(ssh)) = target else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "remote Codex auth reader currently supports SSH targets only",
        ));
    };
    let auth_path = format!("{remote_home}/auth.json");
    match pc_acpx::ssh::run_ssh_command(
        &ssh.spec.as_connection_config(),
        &format!("cat {}", pc_acpx::ssh::shell_quote(&auth_path)),
        &pc_acpx::ssh::SshCommandOptions {
            env: std::collections::BTreeMap::new(),
            stdin: None,
            timeout_ms: 10_000,
            max_buffer: 1024 * 1024,
        },
    )
    .await
    {
        Ok(result) => Ok(result.stdout.into_bytes()),
        Err(error) if error.stderr.contains("No such file") => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "remote Codex auth.json is absent",
        )),
        Err(error) => Err(std::io::Error::other(format!(
            "read remote Codex auth.json: {error}"
        ))),
    }
}

async fn pc_adapter_codex_home_stage(input: &StageRemoteCodexHomeInput) -> Result<PathBuf, String> {
    pc_adapter_codex_local_stage(input)
        .await
        .map_err(|error| format!("stage Codex home: {error}"))
}

async fn pc_adapter_codex_local_stage(
    input: &StageRemoteCodexHomeInput,
) -> std::io::Result<PathBuf> {
    crate::codex_home_staging::stage_codex_home_for_sync(
        &input.local_home,
        crate::codex_home_staging::StageCodexHomeForSyncOptions {
            run_id: Some(input.run_id.clone()),
        },
    )
    .await
}

async fn pc_adapter_codex_local_teardown(path: &Path) {
    crate::codex_home_staging::teardown_staged_codex_home(path).await;
}
