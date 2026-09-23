//! CLI 类 adapter 的共用核心（M3-8 batch 1 抽出来的）。
//!
//! # 为什么有这一层
//!
//! batch 1 的 7 个 provider（claude / codebuddy / codex / copilot / opencode /
//! codearts / deveco）共享**同一套运行骨架**：spawn、把 prompt 送到 stdin、
//! 逐行喂解码器、终态归因（退出码 / 超时 / 取消）、stderr 尾巴、版本探测。
//! 差别只在
//!
//! - argv（[`CliSpec::build_args`]）、
//! - prompt 怎么送（[`PromptTransport`]）、
//! - 事件流怎么解（[`CliDecoder`]）、
//! - 能力位与封锁参数表（各 provider 自己的 `args.rs`）。
//!
//! 于是这里定义 [`CliProvider`] + 一个 blanket 的
//! `impl<P: CliProvider> RuntimeAdapter for P`，每个 provider 只需要实现
//! `spec()` / `config()` / `decoder_for()` 三个方法。`pi_local`（M3-2）**不动**：
//! 它有自己的会话锁（`flock` 的进程内替代）与 `--session <path>` 语义，
//! 迁移留给 M4（`docs/33` 记为技术债）。
//!
//! # 有意偏离 `docs/18` §4 的"新 adapter 配方"
//!
//! `docs/18` §4 给的目录配方是 `{mod.rs, args.rs, run.rs, stream.rs}` 四个文件
//! **各自一份**。batch 1 有 7 个 provider，照配方抄会得到 7 份 run 循环与 7 份
//! 参数过滤（其中"终态优先级"那段尤其容易抄歪）。本片改成"共享核心 + 每个
//! provider 只留 spec/decoder"，偏离记在 `docs/33`。
//!
//! # 边界
//!
//! 这里**不碰** `adapter.rs` / `adapter/traits.rs` 的公开契约：`ProtocolFamily`
//! 不新增变体，`RuntimeAdapter` 的方法签名一字不改。共享核心只是把同一份契约
//! 复用 7 次。

pub mod args;
pub mod decoder;
mod run;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch};

use crate::adapter::{
    AdapterCapabilities, AdapterError, CancelOutcome, EventDecoder, LaunchRequest, ProtocolFamily,
    RunHandle, RunId, RuntimeAdapter, VersionProbe,
};
use crate::catalog::AgentType;

pub use args::{filter_extra_args, ArgPolicy, ArgValueMode};
pub use decoder::{tokens, CliDecoder, CliSummary, DecoderState};

use run::{CliRun, CliRunContext};

/// 默认执行超时（上游守护进程的 fallback；与 `pi_local` 同值）。
// 2 小时；用 `from_secs` 是为了不依赖 `Duration::from_hours`（MSRV 1.80 上不一定可用）。
#[allow(clippy::duration_suboptimal_units)]
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
/// 流结束后的排水宽限（上游 `cmd.WaitDelay` 同义）。
pub const DEFAULT_DRAIN_GRACE: Duration = Duration::from_secs(1);
/// 版本探测超时。
pub const DEFAULT_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// 一个 provider 的运行期配置。
#[derive(Debug, Clone)]
pub struct CliCoreConfig {
    /// 可执行文件（裸名字走 `PATH`，含路径分隔符则按文件校验）。
    pub executable: PathBuf,
    /// `LaunchRequest::timeout` 为空时用的默认超时。
    pub default_timeout: Duration,
    /// 排水宽限。
    pub stream_drain_grace: Duration,
    /// 版本探测超时。
    pub version_probe_timeout: Duration,
}

impl CliCoreConfig {
    /// 以可执行文件名建配置，其余取默认值。
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            default_timeout: DEFAULT_TIMEOUT,
            stream_drain_grace: DEFAULT_DRAIN_GRACE,
            version_probe_timeout: DEFAULT_VERSION_PROBE_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_stream_drain_grace(mut self, grace: Duration) -> Self {
        self.stream_drain_grace = grace;
        self
    }
}

/// prompt 怎么送到子进程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTransport {
    /// 一整段文本写进 stdin，写完即关（opencode / codearts）。
    StdinText,
    /// Claude 系 `stream-json` 的 stdin 信封（`{"type":"user",...}` 一行），写完即关
    /// （claude / codebuddy）。
    StdinJsonEnvelope,
    /// prompt 是 argv 的一部分，stdin 关闭（copilot / deveco）。
    Argv,
    /// JSON-RPC over stdio：stdin 常开，帧由解码器的 outbox 驱动（codex）。
    JsonRpc,
}

impl PromptTransport {
    /// 是否把 prompt 写进 stdin（一致性套件的断言开关用得上）。
    pub fn goes_to_stdin(self) -> bool {
        matches!(
            self,
            Self::StdinText | Self::StdinJsonEnvelope | Self::JsonRpc
        )
    }
}

/// provider 的能力位（映射到 [`AdapterCapabilities`]）。
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)] // 能力位就是一组 bool，拆成状态机只会更难读（与 adapter.rs 同款）
pub struct CliCapabilities {
    /// 线协议族。
    pub protocol: ProtocolFamily,
    /// stdout 上是否逐条增量。
    pub streaming: bool,
    /// 是否给推理增量。
    pub thinking: bool,
    /// 是否给工具调用事件。
    pub tool_events: bool,
    /// 是否上报 token 用量。
    pub usage_reporting: bool,
    /// 是否支持续跑指定会话。
    pub resume: bool,
}

impl CliCapabilities {
    /// 补齐"所有 adapter 都成立"的三项：能流式、能探测版本、启动骨架来自白名单表。
    pub fn to_adapter(self, kind: AgentType) -> AdapterCapabilities {
        AdapterCapabilities {
            protocol: self.protocol,
            streaming: self.streaming,
            thinking: self.thinking,
            tool_events: self.tool_events,
            usage_reporting: self.usage_reporting,
            resume: self.resume,
            version_probe: true,
            launch_header: kind.launch_header().to_owned(),
        }
    }
}

/// 一个 provider 的静态描述。
#[derive(Clone, Copy)]
pub struct CliSpec {
    /// 日志 / 错误串里的名字（也是退出码错误串的前缀）。
    pub label: &'static str,
    /// prompt 怎么送。
    pub transport: PromptTransport,
    /// 能力位。
    pub capabilities: CliCapabilities,
    /// stdin 写失败是否致命。
    ///
    /// `true`（`StdinText` / `StdinJsonEnvelope`）= prompt 没送到，run 注定跑歪；
    /// `false`（`JsonRpc`）= 对端先退出导致 EPIPE 是常态，退出码才权威。
    pub prompt_write_is_fatal: bool,
    /// argv 组装（含 `extra_args` 过滤）。
    pub build_args: fn(&LaunchRequest) -> Vec<String>,
}

/// 一个 CLI provider 需要提供的三件事。
pub trait CliProvider: Send + Sync + 'static {
    /// 官方类型（registry 的键）。
    fn kind(&self) -> AgentType;

    /// 静态描述。
    fn spec(&self) -> CliSpec;

    /// 运行期配置。
    fn config(&self) -> &CliCoreConfig;

    /// 为这次 run 造一个解码器。
    fn decoder_for(&self, request: &LaunchRequest) -> Box<dyn CliDecoder>;
}

/// 把 [`CliProvider`] 直接当 [`RuntimeAdapter`] 用。
///
/// 这样 provider 模块里不需要 `impl RuntimeAdapter`：它只描述"是什么"，
/// "怎么跑"由共享核心负责。`pi_local` 不实现 [`CliProvider`]，因此不受影响。
#[async_trait]
impl<P: CliProvider> RuntimeAdapter for P {
    fn kind(&self) -> AgentType {
        CliProvider::kind(self)
    }

    fn capabilities(&self) -> AdapterCapabilities {
        self.spec().capabilities.to_adapter(CliProvider::kind(self))
    }

    async fn launch(&self, request: LaunchRequest) -> Result<RunHandle, AdapterError> {
        let kind = CliProvider::kind(self);
        let spec = self.spec();
        let config = self.config();
        if request.prompt.trim().is_empty() {
            return Err(AdapterError::EmptyPrompt { kind });
        }
        let executable = resolve_executable(&config.executable).ok_or_else(|| {
            AdapterError::ExecutableUnavailable {
                kind,
                path: config.executable.clone(),
                reason: "不是可执行文件，且 PATH 上找不到".to_owned(),
            }
        })?;

        let args = (spec.build_args)(&request);
        let mut command = Command::new(&executable);
        command
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if spec.transport == PromptTransport::Argv {
            command.stdin(Stdio::null());
        } else {
            command.stdin(Stdio::piped());
        }
        if let Some(cwd) = request.cwd.as_deref() {
            command.current_dir(cwd);
            // opencode / codearts / deveco 系在解析项目根（AGENTS.md 向上查找、
            // skills / 项目配置扫描）时**先读 PWD 再退到进程 cwd**，只改
            // `current_dir` 不够（上游同款覆盖 `PWD`）。对其余 CLI 而言这恰好是
            // `cd <cwd> && cmd` 的语义，无害。
            command.env("PWD", cwd);
        }
        // 上游 `buildEnv` = 继承进程环境 + 请求里的覆盖项（`Command` 默认就继承）。
        for (key, value) in &request.env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|source| AdapterError::Spawn {
            kind,
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
        register_cancel_slot(request.run_id.clone(), cancel_tx);

        let decoder = self.decoder_for(&request);
        let context = CliRunContext {
            run_id: request.run_id.clone(),
            kind,
            label: spec.label,
            transport: spec.transport,
            prompt: request.prompt,
            prompt_write_is_fatal: spec.prompt_write_is_fatal,
            executable,
            timeout: request.timeout.unwrap_or(config.default_timeout),
            stream_drain_grace: config.stream_drain_grace,
            started_at: Instant::now(),
            events: Some(events),
            outcome: Some(outcome_tx),
        };
        tokio::spawn(
            CliRun::new(context, decoder, cancel_rx).execute(child, stdin, stdout, stderr, pid),
        );

        Ok(RunHandle::new(request.run_id, events_rx, outcome_rx))
    }

    async fn cancel(&self, run_id: &RunId) -> Result<CancelOutcome, AdapterError> {
        Ok(cancel_run(run_id))
    }

    async fn probe_version(&self) -> Result<VersionProbe, AdapterError> {
        let kind = CliProvider::kind(self);
        let config = self.config();
        let executable = resolve_executable(&config.executable).ok_or_else(|| {
            AdapterError::ExecutableUnavailable {
                kind,
                path: config.executable.clone(),
                reason: "不是可执行文件，且 PATH 上找不到".to_owned(),
            }
        })?;
        let mut command = Command::new(&executable);
        command.arg("--version").kill_on_drop(true);
        let output =
            match tokio::time::timeout(config.version_probe_timeout, command.output()).await {
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
                            config.version_probe_timeout
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
        // 无请求上下文的解码器（一致性套件的脏数据回放用）：模型名等上下文缺省。
        self.decoder_for(&LaunchRequest::new(""))
    }
}

/// 跨 provider 共享的取消槽位表：`run_id` 全局唯一，因此不需要按 provider 分表。
fn cancel_slots() -> &'static Mutex<HashMap<RunId, watch::Sender<bool>>> {
    static SLOTS: OnceLock<Mutex<HashMap<RunId, watch::Sender<bool>>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 登记一个取消槽位（`run_id` 撞车时后到的覆盖先到的，实际不会发生）。
pub(crate) fn register_cancel_slot(run_id: RunId, tx: watch::Sender<bool>) {
    let mut slots = cancel_slots()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    slots.insert(run_id, tx);
}

/// run 收尾时摘掉自己的槽位（必须在终态发出**之前**，否则迟到的 `cancel()`
/// 会拿到 `Signalled` 而进程其实已经结束）。
pub(crate) fn release_cancel_slot(run_id: &RunId) {
    let mut slots = cancel_slots()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    slots.remove(run_id);
}

/// 取消一次 run（幂等）。
pub fn cancel_run(run_id: &RunId) -> CancelOutcome {
    let slots = cancel_slots()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match slots.get(run_id) {
        Some(tx) if tx.send(true).is_ok() => CancelOutcome::Signalled,
        _ => CancelOutcome::NotRunning,
    }
}

/// 当前登记的 run 数（日志/诊断用）。
pub fn active_runs() -> usize {
    cancel_slots()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .len()
}

/// 解析可执行文件（与 `pi_local::resolve_executable` 同款；那边是私有的，
/// 这里保留一份，`docs/33` 记了这处重复）。
pub fn resolve_executable(configured: &Path) -> Option<PathBuf> {
    if configured.is_absolute() || configured.components().count() > 1 {
        return configured.is_file().then_some(configured.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(configured))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_executable_accepts_paths_and_path_lookups() {
        assert_eq!(
            resolve_executable(Path::new("/bin/sh")).map(|p| p.display().to_string()),
            Some("/bin/sh".to_owned())
        );
        assert!(resolve_executable(Path::new("/definitely/not/here")).is_none());
        // 裸名字走 PATH（`sh` 一定在）。
        assert!(resolve_executable(Path::new("sh")).is_some());
    }

    #[tokio::test]
    async fn cancel_unknown_run_is_not_running() {
        let unknown = RunId::new();
        assert_eq!(cancel_run(&unknown), CancelOutcome::NotRunning);
    }

    #[tokio::test]
    async fn cancel_slot_signals_once_then_releases() {
        let run_id = RunId::new();
        let (tx, mut rx) = watch::channel(false);
        register_cancel_slot(run_id.clone(), tx);
        assert!(rx.has_changed().is_ok());
        assert_eq!(cancel_run(&run_id), CancelOutcome::Signalled);
        assert!(rx.changed().await.is_ok());
        assert!(*rx.borrow());
        release_cancel_slot(&run_id);
        assert_eq!(cancel_run(&run_id), CancelOutcome::NotRunning);
    }
}
