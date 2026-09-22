//! `pi-local`：把本机 `pi` CLI 接成 M3 的第一个真 adapter（上游 `server/pkg/agent/pi.go`）。
//!
//! # 与上游的对应关系
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`args::build_args`] | `buildPiArgs`（`pi.go` L909） |
//! | [`sanitize`] | `stripPiStructuredToolMarkup` / `safePiTextEmitLen` / `looksLikePiControlTokenPrefix`（L242-360） |
//! | [`stream`] | `parsePiStreamLine` 与 `piStreamEvent`（L830-907） |
//! | [`run`] | `Execute` 的 stdout 循环与 `pi.go` L640-735 的终态归因 |
//! | [`PiLocal::launch`] | `Execute` 的 spawn/管道装配段（L400-560） |
//! | [`PiLocal::probe_version`] | `detectCLIVersion`（共享实现，在 `claude.go` L1110） |
//!
//! # 两处**有意偏离**上游（都在 `docs/18` 有记录）
//!
//! 1. **会话锁用进程内注册表，而不是 `flock`**：上游 `tryLockPiSessionFile`
//!    （`pi_session_lock_unix.go`）对 session 文件的 fd 做 `flock(LOCK_EX|LOCK_NB)`，
//!    锁覆盖整个子进程生命周期、进程崩溃时由内核释放。Rust 标准库没有 `flock`，
//!    而为一个锁引入 `libc` 直接依赖违反本片"不新增第三方依赖"的约束
//!    （§7.5）。因此这里用进程内独占集合 + Drop guard：**同一个 daemon 进程内**
//!    语义等价（同一 session 文件的第二个 run 被拒）；跨进程不覆盖，留待 M3-8。
//! 2. **会话被占用折叠成 `AdapterError::SessionBusy`**：上游返回
//!    `piSessionBusyResult`（`Status:"failed"` + `ResumeRejectedTransient:true`），
//!    即"续跑被拒、可重试"，分类由 M3-3 的租约/重试逻辑消费。本片没有 M3-3，
//!    与其伪造一个终态不如在启动阶段直接失败，"可重试"这一语义由错误类型承载。
//!
//! # 没实现的（M3-8 backlog，见 `docs/18`）
//!
//! - 10 分钟的 turn 错误宽限（`defaultPiTurnErrorGrace`）：当前由
//!   `LaunchRequest::timeout` 兜住，M3-8 做统一策略；
//! - 最低版本门（`MinVersions`）：`probe_version` 只报事实，不做拦截。

pub mod sanitize;

mod args;
mod run;
mod stream;

pub use stream::PiDecoder;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch};

use crate::adapter::{
    AdapterCapabilities, AdapterError, CancelOutcome, EventDecoder, LaunchRequest, ProtocolFamily,
    RunHandle, RunId, RuntimeAdapter, VersionProbe,
};
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

use run::{PiRun, PiRunContext};
use stream::LABEL;

/// 上游守护进程的默认执行超时（`pi.go` 的 fallback；上游默认 2h）。
// 2 小时；用 `from_secs` 是为了不依赖 `Duration::from_hours`（MSRV 1.80 上不一定可用）。
#[allow(clippy::duration_suboptimal_units)]
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// 子进程退出后等 stdout/stderr 读取任务收尾的宽限（上游 `cmd.WaitDelay`）。
const DEFAULT_STREAM_DRAIN_GRACE: Duration = Duration::from_secs(10);

/// `--version` 探测超时（上游 `detectVersionTimeout`）。
const DEFAULT_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// `pi-local` 的配置。
#[derive(Debug, Clone)]
pub struct PiLocalConfig {
    /// CLI 可执行文件；默认 `pi`（走 `PATH` 解析）。可写绝对路径（测试/多版本共存用）。
    pub executable: PathBuf,
    /// 会话文件目录；默认 `~/.multica/pi-sessions`（上游 `piSessionDir`）。
    pub session_dir: PathBuf,
    /// 默认执行超时（`LaunchRequest::timeout` 优先）。
    pub default_timeout: Duration,
    /// 退出后 stdio 收尾宽限。
    pub stream_drain_grace: Duration,
    /// 版本探测超时。
    pub version_probe_timeout: Duration,
}

impl Default for PiLocalConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::from(LABEL),
            session_dir: default_session_dir(),
            default_timeout: DEFAULT_TIMEOUT,
            stream_drain_grace: DEFAULT_STREAM_DRAIN_GRACE,
            version_probe_timeout: DEFAULT_VERSION_PROBE_TIMEOUT,
        }
    }
}

/// 上游 `piSessionDir()`：`<HOME>/.multica/pi-sessions`。
fn default_session_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join(".multica").join("pi-sessions"),
        // 无 HOME（容器里少见但可能出现）：退到临时目录，别让 launch 直接崩。
        _ => std::env::temp_dir().join(".multica").join("pi-sessions"),
    }
}

/// 新会话文件路径：`<dir>/<UTC 20060102T150405.000000000>.jsonl`（上游 `newPiSessionPath`）。
///
/// 同纳秒碰撞理论上可能，但碰撞会被会话锁拦成硬错误（上游同款行为），
/// 不会出现两个 run 写同一个 JSONL。
fn new_session_path(session_dir: &Path) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.9f");
    session_dir.join(format!("{stamp}.jsonl"))
}

/// 上游 `ensurePiSessionFile`：**新**会话才建目录与空文件；续跑路径原样不碰。
fn ensure_session_file(path: &Path) -> Result<(), AdapterError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AdapterError::SessionIo {
            kind: AgentType::Pi,
            path: path.to_path_buf(),
            source,
        })?;
    }
    if !path.exists() {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| AdapterError::SessionIo {
                kind: AgentType::Pi,
                path: path.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

/// `PATH` 解析（上游 `exec.LookPath`）：带分隔符的路径按文件校验，裸名按 `PATH` 找。
fn resolve_executable(configured: &Path) -> Option<PathBuf> {
    if configured.is_absolute() || configured.components().count() > 1 {
        return configured.is_file().then_some(configured.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(configured))
        .find(|candidate| candidate.is_file())
}

/// run/cancel 槽位表 + 会话独占集合。
///
/// 两者都是"运行期状态"，随 adapter 实例存活；`PiLocal` 通过 `Arc` 把它们共享给
/// 每个 run 任务（run 结束时自己摘掉自己的 cancel 槽位）。
#[derive(Debug, Default)]
struct RunRegistry {
    sessions: RwLock<HashSet<PathBuf>>,
    cancels: Mutex<HashMap<RunId, watch::Sender<bool>>>,
}

impl RunRegistry {
    /// 独占一个会话文件；已被占用返回 `None`（= 上游 `piSessionBusyResult`）。
    fn try_lock_session(self: &Arc<Self>, path: PathBuf) -> Option<SessionGuard> {
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if !sessions.insert(path.clone()) {
            return None;
        }
        drop(sessions);
        Some(SessionGuard {
            path,
            owner: Arc::clone(self),
        })
    }

    fn register_cancel(&self, run_id: RunId, cancel: watch::Sender<bool>) {
        self.cancels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(run_id, cancel);
    }

    fn signal_cancel(&self, run_id: &RunId) -> bool {
        let cancels = self.cancels.lock().unwrap_or_else(PoisonError::into_inner);
        match cancels.get(run_id) {
            Some(cancel) => {
                // 接收端已在 select 里等：`send` 立刻唤醒它。
                let _ = cancel.send(true);
                true
            }
            None => false,
        }
    }

    /// run 任务收尾时摘掉自己的槽位（会话锁由 guard 随任务栈释放）。
    fn finish(&self, run_id: &RunId) {
        self.cancels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(run_id);
    }

    /// 仅测试用：当前被独占的会话数。
    #[cfg(test)]
    fn locked_session_count(&self) -> usize {
        self.sessions
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// 仅测试用：当前挂着的 cancel 槽位数。
    #[cfg(test)]
    fn active_run_count(&self) -> usize {
        self.cancels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// 会话独占 guard（`flock` 的进程内替代，见模块文档第 1 条偏离）。
struct SessionGuard {
    path: PathBuf,
    owner: Arc<RunRegistry>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.owner
            .sessions
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.path);
    }
}

/// 本机 pi CLI adapter。
#[derive(Debug)]
pub struct PiLocal {
    config: PiLocalConfig,
    runs: Arc<RunRegistry>,
}

impl PiLocal {
    /// 用给定配置构造（不探测、不 spawn）。
    pub fn new(config: PiLocalConfig) -> Self {
        Self {
            config,
            runs: Arc::new(RunRegistry::default()),
        }
    }

    /// 当前配置。
    pub fn config(&self) -> &PiLocalConfig {
        &self.config
    }

    /// 配置的可执行文件（未做 `PATH` 解析）。
    pub fn executable(&self) -> &Path {
        &self.config.executable
    }

    /// `PATH` 解析后的可执行文件；解析不到返回启动期错误。
    fn resolved_executable(&self) -> Result<PathBuf, AdapterError> {
        resolve_executable(&self.config.executable).ok_or_else(|| {
            AdapterError::ExecutableUnavailable {
                kind: AgentType::Pi,
                path: self.config.executable.clone(),
                reason: "PATH 中找不到该命令".to_owned(),
            }
        })
    }
}

impl Default for PiLocal {
    fn default() -> Self {
        Self::new(PiLocalConfig::default())
    }
}

#[async_trait]
impl RuntimeAdapter for PiLocal {
    fn kind(&self) -> AgentType {
        AgentType::Pi
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            protocol: ProtocolFamily::JsonLine,
            streaming: true,
            thinking: true,
            tool_events: true,
            usage_reporting: true,
            resume: true,
            version_probe: true,
            launch_header: AgentType::Pi.launch_header().to_owned(),
        }
    }

    async fn launch(&self, request: LaunchRequest) -> Result<RunHandle, AdapterError> {
        if request.prompt.trim().is_empty() {
            return Err(AdapterError::EmptyPrompt {
                kind: AgentType::Pi,
            });
        }
        let executable = self.resolved_executable()?;

        // 会话路径：续跑用调用方给的 path（pi 的 session id 就是文件路径），
        // 否则新开一个（上游 `resumeSessionID` 为空即新会话）。
        let resume_session = request
            .resume_session
            .as_deref()
            .map(str::trim)
            .filter(|session| !session.is_empty());
        let (session_path, is_resume) = match resume_session {
            Some(session) => (PathBuf::from(session), true),
            None => (new_session_path(&self.config.session_dir), false),
        };
        if !is_resume {
            ensure_session_file(&session_path)?;
        }
        // 锁要在 spawn 之前拿到：拿不到就是"启动失败"，不会产生一个注定冲突的 run。
        let session_guard = self
            .runs
            .try_lock_session(session_path.clone())
            .ok_or_else(|| AdapterError::SessionBusy {
                kind: AgentType::Pi,
                path: session_path.clone(),
            })?;

        let args = args::build_args(&request, &session_path);
        let mut command = Command::new(&executable);
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = request.cwd.as_deref() {
            command.current_dir(cwd);
        }
        // 上游 `buildEnv` = 继承进程环境 + 请求里的覆盖项（`Command` 默认就继承）。
        for (key, value) in &request.env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|source| AdapterError::Spawn {
            kind: AgentType::Pi,
            path: executable.clone(),
            source,
        })?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (events, events_rx) = mpsc::unbounded_channel();
        let (outcome_tx, outcome_rx) = oneshot::channel();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.runs.register_cancel(request.run_id.clone(), cancel_tx);

        let context = PiRunContext {
            run_id: request.run_id.clone(),
            prompt: request.prompt,
            session_path,
            executable,
            timeout: request.timeout.unwrap_or(self.config.default_timeout),
            stream_drain_grace: self.config.stream_drain_grace,
            fallback_model: request.model,
            started_at: Instant::now(),
            events: Some(events),
            outcome: Some(outcome_tx),
            runs: Arc::clone(&self.runs),
            session_guard: Some(session_guard),
        };
        tokio::spawn(PiRun::new(context, cancel_rx).execute(child, stdin, stdout, stderr, pid));

        Ok(RunHandle::new(request.run_id, events_rx, outcome_rx))
    }

    async fn cancel(&self, run_id: &RunId) -> Result<CancelOutcome, AdapterError> {
        Ok(if self.runs.signal_cancel(run_id) {
            CancelOutcome::Signalled
        } else {
            CancelOutcome::NotRunning
        })
    }

    async fn probe_version(&self) -> Result<VersionProbe, AdapterError> {
        let executable = self.resolved_executable()?;
        let kind = AgentType::Pi;
        let mut command = Command::new(&executable);
        command.arg("--version").kill_on_drop(true);
        let output =
            match tokio::time::timeout(self.config.version_probe_timeout, command.output()).await {
                Ok(Ok(output)) => output,
                Ok(Err(source)) => {
                    return Err(AdapterError::VersionProbe {
                        kind,
                        reason: source.to_string(),
                    })
                }
                Err(_elapsed) => {
                    return Err(AdapterError::VersionProbe {
                        kind,
                        reason: format!(
                            "超过 {:?} 未返回（上游 detectVersionTimeout 同值）",
                            self.config.version_probe_timeout
                        ),
                    })
                }
            };
        let raw = String::from_utf8_lossy(&output.stdout);
        let first_line = raw
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .to_owned();
        let version = crate::adapter::Semver::parse(&first_line);
        // 版本号救回分支：非零退出但输出里有可辨认的版本号仍算探测成功（上游同款）。
        if !output.status.success() && version.is_none() {
            return Err(AdapterError::VersionProbe {
                kind,
                reason: format!(
                    "退出码 {:?}，stderr: {}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        Ok(VersionProbe {
            kind,
            executable,
            raw: first_line,
            version,
        })
    }

    fn decoder(&self) -> Box<dyn EventDecoder> {
        Box::new(PiDecoder::new())
    }
}

impl TestableAdapter for PiLocal {
    fn with_conformance_env(executable: &Path, workdir: &Path) -> Self {
        Self::new(PiLocalConfig {
            executable: executable.to_path_buf(),
            // 会话文件也必须待在临时目录里，不能污染跑测试的人/CI 的 $HOME。
            session_dir: workdir.join("sessions"),
            ..PiLocalConfig::default()
        })
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: format!("{LABEL} 0.83.2\n"),
            expected_version: Some("0.83.2".to_owned()),
            // 真实的 pi 事件流形状：起手 → turn → 文本增量 → 用量。
            success_stdout: concat!(
                r#"{"type":"agent_start"}"#,
                "\n",
                r#"{"type":"turn_start"}"#,
                "\n",
                r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hello from pi"}}"#,
                "\n",
                r#"{"type":"turn_end","message":{"role":"assistant","model":"gpt-x","usage":{"input":1,"output":2,"totalTokens":3}}}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "hello from pi".to_owned(),
            expected_usage_tokens: Some(3),
            junk_stdout: concat!(
                "not json at all\n",
                r#"{"type":"unknown_future_event","payload":{"nested":true}}"#,
                "\n",
                r#"{"type":"turn_start"}"#,
                "\n",
                r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"ok"}}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "pi exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_upstream_fallbacks() {
        let config = PiLocalConfig::default();
        assert_eq!(config.executable, PathBuf::from("pi"));
        assert_eq!(config.default_timeout, Duration::from_secs(7200));
        assert_eq!(config.stream_drain_grace, Duration::from_secs(10));
        assert_eq!(config.version_probe_timeout, Duration::from_secs(10));
        assert!(config.session_dir.ends_with(".multica/pi-sessions"));
    }

    #[test]
    fn new_session_path_is_jsonl_under_session_dir() {
        let path = new_session_path(Path::new("/tmp/sessions"));
        assert_eq!(path.parent(), Some(Path::new("/tmp/sessions")));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext == "jsonl"),
            "{name}"
        );
        // 20060102T150405.000000000 形状：15 位时间戳 + '.' + 9 位纳秒 + .jsonl
        assert_eq!(name.len(), "20060102T150405.000000000.jsonl".len());
        assert_eq!(name.as_bytes()[8], b'T');
        assert_eq!(name.as_bytes()[15], b'.');
    }

    #[test]
    fn session_lock_is_exclusive_and_released_on_drop() {
        let registry = Arc::new(RunRegistry::default());
        let path = PathBuf::from("/tmp/mc-runtime-lock-test.jsonl");
        let first = registry.try_lock_session(path.clone()).unwrap();
        assert!(registry.try_lock_session(path.clone()).is_none());
        assert_eq!(registry.locked_session_count(), 1);
        drop(first);
        assert_eq!(registry.locked_session_count(), 0);
        assert!(registry.try_lock_session(path).is_some());
    }

    #[test]
    fn cancel_slots_track_run_lifecycle() {
        let registry = RunRegistry::default();
        let run_id = RunId::new();
        assert!(!registry.signal_cancel(&run_id));
        let (cancel_tx, cancel_rx) = watch::channel(false);
        registry.register_cancel(run_id.clone(), cancel_tx);
        assert_eq!(registry.active_run_count(), 1);
        assert!(registry.signal_cancel(&run_id));
        assert!(*cancel_rx.borrow());
        registry.finish(&run_id);
        assert_eq!(registry.active_run_count(), 0);
        assert!(!registry.signal_cancel(&run_id));
    }

    #[test]
    fn resolve_executable_accepts_absolute_and_rejects_missing() {
        assert_eq!(
            resolve_executable(Path::new("mc-runtime-no-such-binary-xyz")),
            None
        );
        assert_eq!(
            resolve_executable(Path::new("/mc-runtime-no-such-binary-xyz")),
            None
        );
        // 带分隔符的相对路径直接按文件校验（不走 PATH）。
        assert_eq!(
            resolve_executable(Path::new("./mc-runtime-no-such-binary-xyz")),
            None
        );
    }

    #[test]
    fn capabilities_are_pi_shaped() {
        let adapter = PiLocal::default();
        let caps = adapter.capabilities();
        assert_eq!(adapter.kind(), AgentType::Pi);
        assert_eq!(caps.protocol, ProtocolFamily::JsonLine);
        assert!(caps.streaming && caps.thinking && caps.tool_events);
        assert!(caps.usage_reporting && caps.resume && caps.version_probe);
        assert_eq!(caps.launch_header, "pi (json mode)");
    }

    #[tokio::test]
    async fn empty_prompt_is_rejected_before_spawning() {
        let adapter = PiLocal::default();
        let error = adapter
            .launch(LaunchRequest::new("   "))
            .await
            .expect_err("空 prompt 必须被拒");
        assert!(matches!(error, AdapterError::EmptyPrompt { .. }));
    }

    // 一致性套件：任何 adapter 只要实现 `TestableAdapter` 就得到同一组断言。
    crate::adapter_conformance!(PiLocal);
}
