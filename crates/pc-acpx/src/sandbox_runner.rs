//! Sandbox provider runner — `BridgeCommandRunner` 的本地/沙箱实现集合。
//!
//! 对齐 Node `createLocalSandboxRunner` + `CommandManagedRuntimeRunner`：
//! - [`LocalSandboxRunner`]：本地沙箱进程（自动探测 `bwrap`/`bubblewrap`，
//!   否则回退到 plain process spawn）。
//! - [`RunnerError`]：typed 错误分类（provider / 命令 / IO / 超时）。
//! - [`RunnerRegistry`]：按 provider id 解析 runner。
//! - [`create_local_sandbox_runner`]：Node 工厂函数等价。
//!
//! 真实 provider runner（daytona / e2b / managed runtime）走
//! `bridge_executor::BridgeCommandRunner` 抽象，由调用方注入。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::bridge_executor::{
    BridgeCommandRunner, RunnerCommandResult, RunnerExecuteInput,
};

// ============================================================================
// RunnerError - typed 错误
// ============================================================================

/// Runner 错误分类。供上层做重试 / 降级 / 监控分流。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunnerErrorCategory {
    /// Provider 未注册 / 不可用。
    ProviderUnavailable,
    /// 进程 spawn 失败（命令找不到 / 权限拒绝）。
    Spawn,
    /// IO 失败（stdin/stdout/stderr 管道）。
    Io,
    /// 命令超时。
    Timeout,
    /// 命令返回非 0（业务错误，不算 runner 错误）。
    NonZeroExit,
    /// 配置错误（options 字段缺失 / 非法）。
    Config,
    /// 其它未知错误。
    Other,
}

impl RunnerErrorCategory {
    /// 是否可短暂重试。
    #[must_use]
    pub fn is_transient(self) -> bool {
        matches!(self, Self::Timeout | Self::Io)
    }
}

/// Runner typed error。
#[derive(Debug, Clone)]
pub struct RunnerError {
    pub category: RunnerErrorCategory,
    pub message: String,
    pub exit_code: Option<i32>,
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.exit_code {
            Some(c) => write!(f, "[{:?}/exit={}] {}", self.category, c, self.message),
            None => write!(f, "[{:?}] {}", self.category, self.message),
        }
    }
}

impl std::error::Error for RunnerError {}

impl RunnerError {
    /// 是否可短暂重试（透传 category 的判断）。
    #[must_use]
    pub fn is_transient(&self) -> bool {
        self.category.is_transient()
    }

    #[must_use]
    pub fn new(category: RunnerErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
            exit_code: None,
        }
    }

    /// 附带 exit code 的错误。
    #[must_use]
    pub fn with_exit_code(mut self, code: i32) -> Self {
        self.exit_code = Some(code);
        self
    }

    /// 启发式从字符串分类。
    #[must_use]
    pub fn classify(message: &str) -> Self {
        let lower = message.to_lowercase();
        if lower.contains("timeout") || lower.contains("timed out") {
            return Self::new(RunnerErrorCategory::Timeout, message);
        }
        if lower.contains("not found") || lower.contains("no such file") {
            return Self::new(RunnerErrorCategory::Spawn, message);
        }
        if lower.contains("permission denied") {
            return Self::new(RunnerErrorCategory::Spawn, message);
        }
        if lower.contains("pipe") || lower.contains("stdin") || lower.contains("stdout") || lower.contains("io error") {
            return Self::new(RunnerErrorCategory::Io, message);
        }
        if lower.contains("config") || lower.contains("missing") {
            return Self::new(RunnerErrorCategory::Config, message);
        }
        if lower.contains("provider") && lower.contains("unavailable") {
            return Self::new(RunnerErrorCategory::ProviderUnavailable, message);
        }
        Self::new(RunnerErrorCategory::Other, message)
    }

    pub fn category(&self) -> RunnerErrorCategory {
        self.category
    }
}

impl From<RunnerError> for String {
    fn from(err: RunnerError) -> Self {
        err.to_string()
    }
}

impl From<std::io::Error> for RunnerError {
    fn from(err: std::io::Error) -> Self {
        let kind = err.kind();
        let cat = match kind {
            std::io::ErrorKind::NotFound => RunnerErrorCategory::Spawn,
            std::io::ErrorKind::PermissionDenied => RunnerErrorCategory::Spawn,
            std::io::ErrorKind::TimedOut => RunnerErrorCategory::Timeout,
            _ => RunnerErrorCategory::Io,
        };
        Self::new(cat, format!("{err}"))
    }
}

// ============================================================================
// LocalSandboxRunner - 本地沙箱执行器
// ============================================================================

/// 沙箱隔离模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SandboxMode {
    /// 直接 spawn（无任何隔离，仅用于本地开发 / 测试）。
    #[default]
    Direct,
    /// 用 `bwrap` (bubblewrap) 隔离。
    Bubblewrap,
}

impl SandboxMode {
    /// 自动选择：探测 `bwrap` 是否可用；可用则用 Bubblewrap，否则 Direct。
    /// 探测结果用 `OnceLock` 缓存。
    #[must_use]
    pub fn auto_detect() -> SandboxMode {
        static CACHE: OnceLock<SandboxMode> = OnceLock::new();
        *CACHE.get_or_init(|| {
            // 在 PATH 中探测 bwrap。
            if which("bwrap").is_some() {
                SandboxMode::Bubblewrap
            } else {
                SandboxMode::Direct
            }
        })
    }
}

impl std::str::FromStr for SandboxMode {
    type Err = RunnerError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "direct" | "none" | "off" => Ok(SandboxMode::Direct),
            "bwrap" | "bubblewrap" => Ok(SandboxMode::Bubblewrap),
            "auto" => Ok(SandboxMode::auto_detect()),
            other => Err(RunnerError::new(
                RunnerErrorCategory::Config,
                format!("unknown sandbox mode `{other}` (expected direct|bwrap|auto)"),
            )),
        }
    }
}

impl std::fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Direct => "direct",
            Self::Bubblewrap => "bwrap",
        })
    }
}

/// `LocalSandboxRunner` 配置。
#[derive(Debug, Clone)]
pub struct LocalSandboxRunnerOptions {
    /// 沙箱模式。默认 `Auto`（探测）。
    pub mode: SandboxMode,
    /// Bubblewrap 模式下额外挂载的 ro 路径列表。
    pub bind_ro: Vec<String>,
    /// Bubblewrap 模式下额外挂载的 rw 路径列表。
    pub bind_rw: Vec<String>,
    /// Bubblewrap 模式下网络隔离：true 表示完全断网。
    pub unshare_network: bool,
    /// 是否在 sandbox 失败时打印诊断信息到 stderr。
    pub verbose: bool,
}

impl Default for LocalSandboxRunnerOptions {
    fn default() -> Self {
        Self {
            mode: SandboxMode::auto_detect(),
            bind_ro: vec!["/usr".into(), "/lib".into(), "/lib64".into(), "/etc".into()],
            bind_rw: vec!["/tmp".into()],
            unshare_network: false,
            verbose: false,
        }
    }
}

/// 本地沙箱执行器。
///
/// - `Direct` 模式：直接 `Command::new(command).args(args).spawn()`。
/// - `Bubblewrap` 模式：`bwrap --ro-bind <p> <p> --bind <p> <p> --chdir <cwd> -- <command> <args>`。
#[derive(Debug, Clone)]
pub struct LocalSandboxRunner {
    options: Arc<LocalSandboxRunnerOptions>,
}

impl Default for LocalSandboxRunner {
    fn default() -> Self {
        Self::new(LocalSandboxRunnerOptions::default())
    }
}

impl LocalSandboxRunner {
    #[must_use]
    pub fn new(options: LocalSandboxRunnerOptions) -> Self {
        Self {
            options: Arc::new(options),
        }
    }

    #[must_use]
    pub fn with_mode(mode: SandboxMode) -> Self {
        Self::new(LocalSandboxRunnerOptions {
            mode,
            ..LocalSandboxRunnerOptions::default()
        })
    }

    #[must_use]
    pub fn options(&self) -> &LocalSandboxRunnerOptions {
        &self.options
    }
}

#[async_trait]
impl BridgeCommandRunner for LocalSandboxRunner {
    async fn execute(&self, input: &RunnerExecuteInput) -> Result<RunnerCommandResult, String> {
        match self.options.mode {
            SandboxMode::Direct => execute_direct(input).await.map_err(|e| e.to_string()),
            SandboxMode::Bubblewrap => {
                execute_bwrap(self.options.as_ref(), input)
                    .await
                    .map_err(|e| e.to_string())
            }
        }
    }
}

async fn execute_direct(input: &RunnerExecuteInput) -> Result<RunnerCommandResult, RunnerError> {
    let mut command = Command::new(&input.command);
    command
        .args(&input.args)
        .envs(&input.env)
        .stdin(if input.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if !input.cwd.is_empty() {
        command.current_dir(&input.cwd);
    }
    let mut child = command.spawn()?;
    if let Some(stdin_data) = &input.stdin {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| RunnerError::new(RunnerErrorCategory::Io, "child stdin pipe unavailable"))?;
        stdin.write_all(stdin_data.as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let (mut stdout, mut stderr) = (
        child.stdout.take().ok_or_else(|| RunnerError::new(RunnerErrorCategory::Io, "child stdout pipe unavailable"))?,
        child.stderr.take().ok_or_else(|| RunnerError::new(RunnerErrorCategory::Io, "child stderr pipe unavailable"))?,
    );
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    });
    let wait = async { child.wait().await.map(|s| s.code()) };
    let (status, timed_out) = match timeout(Duration::from_millis(input.timeout_ms), wait).await {
        Ok(Ok(code)) => (code, false),
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, true)
        }
    };
    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();
    Ok(RunnerCommandResult {
        stdout,
        stderr,
        exit_code: status,
        timed_out,
    })
}

async fn execute_bwrap(
    options: &LocalSandboxRunnerOptions,
    input: &RunnerExecuteInput,
) -> Result<RunnerCommandResult, RunnerError> {
    let mut command = Command::new("bwrap");
    for p in &options.bind_ro {
        command.arg("--ro-bind").arg(p).arg(p);
    }
    for p in &options.bind_rw {
        command.arg("--bind").arg(p).arg(p);
    }
    if options.unshare_network {
        command.arg("--unshare-net");
    }
    if !input.cwd.is_empty() {
        command.arg("--chdir").arg(&input.cwd);
    }
    command.arg("--").arg(&input.command).args(&input.args);
    command
        .envs(&input.env)
        .stdin(if input.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if options.verbose {
        eprintln!("[local-sandbox] exec: bwrap -- ... {} {:?}", input.command, input.args);
    }
    let mut child = command.spawn()?;
    if let Some(stdin_data) = &input.stdin {
        let mut stdin = child.stdin.take().ok_or_else(|| RunnerError::new(RunnerErrorCategory::Io, "bwrap stdin pipe unavailable"))?;
        stdin.write_all(stdin_data.as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let (mut stdout, mut stderr) = (
        child.stdout.take().ok_or_else(|| RunnerError::new(RunnerErrorCategory::Io, "bwrap stdout pipe unavailable"))?,
        child.stderr.take().ok_or_else(|| RunnerError::new(RunnerErrorCategory::Io, "bwrap stderr pipe unavailable"))?,
    );
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    });
    let wait = async { child.wait().await.map(|s| s.code()) };
    let (status, timed_out) = match timeout(Duration::from_millis(input.timeout_ms), wait).await {
        Ok(Ok(code)) => (code, false),
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, true)
        }
    };
    Ok(RunnerCommandResult {
        stdout: stdout_task.await.unwrap_or_default(),
        stderr: stderr_task.await.unwrap_or_default(),
        exit_code: status,
        timed_out,
    })
}

/// Node `createLocalSandboxRunner` 等价：返回默认选项的 LocalSandboxRunner。
#[must_use]
pub fn create_local_sandbox_runner() -> LocalSandboxRunner {
    LocalSandboxRunner::default()
}

/// Node `createLocalSandboxRunner(options)` 等价：根据 JSON options 构造。
pub fn create_local_sandbox_runner_from_options(
    options: &serde_json::Value,
) -> Result<LocalSandboxRunner, RunnerError> {
    let mode_str = options.get("mode").and_then(|v| v.as_str()).unwrap_or("auto");
    let mode: SandboxMode = mode_str.parse()?;
    let bind_ro = options
        .get("bindRo")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_else(|| vec!["/usr".into(), "/lib".into(), "/lib64".into(), "/etc".into()]);
    let bind_rw = options
        .get("bindRw")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_else(|| vec!["/tmp".into()]);
    let unshare_network = options
        .get("unshareNetwork")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let verbose = options.get("verbose").and_then(|v| v.as_bool()).unwrap_or(false);
    Ok(LocalSandboxRunner::new(LocalSandboxRunnerOptions {
        mode,
        bind_ro,
        bind_rw,
        unshare_network,
        verbose,
    }))
}

// ============================================================================
// RunnerRegistry - 按 provider id 解析
// ============================================================================

/// Runner provider id。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunnerProviderId(pub String);

impl RunnerProviderId {
    pub const LOCAL: &'static str = "local";
    pub const LOCAL_SANDBOX: &'static str = "local_sandbox";
    pub const SSH: &'static str = "ssh";

    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RunnerProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Runner 注册表。
#[derive(Default)]
pub struct RunnerRegistry {
    runners: HashMap<String, Arc<dyn BridgeCommandRunner>>,
}

impl RunnerRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, id: impl Into<String>, runner: Arc<dyn BridgeCommandRunner>) {
        self.runners.insert(id.into(), runner);
    }

    /// 注册默认三个 provider：local / local_sandbox / ssh。
    /// `ssh_runner` 为 None 时跳过 ssh 槽。
    #[must_use]
    pub fn with_defaults(ssh_runner: Option<Arc<dyn BridgeCommandRunner>>) -> Self {
        let mut r = Self::new();
        r.register(RunnerProviderId::LOCAL, Arc::new(crate::bridge_executor::LocalProcessBridgeRunner));
        r.register(RunnerProviderId::LOCAL_SANDBOX, Arc::new(create_local_sandbox_runner()));
        if let Some(ssh) = ssh_runner {
            r.register(RunnerProviderId::SSH, ssh);
        }
        r
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<dyn BridgeCommandRunner>> {
        self.runners.get(id)
    }

    #[must_use]
    pub fn resolve(&self, id: &RunnerProviderId) -> Result<Arc<dyn BridgeCommandRunner>, RunnerError> {
        self.runners
            .get(id.as_str())
            .cloned()
            .ok_or_else(|| RunnerError::new(
                RunnerErrorCategory::ProviderUnavailable,
                format!("runner provider `{id}` is not registered"),
            ))
    }

    #[must_use]
    pub fn provider_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.runners.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.runners.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runners.is_empty()
    }
}

// ============================================================================
// Helper: which(cmd) - 简化版，不依赖 which crate
// ============================================================================

/// 在 PATH 中查找可执行文件；返回绝对路径或 None。
fn which(cmd: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn r566_sandbox_mode_from_str_parses_known_values() {
        assert_eq!("direct".parse::<SandboxMode>().unwrap(), SandboxMode::Direct);
        assert_eq!("auto".parse::<SandboxMode>().unwrap(), SandboxMode::auto_detect());
        assert_eq!("bwrap".parse::<SandboxMode>().unwrap(), SandboxMode::Bubblewrap);
    }

    #[test]
    fn r566_sandbox_mode_from_str_rejects_unknown() {
        let err = "weird".parse::<SandboxMode>().unwrap_err();
        assert_eq!(err.category, RunnerErrorCategory::Config);
    }

    #[test]
    fn r566_sandbox_mode_display() {
        assert_eq!(SandboxMode::Direct.to_string(), "direct");
        assert_eq!(SandboxMode::Bubblewrap.to_string(), "bwrap");
    }

    #[test]
    fn r566_runner_error_classify_basic() {
        let e = RunnerError::classify("request timed out");
        assert_eq!(e.category, RunnerErrorCategory::Timeout);
        assert!(e.is_transient());

        let e = RunnerError::classify("No such file or directory");
        assert_eq!(e.category, RunnerErrorCategory::Spawn);
        assert!(!e.is_transient());

        let e = RunnerError::classify("io error reading stdout");
        assert_eq!(e.category, RunnerErrorCategory::Io);
        assert!(e.is_transient());

        let e = RunnerError::classify("provider unavailable");
        assert_eq!(e.category, RunnerErrorCategory::ProviderUnavailable);
    }

    #[test]
    fn r566_runner_error_with_exit_code() {
        let e = RunnerError::new(RunnerErrorCategory::NonZeroExit, "command failed").with_exit_code(42);
        assert_eq!(e.exit_code, Some(42));
        assert!(e.to_string().contains("42"));
    }

    #[test]
    fn r566_runner_registry_default_three_providers() {
        let reg = RunnerRegistry::with_defaults(None);
        assert!(reg.get(RunnerProviderId::LOCAL).is_some());
        assert!(reg.get(RunnerProviderId::LOCAL_SANDBOX).is_some());
        // ssh 未注册
        assert!(reg.get(RunnerProviderId::SSH).is_none());
        assert_eq!(reg.provider_ids(), vec!["local", "local_sandbox"]);
    }

    #[test]
    fn r566_runner_registry_resolve_unknown_returns_error() {
        let reg = RunnerRegistry::new();
        // 使用 match 而非 unwrap_err：Result<Arc<dyn BridgeCommandRunner>, _> 的
        // Ok 端 trait object 不实现 Debug。
        let result = reg.resolve(&RunnerProviderId::new("mystery"));
        let err = match result {
            Ok(_) => panic!("expected Err for unknown provider"),
            Err(e) => e,
        };
        assert_eq!(err.category, RunnerErrorCategory::ProviderUnavailable);
        assert_eq!(err.category(), RunnerErrorCategory::ProviderUnavailable);
        assert!(!err.is_transient());
    }

    #[test]
    fn r566_create_local_sandbox_runner_default() {
        let r = create_local_sandbox_runner();
        // mode 可能是 Direct 或 Bubblewrap（取决于机器）
        assert!(matches!(r.options().mode, SandboxMode::Direct | SandboxMode::Bubblewrap));
    }

    #[test]
    fn r566_create_local_sandbox_runner_from_options_explicit_direct() {
        let opts = serde_json::json!({ "mode": "direct" });
        let r = create_local_sandbox_runner_from_options(&opts).unwrap();
        assert_eq!(r.options().mode, SandboxMode::Direct);
        assert!(!r.options().unshare_network);
    }

    #[test]
    fn r566_create_local_sandbox_runner_from_options_with_overrides() {
        let opts = serde_json::json!({
            "mode": "bwrap",
            "bindRo": ["/opt", "/var"],
            "bindRw": ["/workspace"],
            "unshareNetwork": true,
            "verbose": true,
        });
        let r = create_local_sandbox_runner_from_options(&opts).unwrap();
        assert_eq!(r.options().mode, SandboxMode::Bubblewrap);
        assert!(r.options().unshare_network);
        assert!(r.options().verbose);
        assert_eq!(r.options().bind_ro, vec!["/opt", "/var"]);
        assert_eq!(r.options().bind_rw, vec!["/workspace"]);
    }

    #[test]
    fn r566_create_local_sandbox_runner_from_options_rejects_bad_mode() {
        let opts = serde_json::json!({ "mode": "firejail" });
        assert!(create_local_sandbox_runner_from_options(&opts).is_err());
    }

    #[tokio::test]
    async fn r566_local_sandbox_runner_executes_echo() {
        // Direct 模式跑 echo，确保基本 spawn 路径可用。
        let runner = LocalSandboxRunner::with_mode(SandboxMode::Direct);
        let mut env = BTreeMap::new();
        env.insert("MY_VAR".into(), "hello".into());
        let input = RunnerExecuteInput {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "echo hi-$MY_VAR".into()],
            cwd: String::new(),
            env,
            stdin: None,
            timeout_ms: 5000,
        };
        let r = runner.execute(&input).await.unwrap();
        assert_eq!(r.exit_code, Some(0));
        assert!(!r.timed_out);
        assert!(r.stdout.contains("hi-hello"), "stdout was: {:?}", r.stdout);
    }

    #[tokio::test]
    async fn r566_local_sandbox_runner_times_out() {
        let runner = LocalSandboxRunner::with_mode(SandboxMode::Direct);
        let input = RunnerExecuteInput {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 5".into()],
            cwd: String::new(),
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: 100,
        };
        let r = runner.execute(&input).await.unwrap();
        assert!(r.timed_out, "should have timed out");
    }

    #[tokio::test]
    async fn r566_local_sandbox_runner_spawn_failure_is_typed() {
        // 不存在的命令 -> Spawn 错误。
        let runner = LocalSandboxRunner::with_mode(SandboxMode::Direct);
        let input = RunnerExecuteInput {
            command: "/nonexistent/binary/xyz".into(),
            args: vec![],
            cwd: String::new(),
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: 1000,
        };
        let err = runner.execute(&input).await.unwrap_err();
        let parsed = RunnerError::classify(&err);
        assert_eq!(parsed.category, RunnerErrorCategory::Spawn);
    }
}
