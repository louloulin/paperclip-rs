//! adapter 契约：`RuntimeAdapter` trait 本体，以及它与调用方交换的全部值类型。
//!
//! 五个关注点在 trait 上的落点（plan1 R4 的"统一抽象"就是这一张表）：
//!
//! | 关注点 | 落点 |
//! |---|---|
//! | launch | [`RuntimeAdapter::launch`] → [`RunHandle`] |
//! | stream | [`RuntimeAdapter::decoder`]（协议解码缝）+ [`RunHandle::next_event`] |
//! | cancel | [`RuntimeAdapter::cancel`] |
//! | probe-version | [`RuntimeAdapter::probe_version`] |
//! | capabilities | [`RuntimeAdapter::capabilities`] |
//!
//! 设计取舍（为什么是这些缝）：
//!
//! - **`decoder()` 返回状态机而不是一个纯函数**：pi 的 stdout 是"逐行 JSON 事件"，
//!   但单行解码不够 —— `turn_end` 要累加用量、`turn_start` 要清掉上一 turn 的错误、
//!   文本增量要跨行缓冲（半个控制 token 不能提前吐给用户）。把这件事做成可注入的
//!   [`EventDecoder`]，一致性套件就能**不启进程**地回放一段真实 transcript 断言事件序列。
//! - **`launch` 立刻返回 [`RunHandle`]**，而不是返回一个 `Stream`：调用方要能在流式读取的
//!   同时拿终态（退出码 / `failure_reason`），也要能在 `cancel` 之外不阻塞地等结果。
//!   `RunHandle` 的两半（事件 + 终态）各自独立，谁先到都不丢。
//! - **`RunOutcome.failure_reason` 用字符串与 M3-3（`mc-task`，LUM-1409）完全一致**，
//!   见 [`FailureReason::as_str`]；本 crate **不依赖** `mc-task`，两个并行切片各自定义枚举，
//!   靠这张字符串表对齐。

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::catalog::AgentType;

/// `RunOutcome.stderr_tail` 的字节上限（按字符边界截断）。
pub const STDERR_TAIL_LIMIT: usize = 4096;

/// 流式事件通道发送端（**无界**）。
///
/// 为什么不用有界通道 + `.await` 背压：阻塞在 `send` 上的 run 连 `cancel` 都响应不了
/// （取消信号只能被 select 分支看到，而 run 正好卡在 send 上）。代价是调用方完全不读时
/// 事件会在内存里堆到该 run 结束。契约因此要求调用方**要么消费事件**
/// （[`RunHandle::next_event`] / [`RunHandle::drain`]），**要么调 [`RunHandle::outcome`]**
/// —— 后者会 drop 接收端，之后 `send` 立刻返回 `Err`，adapter 停止发送。
pub type EventSender = mpsc::UnboundedSender<RuntimeEvent>;

/// 流式事件接收端。
pub type EventReceiver = mpsc::UnboundedReceiver<RuntimeEvent>;

// ── Run 标识 ──

/// 一次 adapter 执行的标识（进程内的运行句柄，不是 DB 主键；落库是 M3-3 的事）。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    /// 生成 `run_<uuid-simple>`。
    pub fn new() -> Self {
        let uuid = uuid::Uuid::new_v4().simple();
        Self(format!("run_{uuid}"))
    }

    /// 直接取字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for RunId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

// ── 终态 ──

/// run 的终态。与上游 `agent.Result.Status` 对齐（`completed` / `failed` /
/// `timeout` / `aborted`），其中 `aborted` 在本 crate 叫 [`RunStatus::Cancelled`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// 进程正常退出且没有任何 provider/turn 级错误。
    Completed,
    /// 进程非零退出，或进程 0 退出但 pi 报了一个未恢复的 turn 错误。
    Failed,
    /// 调用方 `cancel` 打断了这次 run（且没有更权威的 provider 错误）。
    Cancelled,
    /// 超过 `LaunchRequest::timeout`（或 adapter 的默认超时）。
    Timeout,
}

/// 失败归因。**取值字符串与 M3-3（`mc-task`）的表 1:1 对齐**。
///
/// `RuntimeRecovery` 不由 adapter 产生（进程内看不出来）：它属于"租约过期 + 重试"
/// 的判定，落在 M3-3/M3-7。这里保留取值，是为了让 M3-3 直接 `as_str()` 落库。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    /// agent 自己失败：非零退出、turn 级 provider 错误、协议错误。
    AgentError,
    /// 执行超时。
    Timeout,
    /// 运行时不可达：可执行文件缺失 / spawn 失败 / 会话文件不可写。
    RuntimeOffline,
    /// 租约过期后的恢复尝试（M3-3 产生，adapter 不产生）。
    RuntimeRecovery,
    /// 人工取消 / 看门狗取消。
    Manual,
}

impl FailureReason {
    /// 全部 5 类，顺序即 `mc-task` 表的顺序。
    pub const ALL: [Self; 5] = [
        Self::AgentError,
        Self::Timeout,
        Self::RuntimeOffline,
        Self::RuntimeRecovery,
        Self::Manual,
    ];

    /// 落库/上报用的字符串（= `mc-task` 的 `failure_reason` 取值）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentError => "agent_error",
            Self::Timeout => "timeout",
            Self::RuntimeOffline => "runtime_offline",
            Self::RuntimeRecovery => "runtime_recovery",
            Self::Manual => "manual",
        }
    }

    /// 反向解析。
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|r| r.as_str() == value)
    }
}

impl fmt::Display for FailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── 用量 ──

/// token 用量（字段同 pi 的 `usage` 对象：`input` / `output` / `cacheRead` /
/// `cacheWrite` / `totalTokens`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// 上游自己给的 `totalTokens`（**不是** input+output：缓存命中另算）。
    pub total_tokens: u64,
}

impl TokenUsage {
    /// 全零。
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, rhs: Self) {
        self.input += rhs.input;
        self.output += rhs.output;
        self.cache_read += rhs.cache_read;
        self.cache_write += rhs.cache_write;
        self.total_tokens += rhs.total_tokens;
    }
}

/// 按模型聚合的用量（`RunOutcome::usage` 的元素）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model: String,
    pub usage: TokenUsage,
}

// ── 事件 ──

/// stdout 解码出来的流式事件。
///
/// 与上游 `agent.Message` 的对应：`Progress` ↔ `MessageStatus`、`Text` ↔
/// `MessageText`、`Thinking` ↔ `MessageThinking`、`ToolUse` ↔ `MessageToolUse`、
/// `ToolResult` ↔ `MessageToolResult`、`Error` ↔ `MessageError`；`Started` 与
/// `Usage` 是本 crate 为"可观测的 launch/计费"补的两条。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeEvent {
    /// 进程已 spawn（launch 成功后第一条事件）。
    Started {
        executable: String,
        pid: Option<u32>,
    },
    /// 生命周期/进度（pi 的 `agent_start` → `"running"`）。
    Progress { status: String },
    /// 助手正文增量（已过消毒：控制 token / 结构化工具标记不会漏给用户）。
    Text { delta: String },
    /// 推理增量。
    Thinking { delta: String },
    /// 工具调用开始。
    ToolUse {
        call_id: String,
        tool: String,
        input: serde_json::Value,
    },
    /// 工具调用结束。
    ToolResult {
        call_id: String,
        output: String,
        is_error: bool,
    },
    /// 协议级错误（**不必然**终止 run：pi 会在自动重试前先报一次）。
    Error { message: String },
    /// 一个 turn 结束时上报的用量增量（`RunOutcome::usage` 是各 turn 的累加）。
    Usage { model: String, usage: TokenUsage },
}

impl RuntimeEvent {
    /// 事件名（日志/断言用）。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::Progress { .. } => "progress",
            Self::Text { .. } => "text",
            Self::Thinking { .. } => "thinking",
            Self::ToolUse { .. } => "tool_use",
            Self::ToolResult { .. } => "tool_result",
            Self::Error { .. } => "error",
            Self::Usage { .. } => "usage",
        }
    }
}

// ── 请求 / 能力 / 版本 ──

/// 一次 launch 的入参。
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    /// 运行标识（`launch` 之前就定，便于调用方先建立映射）。
    pub run_id: RunId,
    /// 任务 prompt（pi 走 **stdin**，不是 argv）。
    pub prompt: String,
    /// 工作目录；`None` 用进程 cwd。
    pub cwd: Option<PathBuf>,
    /// 模型（透传给 CLI 的 `--model` 等）。
    pub model: Option<String>,
    /// 思考等级（pi 的 `--thinking`）。
    pub thinking_level: Option<String>,
    /// 续跑指定会话（pi 的 `--session <path>` 的 path 就是会话 id）。
    pub resume_session: Option<String>,
    /// 超时；`None` 用 adapter 的默认值。
    pub timeout: Option<Duration>,
    /// 追加环境变量（叠加在继承的环境之上）。
    pub env: BTreeMap<String, String>,
    /// 追加 CLI 参数（adapter 必须过滤掉自己管理的参数）。
    pub extra_args: Vec<String>,
}

impl LaunchRequest {
    /// 最小请求：只给 prompt，其余留空。
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            run_id: RunId::new(),
            prompt: prompt.into(),
            cwd: None,
            model: None,
            thinking_level: None,
            resume_session: None,
            timeout: None,
            env: BTreeMap::new(),
            extra_args: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_run_id(mut self, run_id: RunId) -> Self {
        self.run_id = run_id;
        self
    }

    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    #[must_use]
    pub fn with_thinking_level(mut self, level: impl Into<String>) -> Self {
        self.thinking_level = Some(level.into());
        self
    }

    #[must_use]
    pub fn with_resume_session(mut self, session: impl Into<String>) -> Self {
        self.resume_session = Some(session.into());
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    #[must_use]
    pub fn with_extra_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extra_args = args.into_iter().map(Into::into).collect();
        self
    }
}

/// 线协议族。本片只给 pi 定族（[`ProtocolFamily::JsonLine`] = 逐行 JSON 事件）；
/// 其余 24 项在 M3-8 落地时按上游 `launchHeaders` 的骨架归族
/// （`(stream-json)` → [`ProtocolFamily::StreamJson`]、`app-server` →
/// [`ProtocolFamily::AppServer`]、`acp` → [`ProtocolFamily::Acp`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolFamily {
    /// 逐行 JSON 事件（pi `-p --mode json`）。
    JsonLine,
    /// Claude 风格 `stream-json`。
    StreamJson,
    /// Codex `app-server`。
    AppServer,
    /// ACP（Agent Client Protocol）。
    Acp,
    /// 未归类 / 非结构化。
    Opaque,
}

/// adapter 自述能力。M3-4 的 runtime 台账、M3-7 的守护进程选择都读它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // 能力位就是一组 bool，拆成状态机只会更难读
pub struct AdapterCapabilities {
    /// 线协议族。
    pub protocol: ProtocolFamily,
    /// stdout 上是否逐条增量（false = 只有终态）。
    pub streaming: bool,
    /// 是否流式给推理增量。
    pub thinking: bool,
    /// 是否给工具调用事件。
    pub tool_events: bool,
    /// 是否上报 token 用量。
    pub usage_reporting: bool,
    /// 是否支持续跑指定会话。
    pub resume: bool,
    /// 是否支持 `--version` 探测。
    pub version_probe: bool,
    /// 用户可见的启动骨架（= [`AgentType::launch_header`]，一致性套件会断言两者相等）。
    pub launch_header: String,
}

/// semver 三段（上游只比较 major/minor/patch）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Semver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Semver {
    /// 取字符串里**第一个** `\d+\.\d+\.\d+`（允许 `v` 前缀、允许前后有别的文本）——
    /// 对齐上游 `versionRe` 的 `FindStringSubmatch` 语义。
    pub fn parse(raw: &str) -> Option<Self> {
        let bytes = raw.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if !bytes[i].is_ascii_digit() {
                i += 1;
                continue;
            }
            let start = i;
            let mut parts = [0_u64; 3];
            let mut matched = true;
            for (idx, part) in parts.iter_mut().enumerate() {
                let begin = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == begin {
                    matched = false;
                    break;
                }
                // 位数上限 18：远超真实版本号，同时让 `parse::<u64>` 不可能溢出。
                if i - begin > 18 {
                    matched = false;
                    break;
                }
                *part = raw[begin..i].parse().ok()?;
                if idx < 2 {
                    if i < bytes.len() && bytes[i] == b'.' {
                        i += 1;
                    } else {
                        matched = false;
                        break;
                    }
                }
            }
            if matched {
                return Some(Self {
                    major: parts[0],
                    minor: parts[1],
                    patch: parts[2],
                });
            }
            // 不是版本号：从数字串后一位继续扫，避免死循环。
            i = start + 1;
        }
        None
    }
}

impl fmt::Display for Semver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// `probe_version` 的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionProbe {
    /// 被探测的 adapter 类型。
    pub kind: AgentType,
    /// 实际执行的文件（PATH 解析后的绝对路径）。
    pub executable: PathBuf,
    /// CLI 原样输出（首行）。
    pub raw: String,
    /// 解析出的版本；CLI 输出里没有三段版本号时为 `None`（**不算**探测失败 ——
    /// 探测成功但格式未知，与"探测失败"是两件事）。
    pub version: Option<Semver>,
}

/// `cancel` 的结果。**幂等**：对未知/已结束的 run 调用返回
/// [`CancelOutcome::NotRunning`] 且 `Ok` —— 取消一个已经结束的东西不是错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    /// 已向该 run 发出取消信号（进程会被杀，run 终态是 `cancelled`）。
    Signalled,
    /// 该 run 不在运行中（已完成 / 未知）。
    NotRunning,
}

// ── 终态结果 ──

/// 一次 run 的终态。**只在进程真的结束（或被 cancel/timeout 打断）之后**产生。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunOutcome {
    /// 与 `LaunchRequest::run_id` 相同。
    pub run_id: RunId,
    /// 终态。
    pub status: RunStatus,
    /// 进程退出码；被信号杀死时为 `None`。
    pub exit_code: Option<i32>,
    /// 助手正文全文（各 turn 的文本增量拼接）。
    pub output: String,
    /// 失败说明（`status == Completed` 时为 `None`）。
    pub error: Option<String>,
    /// 失败归因（`status == Completed` 时为 `None`）。
    pub failure_reason: Option<FailureReason>,
    /// 会话 id（pi 就是 session 文件路径，供后续 `resume_session` 用）。
    pub session_id: Option<String>,
    /// 各模型用量累加（按模型名排序，稳定输出）。
    pub usage: Vec<ModelUsage>,
    /// 墙钟耗时（含 spawn 到 reaped）。
    pub duration_ms: u64,
    /// 子进程 stderr 的**尾部**（最多 [`STDERR_TAIL_LIMIT`] 字节），诊断用。
    ///
    /// 上游只把它写日志；这里带上是因为本 crate 没有日志注入点，而"进程非零退出 + 一句
    /// 人话"恰恰是排障时最需要的东西。**不参与**错误串拼接（错误串形状与上游一致）。
    pub stderr_tail: String,
}

impl RunOutcome {
    /// `status == Completed` 的便捷判断。
    pub fn is_success(&self) -> bool {
        self.status == RunStatus::Completed
    }

    /// 全部模型用量的合计。
    pub fn total_usage(&self) -> TokenUsage {
        self.usage.iter().fold(TokenUsage::default(), |mut acc, u| {
            acc += u.usage;
            acc
        })
    }
}

// ── 错误 ──

/// adapter 自身的错误（**不是**"run 失败了" —— run 失败在 [`RunOutcome`] 里）。
///
/// `runtime_offline` 类语义全在这一层：可执行文件缺失 / spawn 失败 / 会话文件不可写，
/// 调用方（M3-7）把它们映射成 [`FailureReason::RuntimeOffline`] 并跳过 retry。
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("{kind} 可执行文件不可用（{path}）：{reason}")]
    ExecutableUnavailable {
        kind: AgentType,
        path: PathBuf,
        reason: String,
    },
    #[error("spawn {kind} 失败（{path}）：{source}")]
    Spawn {
        kind: AgentType,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{kind} 的 prompt 不能为空")]
    EmptyPrompt { kind: AgentType },
    #[error("{kind} 的会话文件正被占用：{path}")]
    SessionBusy { kind: AgentType, path: PathBuf },
    #[error("{kind} 的会话文件不可用（{path}）：{source}")]
    SessionIo {
        kind: AgentType,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{kind} 探测版本失败：{reason}")]
    VersionProbe { kind: AgentType, reason: String },
    #[error("run {run_id} 的终态通道已关闭（adapter 任务提前结束）")]
    OutcomeLost { run_id: RunId },
}

impl AdapterError {
    /// 该错误是否代表"运行时不在"（M3-7 据此落 `runtime_offline`）。
    pub fn is_runtime_offline(&self) -> bool {
        matches!(
            self,
            Self::ExecutableUnavailable { .. }
                | Self::Spawn { .. }
                | Self::SessionBusy { .. }
                | Self::SessionIo { .. }
        )
    }
}

// ── 句柄 / 解码缝 / trait ──
//
// 这三块各占一个子模块，纯粹为了守住 `scripts/file_size_check.py` 的 800 行上限
// （gate ⑩）。`pub use` 保证 `crate::adapter::*` 的对外面完全不变。
mod handle;
mod traits;

pub use handle::RunHandle;
pub use traits::{EventDecoder, RuntimeAdapter};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_reason_strings_match_mc_task_table() {
        // 这 5 个字符串是 M3-2 ↔ M3-3 的接口契约，改动必须两边同时改。
        let table: [(&str, FailureReason); 5] = [
            ("agent_error", FailureReason::AgentError),
            ("timeout", FailureReason::Timeout),
            ("runtime_offline", FailureReason::RuntimeOffline),
            ("runtime_recovery", FailureReason::RuntimeRecovery),
            ("manual", FailureReason::Manual),
        ];
        for (name, reason) in table {
            assert_eq!(reason.as_str(), name);
            assert_eq!(FailureReason::parse(name), Some(reason));
        }
        assert_eq!(FailureReason::parse("nope"), None);
    }

    #[test]
    fn run_id_is_prefixed_and_unique() {
        let a = RunId::new();
        let b = RunId::new();
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("run_"));
        assert_eq!(RunId::from(a.as_str().to_owned()), a);
    }

    #[test]
    fn semver_takes_first_triplet() {
        assert_eq!(
            Semver::parse("0.83.1"),
            Some(Semver {
                major: 0,
                minor: 83,
                patch: 1
            })
        );
        assert_eq!(Semver::parse("pi v0.83.1 (build 2)").unwrap().minor, 83);
        assert_eq!(
            Semver::parse("multica-build 2026.9.22 then 1.2.3").unwrap(),
            Semver {
                major: 2026,
                minor: 9,
                patch: 22
            }
        );
        assert_eq!(Semver::parse("no version here"), None);
        assert_eq!(Semver::parse(""), None);
        assert_eq!(Semver::parse("1.2"), None);
        // 长数字串不能让解析溢出。
        assert_eq!(Semver::parse(&"9".repeat(40)), None);
        assert_eq!(Semver::parse("1.2.3").unwrap().to_string(), "1.2.3");
    }

    #[test]
    fn token_usage_accumulates_per_model() {
        let mut total = TokenUsage::default();
        assert!(total.is_empty());
        total += TokenUsage {
            input: 10,
            output: 2,
            cache_read: 1,
            cache_write: 3,
            total_tokens: 16,
        };
        total += TokenUsage {
            input: 1,
            output: 1,
            ..TokenUsage::default()
        };
        assert_eq!(
            total,
            TokenUsage {
                input: 11,
                output: 3,
                cache_read: 1,
                cache_write: 3,
                total_tokens: 16
            }
        );
        assert!(!total.is_empty());
    }

    #[test]
    fn runtime_offline_classification() {
        let err = AdapterError::ExecutableUnavailable {
            kind: AgentType::Pi,
            path: PathBuf::from("/nope/pi"),
            reason: "not found".into(),
        };
        assert!(err.is_runtime_offline());
        assert!(!AdapterError::EmptyPrompt {
            kind: AgentType::Pi
        }
        .is_runtime_offline());
        assert_eq!(
            AdapterError::EmptyPrompt {
                kind: AgentType::Pi
            }
            .to_string(),
            "pi 的 prompt 不能为空"
        );
    }
}
