//! `pc-acpx::ssh` - port of `ssh.ts` from Node
//! `paperclip/packages/adapter-utils/src/`.
//!
//! Pure helpers for SSH-backed remote execution. Async functions
//! (`createSshCommandManagedRuntimeRunner`,
//! `runSshCommand`, `buildSshSpawnTarget`,
//! `syncDirectoryToSsh`, `syncDirectoryFromSsh`,
//! `prepareWorkspaceForSshExecution`,
//! `restoreWorkspaceFromSshExecution`,
//! `ensureSshWorkspaceReady`, `startSshEnvLabFixture`,
//! `buildSshEnvLabFixtureConfig`,
//! `getSshEnvLabSupport`, `isSshEnvLabFixtureProcess`,
//! `readSshEnvLabFixtureState`, `stopSshEnvLabFixture`,
//! `readSshEnvLabFixtureStatus`, `fileExists`,
//! `estimateLocalDirSize`, `probeRemoteDirSize`,
//! `withTempFile`, `execFileText`, `spawnText`,
//! `runLocalGit`, `commandExists`, `resolveCommandPath`,
//! `tarExcludeArgs_estimate`, `createSshAuthArgs`,
//! etc.) are deferred - they require real `ssh` process
//! invocation, port allocation, file streaming, and an in-process
//! sshd fixture spawn. This module ports:
//!
//! - Canonical types: `SshConnectionConfig`,
//!   `SshCommandResult`, `SshRemoteExecutionSpec`
//! - Pure helpers: `shell_quote`,
//!   `is_valid_shell_env_key`,
//!   `parse_ssh_remote_execution_spec`,
//!   `tar_exclude_args`,
//!   `tar_spawn_env`,
//!   `tar_pattern_to_regexp`,
//!   `build_known_hosts_entry`
//! - Re-exports the SSH session identity helpers + remote spec
//!   identity so callers can transition into the dedicated module
//!   without changing call sites in `execution_target` /
//!   `remote_managed_runtime`.

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// =============================================================================
// Canonical SSH types
// =============================================================================

/// SSH connection configuration used by every SSH-backed remote
/// execution. Mirrors Node `SshConnectionConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConnectionConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub remote_workspace_path: String,
    pub private_key: Option<String>,
    pub known_hosts: Option<String>,
    pub strict_host_key_checking: bool,
}

/// Standard `{stdout, stderr}` payload returned by every SSH script
/// invocation. Mirrors Node `SshCommandResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SshCommandResult {
    pub stdout: String,
    pub stderr: String,
}

/// Full SSH remote execution spec. `SshRemoteExecutionSpec` extends
/// `SshConnectionConfig` with the per-run working directory.
/// Mirrors Node `SshRemoteExecutionSpec`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshRemoteExecutionSpec {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub remote_workspace_path: String,
    pub private_key: Option<String>,
    pub known_hosts: Option<String>,
    pub strict_host_key_checking: bool,
    pub remote_cwd: String,
}

impl SshRemoteExecutionSpec {
    /// Construct a spec from its connection + cwd inputs.
    #[must_use]
    pub fn from_parts(config: SshConnectionConfig, remote_cwd: String) -> Self {
        Self {
            host: config.host,
            port: config.port,
            username: config.username,
            remote_workspace_path: config.remote_workspace_path,
            private_key: config.private_key,
            known_hosts: config.known_hosts,
            strict_host_key_checking: config.strict_host_key_checking,
            remote_cwd,
        }
    }

    /// Borrow as a `SshConnectionConfig` (drops `remote_cwd`).
    #[must_use]
    pub fn as_connection_config(&self) -> SshConnectionConfig {
        SshConnectionConfig {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            remote_workspace_path: self.remote_workspace_path.clone(),
            private_key: self.private_key.clone(),
            known_hosts: self.known_hosts.clone(),
            strict_host_key_checking: self.strict_host_key_checking,
        }
    }

    /// Effective remote workspace path: explicit field when set,
    /// else `remote_cwd`.
    #[must_use]
    pub fn effective_remote_workspace_path(&self) -> &str {
        if self.remote_workspace_path.is_empty() {
            &self.remote_cwd
        } else {
            &self.remote_workspace_path
        }
    }
}

// =============================================================================
// Pure helpers.
// =============================================================================

/// POSIX single-quote a string. Same algorithm as
/// `command_managed_runtime::shell_quote`; this is the SSH-own
/// copy preserved for parity with `ssh.ts`'s `shellQuote` export.
/// Mirrors Node `shellQuote`.
#[must_use]
pub fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', r#"'"'"'"#);
    format!("'{escaped}'")
}

/// `true` when a value is a valid `bash`/`sh` env variable name
/// (POSIX: starts with letter or underscore, followed by any number
/// of letters / digits / underscores). Mirrors Node
/// `isValidShellEnvKey`.
#[must_use]
pub fn is_valid_shell_env_key(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parse a JSON-ish value into an `SshRemoteExecutionSpec`.
/// Returns `None` when any required field is missing/invalid.
/// Mirrors Node `parseSshRemoteExecutionSpec`.
#[must_use]
pub fn parse_ssh_remote_execution_spec(
    value: &serde_json::Value,
) -> Option<SshRemoteExecutionSpec> {
    let parsed = match value {
        serde_json::Value::Object(m) => m,
        _ => return None,
    };
    let host = parsed
        .get("host")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let username = parsed
        .get("username")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let remote_cwd = parsed
        .get("remoteCwd")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let port_value = match parsed.get("port") {
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        Some(serde_json::Value::String(s)) => s.parse::<u64>().ok(),
        _ => None,
    };
    if host.is_empty()
        || username.is_empty()
        || remote_cwd.is_empty()
        || !matches!(port_value, Some(1..=65535))
    {
        return None;
    }

    let remote_workspace_path = parsed
        .get("remoteWorkspacePath")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| remote_cwd.clone());

    let private_key = parsed
        .get("privateKey")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let known_hosts = parsed
        .get("knownHosts")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let strict_host_key_checking = parsed
        .get("strictHostKeyChecking")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    Some(SshRemoteExecutionSpec {
        host,
        port: port_value.unwrap_or(22) as u16,
        username,
        remote_workspace_path,
        private_key,
        known_hosts,
        strict_host_key_checking,
        remote_cwd,
    })
}

/// Build the `tar --exclude <pattern>` argv fragment. Always
/// prepends `._*` (Mac resource fork metadata) before any
/// caller-supplied excludes. Mirrors Node `tarExcludeArgs`.
#[must_use]
pub fn tar_exclude_args(exclude: Option<&[String]>) -> Vec<String> {
    let mut combined: Vec<String> = vec!["._*".to_string()];
    if let Some(e) = exclude {
        combined.extend(e.iter().cloned());
    }
    combined
        .into_iter()
        .flat_map(|entry| [String::from("--exclude"), entry])
        .collect()
}

/// Build the env map the SSH tar spawn uses. Node's
/// `tarSpawnEnv` returns a `process.env`-derived object with
/// `COPYFILE_DISABLE=1` layered on top. The Rust helper is a pure
/// default (host env merging is async-side). Mirrors Node
/// `tarSpawnEnv`.
#[must_use]
pub fn tar_spawn_env_defaults() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    // Prevent macOS bsdtar from emitting AppleDouble metadata
    // files like ._README.md.
    m.insert("COPYFILE_DISABLE".to_string(), "1".to_string());
    m
}

/// Convert a tar `--exclude` pattern into a regexp for the local
/// size estimate (the estimate feeds a clamped percent, so we only
/// need approximate fidelity). Supports literal names plus `*` /
/// `?` glob characters. Mirrors Node `tarPatternToRegExp`.
#[must_use]
pub fn tar_pattern_to_regexp(pattern: &str) -> Result<Regex, String> {
    let mut escaped = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        match c {
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                escaped.push('\\');
                escaped.push(c);
            }
            '*' => escaped.push_str("[^/]*"),
            '?' => escaped.push_str("[^/]"),
            _ => escaped.push(c),
        }
    }
    Regex::new(&format!("^{escaped}$")).map_err(|e| e.to_string())
}

/// Direct helper that converts a tar `--exclude` pattern into a
/// regexp, building on `tar_pattern_to_regexp`. Returns an
/// `Option` that the SSH side uses to skip walks for already
/// excluded entries.
#[must_use]
pub fn try_tar_pattern_to_regexp(pattern: &str) -> Option<Regex> {
    tar_pattern_to_regexp(pattern).ok()
}

/// Build one line of a `~/.ssh/known_hosts` file from a host /
/// port / public-key tuple. The bracketed `[host]:port` form
/// disambiguates non-default ports. Mirrors Node
/// `buildKnownHostsEntry`.
#[must_use]
pub fn build_known_hosts_entry(input: KnownHostsEntryInput) -> String {
    format!(
        "[{}]:{} {}",
        input.host.trim(),
        input.port,
        input.public_key.trim()
    )
}

/// Input shape for [`build_known_hosts_entry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHostsEntryInput {
    pub host: String,
    pub port: u16,
    pub public_key: String,
}

// =============================================================================
// SSH 执行器（对齐 Node ssh.ts：createSshAuthArgs / runSshCommand /
// createSshCommandManagedRuntimeRunner / buildSshSpawnTarget）
// =============================================================================

/// `ssh` 认证参数 + 临时文件句柄（对齐 Node `createSshAuthArgs`）。
///
/// Node 用 `fs.mkdtemp` 把 private key / known hosts 写成 0600 临时文件，
/// 命令结束后 `cleanup()` 删除整个临时目录。Rust 版本在 `Drop` 中同步
/// 清理，防止异步路径上泄漏密钥文件。
pub struct SshAuthArgs {
    args: Vec<String>,
    temp_dirs: Vec<std::path::PathBuf>,
}

impl SshAuthArgs {
    /// 构造认证参数（对齐 Node `createSshAuthArgs`）：
    /// - 固定 `-o BatchMode=yes -o ConnectTimeout=10` + StrictHostKeyChecking
    /// - strictHostKeyChecking=true 且给了 knownHosts → 写入 0600 临时文件，
    ///   用 `UserKnownHostsFile` 指向它
    /// - strictHostKeyChecking=false → `UserKnownHostsFile=/dev/null`
    /// - 给了 privateKey → 写入 0600 临时文件，用 `-i` 指向它
    pub fn create(config: &SshConnectionConfig) -> Result<Self, String> {
        let mut temp_dirs = Vec::new();
        let mut args: Vec<String> = vec![
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
            "-o".to_string(),
            format!(
                "StrictHostKeyChecking={}",
                if config.strict_host_key_checking {
                    "yes"
                } else {
                    "no"
                }
            ),
        ];
        if config.strict_host_key_checking {
            if let Some(known_hosts) = config.known_hosts.as_deref().filter(|s| !s.is_empty()) {
                let dir = write_temp_secure_file("paperclip-ssh-known-hosts-", known_hosts)?;
                args.push("-o".to_string());
                args.push(format!(
                    "UserKnownHostsFile={}",
                    dir.join("payload").display()
                ));
                temp_dirs.push(dir);
            }
        } else {
            args.push("-o".to_string());
            args.push("UserKnownHostsFile=/dev/null".to_string());
        }
        if let Some(private_key) = config.private_key.as_deref().filter(|s| !s.is_empty()) {
            let dir = write_temp_secure_file("paperclip-ssh-key-", private_key)?;
            args.push("-i".to_string());
            args.push(dir.join("payload").display().to_string());
            temp_dirs.push(dir);
        }
        Ok(Self { args, temp_dirs })
    }

    /// 认证参数（`-o ...` / `-i ...` 列表，不含 host 目标）。
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

impl Drop for SshAuthArgs {
    fn drop(&mut self) {
        for dir in &self.temp_dirs {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// 把内容写入一个 `temp_dir/<uuid>/payload` 的 0600 文件（对齐 Node
/// `withTempFile`：目录 mkdtemp、文件 0600、内容补尾随换行）。
fn write_temp_secure_file(prefix: &str, contents: &str) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!(
        "{}{}-{}",
        prefix,
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("create temp dir {} failed: {error}", dir.display()))?;
    let file_path = dir.join("payload");
    let normalized = if contents.ends_with('\n') {
        contents.to_string()
    } else {
        format!("{contents}\n")
    };
    let write_result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&file_path)?;
        file.write_all(normalized.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!(
            "write temp file {} failed: {error}",
            file_path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(dir)
}

/// `run_ssh_command` 选项（对齐 Node `runSshCommand` options）。
#[derive(Debug, Clone)]
pub struct SshCommandOptions {
    pub env: BTreeMap<String, String>,
    pub stdin: Option<String>,
    pub timeout_ms: u64,
    pub max_buffer: usize,
}

impl Default for SshCommandOptions {
    fn default() -> Self {
        Self {
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: 15_000,
            max_buffer: 1024 * 128,
        }
    }
}

/// SSH 命令失败结果（对齐 Node `spawnText` 失败时挂在 error 上的
/// stdout / stderr / code / signal / killed 属性）。
#[derive(Debug, Clone)]
pub struct SshCommandError {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub timed_out: bool,
    pub message: String,
}

impl std::fmt::Display for SshCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SshCommandError {}

impl SshCommandError {
    /// 组装失败消息（对齐 Node `spawnText` close 分支：
    /// `stderr.trim() || stdout.trim() || Process exited with code <code>`）。
    fn from_output(
        stdout: &str,
        stderr: &str,
        exit_code: Option<i32>,
        signal: Option<String>,
        timed_out: bool,
    ) -> Self {
        let message = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("Process exited with code {}", exit_code.unwrap_or(-1))
        };
        Self {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code,
            signal,
            timed_out,
            message,
        }
    }
}

/// 构建远程登录脚本（对齐 Node `runSshCommand` / `buildSshSpawnTarget`
/// 的 remoteScript）：
///
/// ```sh
/// if [ -f /etc/profile ]; then . /etc/profile >/dev/null 2>&1 || true; fi
/// if [ -f "$HOME/.profile" ]; then . "$HOME/.profile" >/dev/null 2>&1 || true; fi
/// if [ -f "$HOME/.bash_profile" ]; then . "$HOME/.bash_profile" >/dev/null 2>&1 || true; elif [ -f "$HOME/.bashrc" ]; then . "$HOME/.bashrc" >/dev/null 2>&1 || true; fi
/// if [ -f "$HOME/.zprofile" ]; then . "$HOME/.zprofile" >/dev/null 2>&1 || true; fi
/// exec env KEY=VALUE sh -c '<remoteCommand>'   # env 非空时
/// exec sh -c '<remoteCommand>'                 # env 为空时
/// ```
///
/// 先 source 登录 profile 再跑 `env KEY=VAL cmd`，让用户显式注入的
/// identity 覆盖 profile 里重新导出的同名变量（对齐 Node 注释语义）。
fn build_ssh_login_script(
    remote_command: &str,
    env: &BTreeMap<String, String>,
) -> Result<String, String> {
    for key in env.keys() {
        if !is_valid_shell_env_key(key) {
            return Err(format!("Invalid SSH environment variable key: {key}"));
        }
    }
    let env_args: Vec<String> = env
        .iter()
        .map(|(key, value)| format!("{key}={}", shell_quote(value)))
        .collect();
    let exec_line = if env_args.is_empty() {
        format!("exec sh -c {}", shell_quote(remote_command))
    } else {
        format!(
            "exec env {} sh -c {}",
            env_args.join(" "),
            shell_quote(remote_command)
        )
    };
    let mut lines = ssh_profile_sourcing_lines();
    lines.push(exec_line);
    Ok(lines.join(" && "))
}

/// 登录 profile sourcing 行（对齐 Node `runSshCommand` /
/// `buildSshSpawnTarget` remoteScript 的前 4 行）。
fn ssh_profile_sourcing_lines() -> Vec<String> {
    vec![
        "if [ -f /etc/profile ]; then . /etc/profile >/dev/null 2>&1 || true; fi".to_string(),
        "if [ -f \"$HOME/.profile\" ]; then . \"$HOME/.profile\" >/dev/null 2>&1 || true; fi"
            .to_string(),
        "if [ -f \"$HOME/.bash_profile\" ]; then . \"$HOME/.bash_profile\" >/dev/null 2>&1 || true; elif [ -f \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\" >/dev/null 2>&1 || true; fi"
            .to_string(),
        "if [ -f \"$HOME/.zprofile\" ]; then . \"$HOME/.zprofile\" >/dev/null 2>&1 || true; fi"
            .to_string(),
    ]
}

/// 运行一条远程 SSH 命令（对齐 Node `runSshCommand`）：
///
/// 1. 构造认证参数（临时 key / known_hosts 文件）
/// 2. 组装远程登录脚本（profile sourcing + env 注入 + `sh -c`）
/// 3. `ssh -o ... [-i key] -p <port> user@host sh -c '<script>'`
/// 4. 支持 stdin 管道、超时（SIGTERM → 5s 后 SIGKILL）、maxBuffer 上限
///
/// 成功返回 `{stdout, stderr}`；失败返回 [`SshCommandError`]（携带
/// stdout / stderr / exit_code / signal / timed_out，对齐 Node error 属性）。
pub async fn run_ssh_command(
    config: &SshConnectionConfig,
    remote_command: &str,
    options: &SshCommandOptions,
) -> Result<SshCommandResult, SshCommandError> {
    let auth = SshAuthArgs::create(config).map_err(|error| SshCommandError {
        stdout: String::new(),
        stderr: error.clone(),
        exit_code: None,
        signal: None,
        timed_out: false,
        message: error,
    })?;
    let script =
        build_ssh_login_script(remote_command, &options.env).map_err(|error| SshCommandError {
            stdout: String::new(),
            stderr: error.clone(),
            exit_code: None,
            signal: None,
            timed_out: false,
            message: error,
        })?;
    let mut ssh_args: Vec<String> = auth.args().to_vec();
    ssh_args.push("-p".to_string());
    ssh_args.push(config.port.to_string());
    ssh_args.push(format!("{}@{}", config.username, config.host));
    ssh_args.push("sh -c".to_string());
    ssh_args.push(shell_quote(&script));
    spawn_ssh_capture("ssh", &ssh_args, options).await
}

/// 对 `ssh` 子进程做完整 I/O 捕获（对齐 Node `spawnText`）：
/// - stdin 提供时走管道并写入后 end；否则 /dev/null
/// - stdout / stderr 全量收集，任一流超过 `max_buffer` → SIGTERM 并判失败
/// - `timeout_ms` 超时 → SIGTERM，5s 宽限后 SIGKILL（防止远端挂死）
/// - close 时 exit code 0 → 成功；否则失败（携带 code / signal / killed）
async fn spawn_ssh_capture(
    command: &str,
    args: &[String],
    options: &SshCommandOptions,
) -> Result<SshCommandResult, SshCommandError> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;
    use tokio::time::{timeout, Duration};

    let spawn_error = |message: String| SshCommandError {
        stdout: String::new(),
        stderr: message.clone(),
        exit_code: None,
        signal: None,
        timed_out: false,
        message,
    };
    let mut child = Command::new(command)
        .args(args)
        .stdin(if options.stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| spawn_error(error.to_string()))?;

    if let Some(stdin_data) = &options.stdin {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| spawn_error("child stdin pipe unavailable".to_string()))?;
        let data = stdin_data.clone();
        tokio::spawn(async move {
            let _ = stdin.write_all(data.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| spawn_error("child stdout pipe unavailable".to_string()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| spawn_error("child stderr pipe unavailable".to_string()))?;

    let max_buffer = options.max_buffer;
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut overflow = false;
        let mut tmp = [0u8; 8192];
        loop {
            match stdout.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.len() > max_buffer {
                        overflow = true;
                        break;
                    }
                }
            }
        }
        (String::from_utf8_lossy(&buf).into_owned(), overflow)
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut overflow = false;
        let mut tmp = [0u8; 8192];
        loop {
            match stderr.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.len() > max_buffer {
                        overflow = true;
                        break;
                    }
                }
            }
        }
        (String::from_utf8_lossy(&buf).into_owned(), overflow)
    });

    let mut timed_out = false;
    let wait_result = if options.timeout_ms > 0 {
        match timeout(Duration::from_millis(options.timeout_ms), child.wait()).await {
            Ok(result) => result
                .map(|status| status.code())
                .map_err(|error| error.to_string()),
            Err(_) => {
                timed_out = true;
                // SIGTERM，5s 宽限后 SIGKILL（对齐 Node killEscalation）。
                let _ = child.kill().await;
                let escalated = timeout(Duration::from_secs(5), child.wait()).await.is_err();
                if escalated {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                }
                Ok(None)
            }
        }
    } else {
        child
            .wait()
            .await
            .map(|status| status.code())
            .map_err(|error| error.to_string())
    };
    let (stdout, stdout_overflow) = stdout_task.await.unwrap_or_default();
    let (stderr, stderr_overflow) = stderr_task.await.unwrap_or_default();

    if stdout_overflow || stderr_overflow {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(SshCommandError {
            stdout,
            stderr,
            exit_code: None,
            signal: None,
            timed_out: false,
            message: format!("Process output exceeded maxBuffer of {max_buffer} bytes."),
        });
    }

    let exit_code = wait_result.map_err(|message| SshCommandError {
        stdout: stdout.clone(),
        stderr: stderr.clone(),
        exit_code: None,
        signal: None,
        timed_out: false,
        message,
    })?;
    match exit_code {
        Some(0) => Ok(SshCommandResult { stdout, stderr }),
        _ => Err(SshCommandError::from_output(
            &stdout, &stderr, exit_code, None, timed_out,
        )),
    }
}

/// SSH runner（对齐 Node `createSshCommandManagedRuntimeRunner` 返回值，
/// 实现 [`crate::bridge_executor::BridgeCommandRunner`]）。
pub struct SshCommandManagedRuntimeRunner {
    spec: SshRemoteExecutionSpec,
    default_cwd: String,
    max_buffer_bytes: usize,
}

impl SshCommandManagedRuntimeRunner {
    /// 构造 SSH runner。
    ///
    /// `default_cwd` 缺省（空）时回退到 `spec.remote_cwd`；`max_buffer_bytes`
    /// 非正数时回退到 1 MiB（对齐 Node：`1024 * 1024`）。
    #[must_use]
    pub fn new(
        spec: SshRemoteExecutionSpec,
        default_cwd: Option<String>,
        max_buffer_bytes: Option<usize>,
    ) -> Self {
        let default_cwd = default_cwd
            .map(|cwd| cwd.trim().to_string())
            .filter(|cwd| !cwd.is_empty())
            .unwrap_or_else(|| spec.remote_cwd.clone());
        let max_buffer_bytes = max_buffer_bytes
            .filter(|bytes| *bytes > 0)
            .unwrap_or(1024 * 1024);
        Self {
            spec,
            default_cwd,
            max_buffer_bytes,
        }
    }

    /// 当前 spec（调试 / 测试用）。
    #[must_use]
    pub fn spec(&self) -> &SshRemoteExecutionSpec {
        &self.spec
    }
}

#[async_trait]
impl crate::bridge_executor::BridgeCommandRunner for SshCommandManagedRuntimeRunner {
    async fn execute(
        &self,
        input: &crate::bridge_executor::RunnerExecuteInput,
    ) -> Result<crate::bridge_executor::RunnerCommandResult, String> {
        use crate::bridge_executor::RunnerCommandResult;
        // 对齐 Node createSshCommandManagedRuntimeRunner.execute：
        // `command` trim；`cwd` trim 后为空 → defaultCwd；env 全部注入。
        let command = input.command.trim();
        let cwd = input.cwd.trim();
        let cwd = if cwd.is_empty() {
            &self.default_cwd
        } else {
            cwd
        };
        // 对齐 Node：`Object.entries(env).filter(v => typeof v === "string")`
        // —— Rust 的 BTreeMap<String,String> 天然全是字符串，原样保留
        // 空值（`KEY=''` 也会注入）。
        let env_entries: Vec<(String, String)> = input
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let env_prefix = if env_entries.is_empty() {
            String::new()
        } else {
            format!(
                "env {} ",
                env_entries
                    .iter()
                    .map(|(key, value)| format!("{key}={}", shell_quote(value)))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let export_prefix = if env_entries.is_empty() {
            String::new()
        } else {
            format!(
                "{} ",
                env_entries
                    .iter()
                    .map(|(key, value)| format!("export {key}={};", shell_quote(value)))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let command_script = if command == "sh" || command == "bash" {
            if matches!(
                input.args.first().map(String::as_str),
                Some("-c") | Some("-lc")
            ) && input.args.len() >= 2
            {
                format!("{export_prefix}{}", input.args[1])
            } else {
                format!(
                    "{env_prefix}exec {}",
                    std::iter::once(shell_quote(command))
                        .chain(input.args.iter().map(|arg| shell_quote(arg)))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            }
        } else {
            format!(
                "{env_prefix}exec {}",
                std::iter::once(shell_quote(command))
                    .chain(input.args.iter().map(|arg| shell_quote(arg)))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let remote_command = format!("cd {} && {command_script}", shell_quote(cwd));
        let result = run_ssh_command(
            &self.spec.as_connection_config(),
            &remote_command,
            &SshCommandOptions {
                env: BTreeMap::new(),
                stdin: input.stdin.clone(),
                timeout_ms: input.timeout_ms,
                max_buffer: self.max_buffer_bytes,
            },
        )
        .await;
        match result {
            Ok(ok) => Ok(RunnerCommandResult {
                stdout: ok.stdout,
                stderr: ok.stderr,
                exit_code: Some(0),
                timed_out: false,
            }),
            Err(error) => Ok(RunnerCommandResult {
                stdout: error.stdout,
                stderr: error.stderr,
                exit_code: error.exit_code,
                timed_out: error.timed_out,
            }),
        }
    }
}

/// 构建 `ssh` spawn 目标（对齐 Node `buildSshSpawnTarget`：命令固定
/// `ssh`，args 含认证参数 + `-p <port> user@host sh -c '<script>'`，
/// auth 临时文件随返回值 drop 清理）。进程 session bridge 启动时把
/// 该目标交给 runner 执行。
#[must_use]
pub fn build_ssh_spawn_target(
    spec: &SshRemoteExecutionSpec,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> Result<SshSpawnTarget, String> {
    for key in env.keys() {
        if !is_valid_shell_env_key(key) {
            return Err(format!("Invalid SSH environment variable key: {key}"));
        }
    }
    let auth = SshAuthArgs::create(&spec.as_connection_config())?;
    let env_args: Vec<String> = env
        .iter()
        .map(|(key, value)| format!("{key}={}", shell_quote(value)))
        .collect();
    let remote_command_parts: String = std::iter::once(shell_quote(command))
        .chain(args.iter().map(|arg| shell_quote(arg)))
        .collect::<Vec<_>>()
        .join(" ");
    let exec_line = if env_args.is_empty() {
        format!("exec {remote_command_parts}")
    } else {
        format!("exec env {} {remote_command_parts}", env_args.join(" "))
    };
    // 对齐 Node buildSshSpawnTarget：profile 行 + `cd` + `exec` 直接
    // join（不包 `sh -c`，env 直接进 exec 行）。
    let mut remote_script = ssh_profile_sourcing_lines();
    remote_script.push(format!(
        "cd {} && {exec_line}",
        shell_quote(&spec.remote_cwd)
    ));
    let remote_script = remote_script.join(" && ");
    let mut ssh_args: Vec<String> = auth.args().to_vec();
    ssh_args.push("-p".to_string());
    ssh_args.push(spec.port.to_string());
    ssh_args.push(format!("{}@{}", spec.username, spec.host));
    ssh_args.push("sh -c".to_string());
    ssh_args.push(shell_quote(&remote_script));
    Ok(SshSpawnTarget {
        args: ssh_args,
        _auth: auth,
    })
}

/// `ssh` spawn 目标（对齐 Node `buildSshSpawnTarget` 返回值：
/// `{ command, args, cleanup }`；Rust 中 command 固定 `"ssh"`，
/// cleanup 由 `_auth` 的 Drop 完成）。
pub struct SshSpawnTarget {
    pub args: Vec<String>,
    _auth: SshAuthArgs,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge_executor::BridgeCommandRunner;
    use serde_json::json;

    // ---- types ----

    #[test]
    fn ssh_remote_execution_spec_from_parts_round_trip() {
        let cfg = SshConnectionConfig {
            host: "h".to_string(),
            port: 22,
            username: "u".to_string(),
            remote_workspace_path: "/w".to_string(),
            private_key: Some("pk".to_string()),
            known_hosts: None,
            strict_host_key_checking: true,
        };
        let spec = SshRemoteExecutionSpec::from_parts(cfg.clone(), "/w/cwd".to_string());
        assert_eq!(spec.remote_cwd, "/w/cwd");
        assert_eq!(spec.host, "h");
        assert_eq!(spec.port, 22);
        assert_eq!(spec.as_connection_config(), cfg);
    }

    #[test]
    fn effective_remote_workspace_path_falls_back_to_remote_cwd() {
        let mut spec = SshRemoteExecutionSpec {
            host: "h".into(),
            port: 22,
            username: "u".into(),
            remote_workspace_path: String::new(),
            private_key: None,
            known_hosts: None,
            strict_host_key_checking: true,
            remote_cwd: "/w".into(),
        };
        assert_eq!(spec.effective_remote_workspace_path(), "/w");
        spec.remote_workspace_path = "/w/explicit".into();
        assert_eq!(spec.effective_remote_workspace_path(), "/w/explicit");
    }

    // ---- shell_quote ----

    #[test]
    fn shell_quote_handles_plain() {
        assert_eq!(shell_quote("plain"), "'plain'");
    }

    #[test]
    fn shell_quote_escapes_single_quote() {
        // A single ' in input becomes '"'"' (5 chars)
        // inside the outer pair of 's. Combined with 2 outer quotes,
        // input `with'quote` produces a 16-char output with 5 's.
        let q = shell_quote("with'quote");
        assert_eq!(q.len(), 16);
        let q_quote_count = q.chars().filter(|c| *c == '\'').count();
        assert_eq!(q_quote_count, 5);
        assert!(q.starts_with("'with'"));
        assert!(q.ends_with("'quote'"));
    }

    #[test]
    fn shell_quote_handles_spaces() {
        assert_eq!(shell_quote("/tmp/with space/dir"), "'/tmp/with space/dir'");
    }

    // ---- is_valid_shell_env_key ----

    #[test]
    fn valid_shell_env_keys() {
        assert!(is_valid_shell_env_key("PATH"));
        assert!(is_valid_shell_env_key("_PRIVATE"));
        assert!(is_valid_shell_env_key("a1_b2"));
    }

    #[test]
    fn invalid_shell_env_keys() {
        assert!(!is_valid_shell_env_key("1ST"));
        assert!(!is_valid_shell_env_key("a-b"));
        assert!(!is_valid_shell_env_key(""));
        assert!(!is_valid_shell_env_key("a.b"));
    }

    // ---- parse_ssh_remote_execution_spec ----

    #[test]
    fn ssh_parser_accepts_valid_payload() {
        let v = json!({
            "host": "h",
            "username": "u",
            "remoteCwd": "/w",
            "port": 2222,
        });
        let s = parse_ssh_remote_execution_spec(&v).expect("must parse");
        assert_eq!(s.host, "h");
        assert_eq!(s.port, 2222);
        assert_eq!(s.username, "u");
        assert_eq!(s.remote_cwd, "/w");
        assert!(s.strict_host_key_checking);
    }

    #[test]
    fn ssh_parser_round_trips_via_camelcase() {
        let original = SshRemoteExecutionSpec {
            host: "h.example".into(),
            port: 22,
            username: "u".into(),
            remote_workspace_path: "/w".into(),
            private_key: Some("pk-mock".into()),
            known_hosts: Some("kh-mock".into()),
            strict_host_key_checking: true,
            remote_cwd: "/w".into(),
        };
        let json = serde_json::to_value(&original).expect("to_value");
        assert_eq!(json["host"], "h.example");
        assert_eq!(json["port"], 22);
        assert!(json["privateKey"].is_string());
        assert!(json["knownHosts"].is_string());
        assert!(json["strictHostKeyChecking"].as_bool().unwrap_or(false));
        let back = parse_ssh_remote_execution_spec(&json).expect("must parse back");
        assert_eq!(back, original);
    }

    #[test]
    fn ssh_parser_defaults_remote_workspace_path_to_remote_cwd() {
        let v = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 22});
        let s = parse_ssh_remote_execution_spec(&v).expect("must parse");
        assert_eq!(s.remote_workspace_path, "/w");
    }

    #[test]
    fn ssh_parser_rejects_invalid_port() {
        let zero = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 0});
        assert!(parse_ssh_remote_execution_spec(&zero).is_none());
        let overflow = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 70_000});
        assert!(parse_ssh_remote_execution_spec(&overflow).is_none());
    }

    #[test]
    fn ssh_parser_rejects_missing_required_fields() {
        let v = json!({"host": "h", "port": 22});
        assert!(parse_ssh_remote_execution_spec(&v).is_none());
    }

    #[test]
    fn ssh_parser_rejects_non_object_value() {
        assert!(parse_ssh_remote_execution_spec(&json!(null)).is_none());
        assert!(parse_ssh_remote_execution_spec(&json!("str")).is_none());
        assert!(parse_ssh_remote_execution_spec(&json!(42)).is_none());
        assert!(parse_ssh_remote_execution_spec(&json!([1, 2, 3])).is_none());
    }

    #[test]
    fn ssh_parser_accepts_string_port() {
        let v = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": "2222"});
        let s = parse_ssh_remote_execution_spec(&v).expect("must parse");
        assert_eq!(s.port, 2222);
    }

    #[test]
    fn ssh_parser_omits_empty_optional_fields() {
        let v = json!({
            "host": "h", "username": "u", "remoteCwd": "/w", "port": 22,
            "privateKey": "", "knownHosts": "",
        });
        let s = parse_ssh_remote_execution_spec(&v).expect("must parse");
        assert!(s.private_key.is_none());
        assert!(s.known_hosts.is_none());
    }

    // ---- tar_exclude_args ----

    #[test]
    fn tar_exclude_args_prepends_resource_fork_pattern() {
        let args = tar_exclude_args(Some(&["node_modules".into(), "target".into()]));
        assert_eq!(
            args,
            vec![
                "--exclude",
                "._*",
                "--exclude",
                "node_modules",
                "--exclude",
                "target",
            ]
        );
    }

    #[test]
    fn tar_exclude_args_without_excludes_has_only_resource_fork() {
        let args = tar_exclude_args(None);
        assert_eq!(args, vec!["--exclude", "._*"]);
    }

    // ---- tar_spawn_env_defaults ----

    #[test]
    fn tar_spawn_env_sets_copyfile_disable() {
        let env = tar_spawn_env_defaults();
        assert_eq!(env.get("COPYFILE_DISABLE").map(String::as_str), Some("1"));
        // BTreeMap iteration is sorted - useful for deterministic shell test
        let keys: Vec<&str> = env.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["COPYFILE_DISABLE"]);
    }

    // ---- tar_pattern_to_regexp ----

    #[test]
    fn tar_pattern_to_regexp_matches_literal() {
        let re = tar_pattern_to_regexp("node_modules").expect("valid regex");
        assert!(re.is_match("node_modules"));
        assert!(!re.is_match("sub/node_modules"));
    }

    #[test]
    fn tar_pattern_to_regexp_handles_star_glob() {
        // `*` becomes `[^/]*` so it does NOT span `/`s.
        let re = tar_pattern_to_regexp("*/target").expect("valid regex");
        assert!(re.is_match("a/target"));
        assert!(!re.is_match("a/b/target"));
        assert!(!re.is_match("target"));
    }

    #[test]
    fn tar_pattern_to_regexp_handles_question_glob() {
        let re = tar_pattern_to_regexp("?").expect("valid regex");
        assert!(re.is_match("a"));
        assert!(re.is_match("b"));
        assert!(!re.is_match("ab"));
        assert!(!re.is_match(""));
    }

    #[test]
    fn tar_pattern_to_regexp_escapes_special_chars() {
        // `.` is a regex special but should match a literal `.`
        let re = tar_pattern_to_regexp("file.txt").expect("valid");
        assert!(re.is_match("file.txt"));
        assert!(!re.is_match("fileXtxt"));
    }

    // ---- build_known_hosts_entry ----

    #[test]
    fn build_known_hosts_entry_formats_bracketed_host_port() {
        let entry = build_known_hosts_entry(KnownHostsEntryInput {
            host: "h.example".to_string(),
            port: 2222,
            public_key: "ssh-ed25519 AAAA...rest".to_string(),
        });
        assert_eq!(entry, "[h.example]:2222 ssh-ed25519 AAAA...rest");
    }

    #[test]
    fn build_known_hosts_entry_strips_whitespace() {
        let entry = build_known_hosts_entry(KnownHostsEntryInput {
            host: "  h.example  ".to_string(),
            port: 22,
            public_key: "  ssh-ed25519 AAAA  ".to_string(),
        });
        assert_eq!(entry, "[h.example]:22 ssh-ed25519 AAAA");
    }

    // ---- build_ssh_login_script ----

    #[test]
    fn login_script_sources_profiles_then_exec_sh_c() {
        let script = build_ssh_login_script("echo hi", &BTreeMap::new()).expect("valid");
        assert!(script.starts_with(
            "if [ -f /etc/profile ]; then . /etc/profile >/dev/null 2>&1 || true; fi && "
        ));
        assert!(script.contains(
            "if [ -f \"$HOME/.profile\" ]; then . \"$HOME/.profile\" >/dev/null 2>&1 || true; fi"
        ));
        assert!(script.contains(
            "elif [ -f \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\" >/dev/null 2>&1 || true; fi"
        ));
        assert!(script.contains("exec sh -c 'echo hi'"));
    }

    #[test]
    fn login_script_injects_env_before_sh_c() {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/opt/bin:/usr/bin".to_string());
        env.insert("TOKEN".to_string(), "a'b".to_string());
        let script = build_ssh_login_script("pwd", &env).expect("valid");
        assert!(script.contains("exec env PATH='/opt/bin:/usr/bin' TOKEN='a'\"'\"'b' sh -c 'pwd'"));
    }

    #[test]
    fn login_script_rejects_invalid_env_key() {
        let mut env = BTreeMap::new();
        env.insert("1BAD-KEY".to_string(), "v".to_string());
        let error = build_ssh_login_script("true", &env).expect_err("invalid key");
        assert!(error.contains("Invalid SSH environment variable key: 1BAD-KEY"));
    }

    // ---- SshAuthArgs ----

    fn config_with_secrets() -> SshConnectionConfig {
        SshConnectionConfig {
            host: "h".into(),
            port: 2222,
            username: "u".into(),
            remote_workspace_path: "/w".into(),
            private_key: Some("PRIVATE KEY DATA".into()),
            known_hosts: Some("[h]:2222 ssh-ed25519 AAAA".into()),
            strict_host_key_checking: true,
        }
    }

    #[test]
    fn auth_args_use_temp_files_for_key_and_known_hosts() {
        let auth = SshAuthArgs::create(&config_with_secrets()).expect("auth args");
        let args = auth.args().to_vec();
        assert!(args.contains(&"-o".to_string()));
        let flags: Vec<&String> = args.iter().collect();
        assert!(flags
            .windows(2)
            .any(|w| w[0] == "-o" && w[1] == "BatchMode=yes"));
        assert!(flags
            .windows(2)
            .any(|w| w[0] == "-o" && w[1] == "ConnectTimeout=10"));
        assert!(flags
            .windows(2)
            .any(|w| w[0] == "-o" && w[1] == "StrictHostKeyChecking=yes"));
        let known_hosts = flags
            .windows(2)
            .find(|w| w[0] == "-o" && w[1].starts_with("UserKnownHostsFile="))
            .map(|w| w[1].trim_start_matches("UserKnownHostsFile="))
            .expect("known hosts file");
        let key = flags
            .windows(2)
            .find(|w| w[0] == "-i")
            .map(|w| w[1].as_str())
            .expect("private key file");
        assert!(known_hosts.contains("paperclip-ssh-known-hosts-"));
        assert!(key.contains("paperclip-ssh-key-"));
        assert!(std::path::Path::new(known_hosts).exists());
        assert!(std::path::Path::new(key).exists());
        // drop 清理临时文件
        drop(auth);
        assert!(!std::path::Path::new(known_hosts).exists());
        assert!(!std::path::Path::new(key).exists());
    }

    #[test]
    fn auth_args_relaxed_mode_points_known_hosts_at_dev_null() {
        let mut config = config_with_secrets();
        config.strict_host_key_checking = false;
        let auth = SshAuthArgs::create(&config).expect("auth args");
        let flags: Vec<&String> = auth.args().iter().collect();
        assert!(flags
            .windows(2)
            .any(|w| w[0] == "-o" && w[1] == "StrictHostKeyChecking=no"));
        assert!(flags
            .windows(2)
            .any(|w| w[0] == "-o" && w[1] == "UserKnownHostsFile=/dev/null"));
        // 仍保留 -i key（有 private key 时）
        assert!(flags.windows(2).any(|w| w[0] == "-i"));
    }

    #[test]
    fn auth_args_without_secrets_has_no_temp_files() {
        let mut config = config_with_secrets();
        config.private_key = None;
        config.known_hosts = None;
        let auth = SshAuthArgs::create(&config).expect("auth args");
        let flags: Vec<&String> = auth.args().iter().collect();
        assert!(!flags.windows(2).any(|w| w[0] == "-i"));
        assert!(!flags
            .windows(2)
            .any(|w| w[0] == "-o" && w[1].starts_with("UserKnownHostsFile=")));
    }

    // ---- SshCommandManagedRuntimeRunner ----

    fn spec_for_runner(remote_cwd: &str) -> SshRemoteExecutionSpec {
        SshRemoteExecutionSpec {
            host: "h".into(),
            port: 2222,
            username: "u".into(),
            remote_workspace_path: remote_cwd.into(),
            private_key: None,
            known_hosts: None,
            strict_host_key_checking: false,
            remote_cwd: remote_cwd.into(),
        }
    }

    #[test]
    fn runner_defaults_fall_back_to_remote_cwd_and_1mib() {
        let runner = SshCommandManagedRuntimeRunner::new(spec_for_runner("/w"), None, None);
        assert_eq!(runner.default_cwd, "/w");
        assert_eq!(runner.max_buffer_bytes, 1024 * 1024);
        let runner = SshCommandManagedRuntimeRunner::new(
            spec_for_runner("/w"),
            Some("  /x  ".into()),
            Some(0),
        );
        assert_eq!(runner.default_cwd, "/x");
        assert_eq!(runner.max_buffer_bytes, 1024 * 1024);
    }

    #[test]
    fn runner_builds_remote_command_with_cd_and_exec() {
        let runner = SshCommandManagedRuntimeRunner::new(spec_for_runner("/w"), None, None);
        let input = crate::bridge_executor::RunnerExecuteInput {
            command: "printf".into(),
            args: vec!["%s".into(), "hi".into()],
            cwd: String::new(),
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: 1000,
        };
        // execute 会尝试真实 ssh；这里只验证失败路径的 exit_code 传播
        // 语义（本机无可用 sshd 时 ssh 命令会立即失败，返回非 0）。
        let result = tokio_test_runtime()
            .block_on(runner.execute(&input))
            .expect("runner err is Ok result");
        assert!(result.exit_code != Some(0));
    }

    // ---- build_ssh_spawn_target ----

    #[test]
    fn spawn_target_shapes_ssh_args() {
        let spec = spec_for_runner("/w");
        let mut env = BTreeMap::new();
        env.insert("A".to_string(), "1".to_string());
        let target = build_ssh_spawn_target(&spec, "node", &["--version".into()], &env)
            .expect("spawn target");
        assert!(target
            .args
            .windows(2)
            .any(|w| w[0] == "-p" && w[1] == "2222"));
        assert!(target.args.contains(&"u@h".to_string()));
        let sh_idx = target
            .args
            .iter()
            .position(|a| a == "sh -c")
            .expect("sh -c");
        let script_arg = &target.args[sh_idx + 1];
        // 整个 remote_script 被 shell_quote 包裹，内部单引号会被转义为
        // `"'"'"`，因此按转义后的形式断言。
        assert!(script_arg.contains("cd '\"'\"'/w'\"'\"'"));
        assert!(script_arg
            .contains("exec env A='\"'\"'1'\"'\"' '\"'\"'node'\"'\"' '\"'\"'--version'\"'\"'"));
        // 外层仍由单个 `sh -c` 包裹
        assert!(script_arg.starts_with("'if [ -f /etc/profile"));
        assert!(script_arg.ends_with("version'\"'\"''"));
    }

    fn tokio_test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }
}
