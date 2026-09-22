//! 重试域：`attempt` / `max_attempts` / `parent_task_id` / `failure_reason` + 自动重试判定。
//!
//! # 真值来源
//!
//! 1. **粗分类 5 桶**（本模块 [`FailureClass`]）：上游迁移
//!    `055_task_lease_and_retry.up.sql` 逐字给出 ——
//!    「`failure_reason` -- coarse classifier set when status flips to failed:
//!    `'agent_error'`, `'timeout'`, `'runtime_offline'`, `'runtime_recovery'`, `'manual'`.
//!    The auto-retry path uses this to decide whether to spawn a child task.」
//!    同一迁移给出 `attempt INT NOT NULL DEFAULT 1`、`max_attempts INT NOT NULL DEFAULT 2`
//!    （「1 disables retry」）、`parent_task_id UUID REFERENCES agent_task_queue(id) ON DELETE SET NULL`。
//!
//! 2. **细分值**（本模块 [`FailureReason`]）：上游 `server/pkg/taskfailure/failure.go`
//!    是 `failure_reason` 列的**唯一**规范值表（27 个：13 个平台侧 + 14 个
//!    `agent_error.*`，见其 `allReasons`）。MUL-1949 之后服务端/daemon 写的就是这些值 ——
//!    也就是说 055 的 `agent_error` 桶被细化成了 14 个 `agent_error.<sub>`。
//!
//! 3. **自动重试判定**：上游服务层 `server/internal/service/task.go` 的
//!    `retryableReasons`（决定「这个原因能不能自动重试」）、`retryAttemptCeiling`
//!    （reason-aware 上限，只放大、绝不复活 `max_attempts<=1`）、
//!    `retryDelayForAttempt`（`runtime_offline` 延迟 1s、`provider_network` 末次延迟 5s）、
//!    以及 `retryEligible` 的其余闸门（autopilot / triage / 必须挂在 issue 或 chat 上）。
//!
//! # 本仓自造列一律不用
//!
//! `docs/15` §2.2 列的 7 个本仓自造列（`retry_count` / `source_task_id` /
//! `terminal_completed_at` / `delegated_failure_evidence` / `initiator_user_id` /
//! `retired_session_id` / `lease_expires_at`）在本模块里**没有任何**对应项：
//! 重试次数是 `attempt`，父指针是 `parent_task_id`（向上游
//! `delegated_from_task_id` / `escalation_for_task_id` 另有两列，见 `state`）。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::TaskError;
use crate::status::TaskStatus;

/// `attempt` 的上游默认值（055：`attempt INT NOT NULL DEFAULT 1`）。
pub const DEFAULT_ATTEMPT: u32 = 1;

/// `max_attempts` 的上游默认值（055：`max_attempts INT NOT NULL DEFAULT 2`
/// —— 首跑 + 一次自动重试）。
pub const DEFAULT_MAX_ATTEMPTS: u32 = 2;

/// `max_attempts <= 该值` 显式关闭自动重试（055 注释「1 disables retry」）。
pub const RETRY_DISABLED_MAX_ATTEMPTS: u32 = 1;

/// `provider_network` 的专用上限（上游 `providerNetworkMaxAttempts`）。
pub const PROVIDER_NETWORK_MAX_ATTEMPTS: u32 = 3;

/// `provider_network` 末次重试的冷却秒数（上游 `providerNetworkFinalRetryWait`）。
pub const PROVIDER_NETWORK_FINAL_RETRY_WAIT_SECS: u64 = 5;

/// `runtime_offline` 重试的延迟秒数（上游 `runtimeOfflineRetryDeferral`）。
/// 正值是为了让它走「健康门控提升」而不是立刻可 claim。
pub const RUNTIME_OFFLINE_RETRY_DEFERRAL_SECS: u64 = 1;

/// 055 的粗分类桶 —— **恰好 5 个值，逐字来自上游迁移 `055`**。
///
/// 它只描述「失败的大类」，不决定是否重试：上游的自动重试判定读的是
/// [`FailureReason::is_retryable`]（`retryableReasons`），不是这个桶。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// `agent_error`：agent 进程侧失败（细化后是 14 个 `agent_error.*`）。
    AgentError,
    /// `timeout`：平台/运行时侧的硬超时或预算超限。
    Timeout,
    /// `runtime_offline`：runtime 掉线、重连失败。
    RuntimeOffline,
    /// `runtime_recovery`：运行时侧不可恢复，需环境/运行时修复后重来。
    RuntimeRecovery,
    /// `manual`：人工终止。
    Manual,
}

impl FailureClass {
    /// 5 个桶，顺序与上游 055 的注释一致。
    pub const ALL: [Self; 5] = [
        Self::AgentError,
        Self::Timeout,
        Self::RuntimeOffline,
        Self::RuntimeRecovery,
        Self::Manual,
    ];

    /// 线上字符串（= 055 的 `failure_reason` 粗值）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentError => "agent_error",
            Self::Timeout => "timeout",
            Self::RuntimeOffline => "runtime_offline",
            Self::RuntimeRecovery => "runtime_recovery",
            Self::Manual => "manual",
        }
    }

    /// 解析粗分类字符串。
    ///
    /// # Errors
    ///
    /// 不是 055 的 5 个值时返回 [`TaskError::UnknownFailureClass`]。
    pub fn parse(s: &str) -> Result<Self, TaskError> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == s)
            .ok_or_else(|| TaskError::unknown_failure_class(s))
    }

    /// 只有 [`FailureClass::Manual`] 是人工行为 —— 自动重试判定必须对它的类恒为 false。
    #[must_use]
    pub const fn is_human_initiated(self) -> bool {
        matches!(self, Self::Manual)
    }
}

impl fmt::Display for FailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `agent_task_queue.failure_reason` 的规范化值。
///
/// 前 27 个变体与上游 `pkg/taskfailure` 的 `allReasons` 逐个对应（顺序亦一致），
/// 之后是本仓需要、但上游以**裸字符串**出现在其它文件里的 3 个值，
/// 最后是 055 粗分类里的 `manual`。
///
/// 命名约定：`Agent*` 前缀 = 线上串带 `agent_error.` 前缀（上游 `IsAgentError`
/// 就是按前缀判定，所以这里也按前缀对齐命名，避免逐个人工分类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum FailureReason {
    // ---------- 平台 / 调度器侧（13）----------
    /// `queued_expired`：在 `queued` 停到 TTL 仍无人 claim（`ExpireStaleQueuedTasks`）。
    QueuedExpired,
    /// `runtime_offline`：持有任务的 runtime 掉线（`FailTasksForOfflineRuntimes`）。
    RuntimeOffline,
    /// `runtime_reconnect_timeout`：等 runtime 复活的 `deferred` 重试耗尽重连宽限
    /// （`FailExpiredRuntimeReconnectRetries`）。不可重试。
    RuntimeReconnectTimeout,
    /// `runtime_recovery`：daemon 重启，上一会话不可恢复（`RecoverOrphanedTasksForRuntime`）。
    RuntimeRecovery,
    /// `timeout`：服务端或运行时侧硬超时（`FailStaleTasks` 等）。
    Timeout,
    /// `iteration_limit`：agent 打到单跑迭代上限（平台强加的预算，故归平台侧）。
    IterationLimit,
    /// `agent_blocked`：agent 主动进入 `blocked`，请求人工输入。不是系统错误。
    AgentBlocked,
    /// `api_invalid_request`：上游 LLM API 以 400 `invalid_request_error` 拒绝请求体。
    /// 会话历史已被污染，重跑必须换新会话。
    ApiInvalidRequest,
    /// `skill_bundle_unavailable`：daemon 下载 agent 技能包失败，agent 进程从未启动。
    /// 可重试且便宜（已下好的包在磁盘缓存里）。
    SkillBundleUnavailable,
    /// `runtime_cli_timeout`：daemon 准备阶段调用的本地 CLI 超时。**不可重试**
    /// （本地确定性卡顿，重试只会再付一次同样的墙钟）。
    RuntimeCliTimeout,
    /// `environment_prepare_failed`：本机环境（工作目录 / overlay home）建不起来。
    EnvironmentPrepareFailed,
    /// `invalid_task_identity`：任务身份非法。
    InvalidTaskIdentity,
    /// `runtime_access_denied`：runtime 侧拒绝访问。
    RuntimeAccessDenied,

    // ---------- agent 进程侧：provider 错误（5）----------
    /// `agent_error.provider_auth_or_access`。
    AgentProviderAuthOrAccess,
    /// `agent_error.provider_quota_limit`。
    AgentProviderQuotaLimit,
    /// `agent_error.provider_capacity_or_rate_limit`。
    AgentProviderCapacityOrRateLimit,
    /// `agent_error.provider_server_error`。
    AgentProviderServerError,
    /// `agent_error.provider_network`：provider 流被瞬断。**可重试**，且有专用三级排程。
    AgentProviderNetwork,

    // ---------- agent 进程侧：agent / runner 错误（8）----------
    /// `agent_error.process_failure`。
    AgentProcessFailure,
    /// `agent_error.empty_or_unparseable_output`。
    AgentEmptyOrUnparseableOutput,
    /// `agent_error.agent_timeout`。
    AgentTimeout,
    /// `agent_error.context_overflow`：会话超长。重跑必须换新会话。
    AgentContextOverflow,
    /// `agent_error.missing_config`。
    AgentMissingConfig,
    /// `agent_error.model_not_found_or_unavailable`。
    AgentModelNotFoundOrUnavailable,
    /// `agent_error.runtime_version_unsupported`。
    AgentRuntimeVersionUnsupported,
    /// `agent_error.runtime_missing_executable`。
    AgentRuntimeMissingExecutable,

    /// `agent_error.unknown`：兜底桶。上游 `Classify` 分不出来时的落点。
    AgentUnknown,

    // ---------- 上游以裸字符串出现在服务层 / daemon 的值（3）----------
    /// `codex_semantic_inactivity`：codex 语义静默。上游 `retryableReasons` 的成员
    /// （不在 `taskfailure` 的 27 个里），且属 resume 黑名单。
    CodexSemanticInactivity,
    /// `agent_fallback_message`：agent 发出兜底文案。属 resume 黑名单。
    AgentFallbackMessage,
    /// `codex_resume_oversized`：codex rollout 只增不减，resume 必然再溢出。不可重试。
    CodexResumeOversized,

    // ---------- 055 粗分类里的第 5 桶 ----------
    /// `manual`：人工终止。上游**不把它写进 `failure_reason` 列** ——
    /// 用户取消走 `status='cancelled'` + `cancelled_by_type='user'`（迁移 `458`）。
    /// 保留本变体是为了让 055 的 5 分类可被完整表达与断言（见
    /// [`FailureReason::is_persisted_in_failure_reason_column`]）。
    Manual,
}

impl FailureReason {
    /// 上游 `taskfailure.allReasons` 的 27 个值（顺序**逐字**相同，便于与
    /// `failure.go` 对照核对）+ 平台侧两个值（`codex_semantic_inactivity`、
    /// `manual`），共 29。
    ///
    /// **不是**枚举的全部：另有 `agent_fallback_message` / `codex_resume_oversized`
    /// 两个平台侧值（同样能 `parse` / `as_str` / 写库），只是不参与
    /// 「上游 27 个」的逐字比对，所以不在这个列表里。
    pub const ALL: [Self; 29] = [
        Self::QueuedExpired,
        Self::RuntimeOffline,
        Self::RuntimeReconnectTimeout,
        Self::RuntimeRecovery,
        Self::Timeout,
        Self::IterationLimit,
        Self::AgentBlocked,
        Self::ApiInvalidRequest,
        Self::SkillBundleUnavailable,
        Self::RuntimeCliTimeout,
        Self::EnvironmentPrepareFailed,
        Self::InvalidTaskIdentity,
        Self::RuntimeAccessDenied,
        Self::AgentProviderAuthOrAccess,
        Self::AgentProviderQuotaLimit,
        Self::AgentProviderCapacityOrRateLimit,
        Self::AgentProviderServerError,
        Self::AgentProviderNetwork,
        Self::AgentProcessFailure,
        Self::AgentEmptyOrUnparseableOutput,
        Self::AgentTimeout,
        Self::AgentContextOverflow,
        Self::AgentMissingConfig,
        Self::AgentModelNotFoundOrUnavailable,
        Self::AgentRuntimeVersionUnsupported,
        Self::AgentRuntimeMissingExecutable,
        Self::AgentUnknown,
        Self::CodexSemanticInactivity,
        Self::Manual,
    ];

    /// 线上字符串（写库值）。`CodexSemanticInactivity` 之后的 3 个是上游裸字符串，
    /// 它们**不是** `taskfailure` 的成员，但确实会出现在同一列里。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueuedExpired => "queued_expired",
            Self::RuntimeOffline => "runtime_offline",
            Self::RuntimeReconnectTimeout => "runtime_reconnect_timeout",
            Self::RuntimeRecovery => "runtime_recovery",
            Self::Timeout => "timeout",
            Self::IterationLimit => "iteration_limit",
            Self::AgentBlocked => "agent_blocked",
            Self::ApiInvalidRequest => "api_invalid_request",
            Self::SkillBundleUnavailable => "skill_bundle_unavailable",
            Self::RuntimeCliTimeout => "runtime_cli_timeout",
            Self::EnvironmentPrepareFailed => "environment_prepare_failed",
            Self::InvalidTaskIdentity => "invalid_task_identity",
            Self::RuntimeAccessDenied => "runtime_access_denied",
            Self::AgentProviderAuthOrAccess => "agent_error.provider_auth_or_access",
            Self::AgentProviderQuotaLimit => "agent_error.provider_quota_limit",
            Self::AgentProviderCapacityOrRateLimit => "agent_error.provider_capacity_or_rate_limit",
            Self::AgentProviderServerError => "agent_error.provider_server_error",
            Self::AgentProviderNetwork => "agent_error.provider_network",
            Self::AgentProcessFailure => "agent_error.process_failure",
            Self::AgentEmptyOrUnparseableOutput => "agent_error.empty_or_unparseable_output",
            Self::AgentTimeout => "agent_error.agent_timeout",
            Self::AgentContextOverflow => "agent_error.context_overflow",
            Self::AgentMissingConfig => "agent_error.missing_config",
            Self::AgentModelNotFoundOrUnavailable => "agent_error.model_not_found_or_unavailable",
            Self::AgentRuntimeVersionUnsupported => "agent_error.runtime_version_unsupported",
            Self::AgentRuntimeMissingExecutable => "agent_error.runtime_missing_executable",
            Self::AgentUnknown => "agent_error.unknown",
            Self::CodexSemanticInactivity => "codex_semantic_inactivity",
            Self::AgentFallbackMessage => "agent_fallback_message",
            Self::CodexResumeOversized => "codex_resume_oversized",
            Self::Manual => "manual",
        }
    }

    /// 解析线上字符串。
    ///
    /// # Errors
    ///
    /// 不是本 crate 认识的 31 个值时返回 [`TaskError::UnknownFailureReason`]
    /// （**不**回落到 `agent_error.unknown`：静默归类会污染失败趋势面板）。
    pub fn parse(s: &str) -> Result<Self, TaskError> {
        let all = [
            Self::ALL.as_slice(),
            &[Self::AgentFallbackMessage, Self::CodexResumeOversized],
        ]
        .concat();
        all.into_iter()
            .find(|candidate| candidate.as_str() == s)
            .ok_or_else(|| TaskError::UnknownFailureReason { got: s.to_owned() })
    }

    /// 是否 agent 进程侧（上游 `IsAgentError` 就是 `strings.HasPrefix(v, "agent_error.")`）。
    #[must_use]
    pub const fn is_agent_error(self) -> bool {
        matches!(
            self,
            Self::AgentProviderAuthOrAccess
                | Self::AgentProviderQuotaLimit
                | Self::AgentProviderCapacityOrRateLimit
                | Self::AgentProviderServerError
                | Self::AgentProviderNetwork
                | Self::AgentProcessFailure
                | Self::AgentEmptyOrUnparseableOutput
                | Self::AgentTimeout
                | Self::AgentContextOverflow
                | Self::AgentMissingConfig
                | Self::AgentModelNotFoundOrUnavailable
                | Self::AgentRuntimeVersionUnsupported
                | Self::AgentRuntimeMissingExecutable
                | Self::AgentUnknown
        )
    }

    /// 055 的 5 桶归类。
    ///
    /// 前 5 个桶是 055 的**原始**成员（`agent_error` / `timeout` / `runtime_offline` /
    /// `runtime_recovery` / `manual`）。055 之后上游新增的平台侧值在 5 桶里
    /// **没有官方对应项**，本 crate 给出如下明示的回落规则（逐条写在各变体上，
    /// 并在 `docs/19` 列出，不伪装成上游定义）：
    ///
    /// - 环境/运行时侧准备失败（`skill_bundle_unavailable` / `environment_prepare_failed` /
    ///   `runtime_access_denied` / `invalid_task_identity`）→ `RuntimeRecovery`
    ///   （与 agent 自身行为无关，需运行时或环境修复后重来）；
    /// - 平台强加的预算超限（`queued_expired` / `iteration_limit` / `runtime_cli_timeout`）
    ///   → `Timeout`；
    /// - provider 与 agent 会话侧（含 `agent_blocked` / `api_invalid_request`）→ `AgentError`。
    #[must_use]
    pub const fn class(self) -> FailureClass {
        match self {
            Self::AgentProviderAuthOrAccess
            | Self::AgentProviderQuotaLimit
            | Self::AgentProviderCapacityOrRateLimit
            | Self::AgentProviderServerError
            | Self::AgentProviderNetwork
            | Self::AgentProcessFailure
            | Self::AgentEmptyOrUnparseableOutput
            | Self::AgentTimeout
            | Self::AgentContextOverflow
            | Self::AgentMissingConfig
            | Self::AgentModelNotFoundOrUnavailable
            | Self::AgentRuntimeVersionUnsupported
            | Self::AgentRuntimeMissingExecutable
            | Self::AgentUnknown
            | Self::AgentBlocked
            | Self::ApiInvalidRequest
            | Self::CodexSemanticInactivity
            | Self::AgentFallbackMessage
            | Self::CodexResumeOversized => FailureClass::AgentError,

            Self::Timeout
            | Self::QueuedExpired
            | Self::IterationLimit
            | Self::RuntimeCliTimeout => FailureClass::Timeout,

            Self::RuntimeOffline | Self::RuntimeReconnectTimeout => FailureClass::RuntimeOffline,

            Self::RuntimeRecovery
            | Self::SkillBundleUnavailable
            | Self::EnvironmentPrepareFailed
            | Self::RuntimeAccessDenied
            | Self::InvalidTaskIdentity => FailureClass::RuntimeRecovery,

            Self::Manual => FailureClass::Manual,
        }
    }

    /// 自动重试资格 —— **逐字**来自上游 `service/task.go` 的 `retryableReasons`
    /// （6 个成员：`runtime_offline` / `runtime_recovery` / `timeout` /
    /// `codex_semantic_inactivity` / `agent_error.provider_network` /
    /// `skill_bundle_unavailable`）。
    ///
    /// 注意 `FailureClass::AgentError` 的**大部分**成员是**不可**重试的
    /// —— 「粗分类」与「能不能重试」不是同一张表，这正是 055 的 5 桶
    /// 不足以做判定的原因。
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::RuntimeOffline
                | Self::RuntimeRecovery
                | Self::Timeout
                | Self::CodexSemanticInactivity
                | Self::AgentProviderNetwork
                | Self::SkillBundleUnavailable
        )
    }

    /// 重试时**不能**复用 agent 会话（上游 `resumeUnsafeFailureReason` 的成员
    /// 与本 crate 建模值的交集）。
    ///
    /// 上游该集合是 6 个：`iteration_limit` / `agent_fallback_message` /
    /// `api_invalid_request` / `codex_semantic_inactivity` /
    /// `agent_error.context_overflow` / `codex_resume_oversized`。
    #[must_use]
    pub const fn is_resume_unsafe(self) -> bool {
        matches!(
            self,
            Self::IterationLimit
                | Self::AgentFallbackMessage
                | Self::ApiInvalidRequest
                | Self::CodexSemanticInactivity
                | Self::AgentContextOverflow
                | Self::CodexResumeOversized
        )
    }

    /// 这个值是否真的会被写进 `failure_reason` 列。
    ///
    /// 只有 [`FailureReason::Manual`] 为 `false`：上游不写这个值（055 的桶名保留了，
    /// 但实际路径走 `status='cancelled'` + `cancelled_by_type='user'`）。仓储层
    /// 在写库前必须查这个断言，否则会造出一个上游 `taskfailure` 不认识的列值。
    #[must_use]
    pub const fn is_persisted_in_failure_reason_column(self) -> bool {
        !matches!(self, Self::Manual)
    }

    /// reason-aware 的 `max_attempts` 上限（上游 `retryAttemptCeiling`）。
    ///
    /// 规则：**只放大、绝不复活** —— `max_attempts <= 1` 时原样返回（055：
    /// 「1 disables retry」，被停用的任务不许因为上限被抬高而复活）；
    /// 只有 `agent_error.provider_network` 有专用上限 3。
    #[must_use]
    pub const fn attempt_ceiling(self, max_attempts: u32) -> u32 {
        if max_attempts <= RETRY_DISABLED_MAX_ATTEMPTS {
            return max_attempts;
        }
        if matches!(self, Self::AgentProviderNetwork)
            && max_attempts < PROVIDER_NETWORK_MAX_ATTEMPTS
        {
            return PROVIDER_NETWORK_MAX_ATTEMPTS;
        }
        max_attempts
    }

    /// 第 `failed_attempt` 次失败之后，下一次尝试要等多少秒
    /// （上游 `retryDelayForAttempt`）。`0` = 立即（子任务直接落 `queued`）。
    #[must_use]
    pub const fn retry_delay_secs(self, failed_attempt: u32) -> u64 {
        if matches!(self, Self::RuntimeOffline) {
            return RUNTIME_OFFLINE_RETRY_DEFERRAL_SECS;
        }
        if matches!(self, Self::AgentProviderNetwork)
            && failed_attempt >= PROVIDER_NETWORK_MAX_ATTEMPTS - 1
        {
            return PROVIDER_NETWORK_FINAL_RETRY_WAIT_SECS;
        }
        0
    }
}

impl fmt::Display for FailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// serde 的线上形态**必须**是 `as_str`/`parse` 的那一对字符串：直接 `derive` 会
// 把 `AgentProviderNetwork` 序列化成 `agent_provider_network`，静默偏离上游
// `agent_error.provider_network` 契约（列值 / ws 事件 / 失败趋势面板三方都会对不上）。
impl TryFrom<String> for FailureReason {
    type Error = TaskError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<FailureReason> for String {
    fn from(value: FailureReason) -> Self {
        value.as_str().to_owned()
    }
}

/// `attempt` / `max_attempts` 的当前值（就是那两列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RetryBudget {
    /// `agent_task_queue.attempt`。
    pub attempt: u32,
    /// `agent_task_queue.max_attempts`。
    pub max_attempts: u32,
}

impl RetryBudget {
    /// 055 的默认值：`attempt=1`、`max_attempts=2`。
    pub const FIRST_RUN: Self = Self {
        attempt: DEFAULT_ATTEMPT,
        max_attempts: DEFAULT_MAX_ATTEMPTS,
    };

    /// 构造。
    #[must_use]
    pub const fn new(attempt: u32, max_attempts: u32) -> Self {
        Self {
            attempt,
            max_attempts,
        }
    }

    /// 自动重试是否被显式关闭（055：「1 disables retry」）。
    #[must_use]
    pub const fn is_retry_disabled(self) -> bool {
        self.max_attempts <= RETRY_DISABLED_MAX_ATTEMPTS
    }
}

impl Default for RetryBudget {
    fn default() -> Self {
        Self::FIRST_RUN
    }
}

/// 任务的运行种类（可 `Default` 为交互式）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    /// 普通交互式任务。
    Interactive,
    /// autopilot 跑（`autopilot_run_id IS NOT NULL`）—— 有自己的恢复通道，不自动重试。
    Autopilot,
    /// Triage 跑 —— 同理（重读入口会看到人工改过的内容，重放失败尝试永远不是想要的）。
    Triage,
}

impl RunKind {
    /// autopilot / triage 都被排除在自动重试之外（上游 `retryEligible`）。
    #[must_use]
    pub const fn allows_auto_retry(self) -> bool {
        matches!(self, Self::Interactive)
    }
}

/// 任务挂在哪条「可重跑」的链上。
///
/// 上游 `retryEligible` 要求 `issue_id` 或 `chat_session_id` 存在，并额外放行
/// source-context quick-create 任务（四个外键全 NULL 但 `context` 里带
/// `source_context_id`）；其余一律不重试，否则重试出来的子任务没有落点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLink {
    /// `issue_id IS NOT NULL`。
    Issue(mc_core::Id),
    /// `chat_session_id IS NOT NULL`。
    Chat(mc_core::Id),
    /// source-context quick-create（无 issue / chat / autopilot 但有 source context）。
    QuickCreate,
    /// 四键全空且不是 quick-create —— 没有重试落点。
    None,
}

impl TaskLink {
    /// 上游 `retryEligible` 的最后一项谓词。
    #[must_use]
    pub const fn is_runnable(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// 自动重试的闸门（上游 `retryEligible` 的入参 + 调用方的前置检查）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryGate {
    /// 运行种类。
    pub run_kind: RunKind,
    /// 可重跑链。
    pub link: TaskLink,
    /// 是否已经有一个未开工的后继占了该 (issue, agent) 的唯一槽位
    /// （上游 `hasRunnableSuccessor`，在两个自动重试入口创建子任务前的**建议性**预检）。
    pub pending_successor: bool,
}

impl RetryGate {
    /// 构造。
    #[must_use]
    pub const fn new(run_kind: RunKind, link: TaskLink, pending_successor: bool) -> Self {
        Self {
            run_kind,
            link,
            pending_successor,
        }
    }
}

impl Default for RetryGate {
    fn default() -> Self {
        Self {
            run_kind: RunKind::Interactive,
            link: TaskLink::None,
            pending_successor: false,
        }
    }
}

/// 不自动重试的原因。逐条对应上游 `retryEligible` 的一个合取项
/// （求值顺序 = 上游 `&&` 的短路顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrySkip {
    /// 原因不在 `retryableReasons` 里（含全部 `manual` 与大部分 `agent_error.*`）。
    ReasonNotRetryable,
    /// `attempt >= ceiling`（含 `max_attempts <= 1` 的「显式关闭」）。
    BudgetExhausted,
    /// autopilot 跑（有自己的恢复通道）。
    AutopilotRun,
    /// Triage 跑（同上）。
    TriageRun,
    /// 没有 issue / chat / quick-create 落点。
    NoRunnableLink,
    /// 唯一槽位已被另一个未开工的后继占住 —— 建子任务也会被
    /// `ON CONFLICT DO NOTHING` 吃掉，跳过可以省掉一次无用写入。
    PendingSuccessor,
}

impl RetrySkip {
    /// 该跳过是否**只**是建议性的（不写子任务也不会破坏正确性）。
    #[must_use]
    pub const fn is_advisory(self) -> bool {
        matches!(self, Self::PendingSuccessor)
    }
}

/// 要创建的自动重试子任务（`parent_task_id` 指回失败的那一行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RetryChild {
    /// 子任务的 `attempt` = 父的 `attempt + 1`。
    pub attempt: u32,
    /// 子任务的 `max_attempts` = reason-aware 上限。
    ///
    /// 必须持久化进子行，否则链上会出现自相矛盾的
    /// `attempt=3, max_attempts=2`（上游 MUL-4910 就是修这个）。
    pub max_attempts: u32,
    /// 下一次尝试的延迟秒数（写 `fire_at`）。
    pub delay_secs: u64,
    /// 子任务的初始状态：有延迟则 `deferred`（等调度器提升），否则 `queued`。
    pub status: TaskStatus,
    /// 重试是否必须换新 agent 会话（`force_fresh_session`）。
    pub force_fresh_session: bool,
}

/// 自动重试判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryDecision {
    /// 是否重试。
    pub retry: bool,
    /// 不重试时的原因（`retry == true` 时为 `None`）。
    pub skip: Option<RetrySkip>,
    /// 本次失败适用的 reason-aware 上限（无论重不重试都有效）。
    pub ceiling: u32,
    /// 重试是否必须换新会话（`FailureReason::is_resume_unsafe`）。
    pub resume_unsafe: bool,
    /// 要创建的子任务；`retry == false` 时为 `None`。
    pub child: Option<RetryChild>,
}

/// 自动重试判定（上游 `retryEligible` + `retryAttemptCeiling` + `retryDelayForAttempt` + `ResumeUnsafeFailure`）。
///
/// 判定顺序**有意**与上游 `retryEligible` 的 `&&` 短路顺序一致，这样
/// `skip` 报出的原因是上游会先撞上的那一个。
#[must_use]
pub fn decide_retry(reason: FailureReason, budget: RetryBudget, gate: RetryGate) -> RetryDecision {
    let ceiling = reason.attempt_ceiling(budget.max_attempts);
    let resume_unsafe = reason.is_resume_unsafe();
    let mut decision = RetryDecision {
        retry: false,
        skip: None,
        ceiling,
        resume_unsafe,
        child: None,
    };

    let skip = if !reason.is_retryable() {
        Some(RetrySkip::ReasonNotRetryable)
    } else if budget.attempt >= ceiling {
        Some(RetrySkip::BudgetExhausted)
    } else if matches!(gate.run_kind, RunKind::Autopilot) {
        Some(RetrySkip::AutopilotRun)
    } else if matches!(gate.run_kind, RunKind::Triage) {
        Some(RetrySkip::TriageRun)
    } else if !gate.link.is_runnable() {
        Some(RetrySkip::NoRunnableLink)
    } else if gate.pending_successor {
        Some(RetrySkip::PendingSuccessor)
    } else {
        None
    };

    if let Some(skip) = skip {
        decision.skip = Some(skip);
        return decision;
    }

    let delay_secs = reason.retry_delay_secs(budget.attempt);
    decision.retry = true;
    decision.child = Some(RetryChild {
        attempt: budget.attempt.saturating_add(1),
        max_attempts: ceiling,
        delay_secs,
        // 正值 = 停泊等调度器提升（runtime_offline 等健康门控；provider_network 末次冷却）。
        status: if delay_secs == 0 {
            TaskStatus::Queued
        } else {
            TaskStatus::Deferred
        },
        force_fresh_session: resume_unsafe,
    });
    decision
}

#[cfg(test)]
mod tests;
