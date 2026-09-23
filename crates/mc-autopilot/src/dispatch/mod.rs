//! 派发面：把 trigger / run 变成 `agent_task_queue` 行（以及「建 issue + 派任务」）。
//!
//! - **写者**：M5-4（`dispatch/**` 整组）。
//! - **上游**：`service/autopilot.go` 的 dispatch 段 ≈2,600 —— `DispatchAutopilot`35 /
//!   `DispatchAutopilotManual`12 / `DispatchAutopilotManualWithKey`20 / `dispatchAutopilot`52 /
//!   `dispatchAutopilotRun`57 / 三个执行分支 / `SyncRunFrom*`159 / `shouldSkipDispatch`98 /
//!   `recordSkippedRun`58 / `failRun`25 / `publishRunDone`13 / 分析 92 / 模板与工具 200。
//! - **三块必须分开测**（`docs/44` §4.2）：① `create_issue.rs` 建 issue + 派任务；
//!   ② `run_only.rs` 只派任务；③ `sync.rs` 把任务终态回写 run。
//! - **边界（R9）**：若发现 `agent_task_queue` 的 `queued` 行不会被执行（daemon 面未接线），
//!   登记为**跨波缺口**，**不要在本波实现 daemon**。
//! - **`autopilot_run.status` 必须显式写入**：列默认值还是 `'pending'`，而 `079` 之后 CHECK 只允许
//!   `{issue_created, running, completed, failed, skipped}` ⇒ 那个默认值已经**落在 CHECK 之外**，
//!   依赖默认值必然插入失败。
//!
//! 上游源：`internal/service/autopilot.go` 的 dispatch 段（`DispatchAutopilot`92 /
//! `DispatchAutopilotForPlan`437 / `dispatchAutopilot`556 / `dispatchCreateIssue`689 /
//! `dispatchRunOnly`981 / `SyncRunFrom*`1085 / `handleDispatchSkip`1263 / `recordSkippedRun`1495 /
//! `shouldSkipDispatch`1321 / `resolveAutopilotLeader`1461）。
//!
//! # 分层
//!
//! ```text
//! mc-autopilot::dispatch        ← 事务边界 + 顺序编排（本文件）
//!        ↓ 自由函数（收 &mut PgConnection / &PgPool）
//! mc-repos::autopilot::{run, run::issue_sql, run::lookup_sql, quota}
//! ```
//!
//! **本 crate 里 0 条 `sqlx::query`**：事务由这里 `PgPool::begin()` 打开，SQL 全部在 `mc-repos`
//! （`mc-autopilot` 没有 `mc-db` 依赖 ⇒ 拿不到 `Db`，一律走自由函数，同 M5-6 `wakeup` 的先例）。
//!
//! # 与上游的结构性差异（`docs/44` §8 已登记）
//!
//! 1. **没有 `concurrency_policy`**：迁移 `043` 已 DROP 该列，`shouldSkipDispatch` 里也**没有**
//!    并发策略分支。于是本次交付的三种结果是上游真正存在的那三种：**`skipped`（准入闸）/
//!    新建 run+task / `reused`（幂等命中，`reused = true`）**。
//! 2. **`AgentReadiness` + `autopilotAdmitInvoke` 不在本波**（runtime 就绪面与私有 squad 调用闸
//!    依赖 M6/M7，`docs/52` known_gap）⇒ 准入闸简化为「assignee 解析得出来 + agent 未归档 +
//!    squad 未归档」。
//! 3. **`create_issue` 的 task 入队在本切片内**：上游那条链是「建 issue → 发 issue 事件 →
//!    监听器 `EnqueueTaskForIssue` 入队」。本地没有 issue→task 监听链（daemon loop 属 M3-7），
//!    所以 dispatch 自己建 task 并回链 `run.task_id`；fence 拦下（`create_task` 返回 `None`）
//!    ⇒ 整事务回滚 + run 落 `failed`。
//! 4. **幂等键不在 `autopilot_run` 上**：该表**没有** `idempotency_key` 列（迁移 `042` 实测）。
//!    幂等由三处唯一索引承担：`uq_autopilot_run_trigger_planned`（计划线）、
//!    `uq_autopilot_run_webhook_delivery`（webhook 线）、`autopilot_quota_reservation.idempotency_key`
//!    （手动带键线，需装 entitlement 平面）。
//!
//! `SyncRunFrom*`（[`AutopilotDispatcher::sync_from_issue_status`] /
//! [`AutopilotDispatcher::sync_from_task`]）交付为**可调用的服务 API + 真库用例**；生产触发链
//! （daemon / task 终态回调）由 M5-7/M5-8 接（known_gap 已登记）。

pub mod admission;
pub mod analytics;
pub mod attribution;
pub mod create_issue;
pub mod run_only;
pub mod skip;
pub mod sync;
pub mod template;

use chrono::{DateTime, SecondsFormat, Utc};
use mc_core::autopilot::RunSource;
use mc_core::Id;
use mc_realtime::{EventEnvelope, RealtimeHandle};
use mc_repos::autopilot::quota as quota_repo;
use mc_repos::autopilot::run::{self as run_sql, AutopilotRunRow, NewAutopilotRun};
use mc_repos::autopilot::AutopilotRow;
use mc_repos::RepoError;
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::quota as quota_policy;

/// `autopilot_recent_duplicate_window`（`autopilot.go:51`）：60s 内同 key 视为重复 issue。
pub(crate) const RECENT_DUPLICATE_WINDOW_SECONDS: i64 = 60;

/// `triggerSummaryMaxLen`（`task.go:126`）：task 行上的标题快照截断长度。
pub(crate) const TRIGGER_SUMMARY_MAX_LEN: usize = 200;

/// 终态 `completed`。
pub(crate) const RUN_STATUS_COMPLETED: &str = "completed";
/// 终态 `failed`。
pub(crate) const RUN_STATUS_FAILED: &str = "failed";
/// 终态 `skipped`。
pub(crate) const RUN_STATUS_SKIPPED: &str = "skipped";

/// 上游 `EventAutopilotRunStart`：run 开始跑（run 已带 issue/task 引用）。
pub const EVENT_AUTOPILOT_RUN_START: &str = "autopilot:run_start";
/// 上游 `EventAutopilotRunDone`：run 进终态（`completed` / `failed` / `skipped` 都发）。
pub const EVENT_AUTOPILOT_RUN_DONE: &str = "autopilot:run_done";

/// realtime 信封的 resource 名（订阅单位是 workspace；`autopilot_id` 在载荷里）。
const EVENT_RESOURCE: &str = "workspace";

/// 派发跳过 / 失败的原因码 —— 上游 `internal/dispatch/reason.go` 的 **17** 个取值。
///
/// 放在派发层而不是 `mc-core`：`autopilot_run.reason_code` 是自由 `TEXT`（没有 CHECK），
/// 这套词表在上游也单独成包（`internal/dispatch`），不属于领域枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasonCode {
    /// `queued`。
    Queued,
    /// `coalesced`。
    Coalesced,
    /// `deferred`。
    Deferred,
    /// `invocation_not_allowed`。
    InvocationNotAllowed,
    /// `target_unavailable`。
    TargetUnavailable,
    /// `runtime_offline`。
    RuntimeOffline,
    /// `runtime_unusable`。
    RuntimeUnusable,
    /// `runtime_access_denied`。
    RuntimeAccessDenied,
    /// `runtime_profile_missing`。
    RuntimeProfileMissing,
    /// `agent_runtime_required`。
    AgentRuntimeRequired,
    /// `attribution_blocked`。
    AttributionBlocked,
    /// `already_active`。
    AlreadyActive,
    /// `self_trigger_suppressed`。
    SelfTriggerSuppressed,
    /// `issue_in_triage`。
    IssueInTriage,
    /// `quota_exceeded`。
    QuotaExceeded,
    /// `issue_limit_reached`。
    IssueLimitReached,
    /// `internal_error`。
    InternalError,
}

impl ReasonCode {
    /// 全部 17 个取值（顺序同上游 `reason.go`）。
    pub const ALL: [Self; 17] = [
        Self::Queued,
        Self::Coalesced,
        Self::Deferred,
        Self::InvocationNotAllowed,
        Self::TargetUnavailable,
        Self::RuntimeOffline,
        Self::RuntimeUnusable,
        Self::RuntimeAccessDenied,
        Self::RuntimeProfileMissing,
        Self::AgentRuntimeRequired,
        Self::AttributionBlocked,
        Self::AlreadyActive,
        Self::SelfTriggerSuppressed,
        Self::IssueInTriage,
        Self::QuotaExceeded,
        Self::IssueLimitReached,
        Self::InternalError,
    ];

    /// 线上字面量（写进 `reason_code` 列的那个）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Coalesced => "coalesced",
            Self::Deferred => "deferred",
            Self::InvocationNotAllowed => "invocation_not_allowed",
            Self::TargetUnavailable => "target_unavailable",
            Self::RuntimeOffline => "runtime_offline",
            Self::RuntimeUnusable => "runtime_unusable",
            Self::RuntimeAccessDenied => "runtime_access_denied",
            Self::RuntimeProfileMissing => "runtime_profile_missing",
            Self::AgentRuntimeRequired => "agent_runtime_required",
            Self::AttributionBlocked => "attribution_blocked",
            Self::AlreadyActive => "already_active",
            Self::SelfTriggerSuppressed => "self_trigger_suppressed",
            Self::IssueInTriage => "issue_in_triage",
            Self::QuotaExceeded => "quota_exceeded",
            Self::IssueLimitReached => "issue_limit_reached",
            Self::InternalError => "internal_error",
        }
    }

    /// 反解（幂等命中时把库里存的 `reason_code` 还原成枚举）。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|code| code.as_str() == raw)
    }
}

impl std::fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一次派发的输入。
///
/// `idempotency_key` 是配额预留表的幂等主键：上游三种形态 —— 手动 `manual:{autopilot}:{key}`、
/// 非手动 `{source}:{uuid}`、计划 `schedule:{trigger}:{plannedAt}`。
#[derive(Debug)]
pub struct DispatchRequest<'a> {
    /// 被派发的 autopilot 行。
    pub autopilot: &'a AutopilotRow,
    /// 触发这条 run 的 trigger（`run_only` 的 `trigger_owner` 归属靠它）。
    pub trigger_id: Option<Uuid>,
    /// run 来源。
    pub source: RunSource,
    /// webhook / API 载荷（`create_issue` 会贴进 issue 描述）。
    pub payload: Option<Value>,
    /// 计划触发时刻（仅 `schedule` 用；写进 `planned_at`）。
    pub planned_at: Option<DateTime<Utc>>,
    /// webhook 投递 id（`run.webhook_delivery_id`）。
    pub webhook_delivery_id: Option<Uuid>,
    /// 手动点击的人（`direct_human` 归属）。
    pub actor_user_id: Option<Uuid>,
    /// 幂等键。
    pub idempotency_key: String,
}

impl<'a> DispatchRequest<'a> {
    /// 手动「立即运行」（不带幂等键）：`manual:{autopilot}:{uuid}`（`DispatchAutopilotManual`150）。
    #[must_use]
    pub fn manual(
        autopilot: &'a AutopilotRow,
        trigger_id: Option<Uuid>,
        payload: Option<Value>,
        actor_user_id: Option<Uuid>,
    ) -> Self {
        Self {
            autopilot,
            trigger_id,
            source: RunSource::Manual,
            payload,
            planned_at: None,
            webhook_delivery_id: None,
            actor_user_id,
            idempotency_key: format!("manual:{}:{}", autopilot.id, Uuid::new_v4()),
        }
    }

    /// 带幂等键的手动运行（`DispatchAutopilotManualWithKey`165）：同键重放 ⇒ `reused`。
    #[must_use]
    pub fn manual_with_key(
        autopilot: &'a AutopilotRow,
        trigger_id: Option<Uuid>,
        payload: Option<Value>,
        actor_user_id: Option<Uuid>,
        idempotency_key: &str,
    ) -> Self {
        let mut req = Self::manual(autopilot, trigger_id, payload, actor_user_id);
        req.idempotency_key = format!("manual:{}:{idempotency_key}", autopilot.id);
        req
    }

    /// 计划 / API 触发的通用形态：`{source}:{uuid}`（`DispatchAutopilot`92）。
    #[must_use]
    pub fn for_source(
        autopilot: &'a AutopilotRow,
        source: RunSource,
        trigger_id: Option<Uuid>,
        payload: Option<Value>,
    ) -> Self {
        Self {
            autopilot,
            trigger_id,
            source,
            payload,
            planned_at: None,
            webhook_delivery_id: None,
            actor_user_id: None,
            idempotency_key: format!("{}:{}", source.as_str(), Uuid::new_v4()),
        }
    }

    /// webhook 投递（M5-5 的 ingress 用）：`webhook:{deliveryID}`，投递 id 就是幂等键。
    #[must_use]
    pub fn for_webhook(
        autopilot: &'a AutopilotRow,
        trigger_id: Option<Uuid>,
        payload: Option<Value>,
        delivery_id: Uuid,
    ) -> Self {
        Self {
            autopilot,
            trigger_id,
            source: RunSource::Webhook,
            payload,
            planned_at: None,
            webhook_delivery_id: Some(delivery_id),
            actor_user_id: None,
            idempotency_key: format!("webhook:{delivery_id}"),
        }
    }
}

/// 一次派发的结果。
#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    /// 终态（或刚建出）的 run 行。
    pub run: AutopilotRunRow,
    /// 跳过 / 失败时带上原因码；正常跑起来是 `None`。
    pub reason_code: Option<ReasonCode>,
    /// 幂等命中（同一个幂等键已有 run，本次**没有**产生副作用）。
    pub reused: bool,
}

impl DispatchOutcome {
    /// run 是否落 `skipped`。
    #[must_use]
    pub fn is_skipped(&self) -> bool {
        self.run.status == RUN_STATUS_SKIPPED
    }
}

/// 「跳过」—— 上游 `errDispatchSkipped`：**不是失败**，run 落 `skipped` 而不是 `failed`。
#[derive(Debug, Clone)]
pub struct DispatchSkipped {
    /// 人读原因（写 `autopilot_run.failure_reason`）。
    pub reason: String,
    /// 原因码。
    pub code: ReasonCode,
}

impl DispatchSkipped {
    /// 构造。
    #[must_use]
    pub fn new(reason: impl Into<String>, code: ReasonCode) -> Self {
        Self {
            reason: reason.into(),
            code,
        }
    }
}

/// 派发子过程的出口：要么跳过，要么失败（成功走 `Ok`）。
#[derive(Debug)]
pub(crate) enum SideEffectError {
    /// 跳过（run 落 `skipped`）。
    Skipped(DispatchSkipped),
    /// 失败（run 落 `failed`）。
    Failed {
        /// 人读信息。
        message: String,
        /// 返回给调用方的原因码（DB 里一律写 `internal_error`，见 `fail_run`）。
        code: ReasonCode,
    },
}

impl SideEffectError {
    /// 跳过。
    pub(crate) fn skipped(reason: impl Into<String>, code: ReasonCode) -> Self {
        Self::Skipped(DispatchSkipped::new(reason, code))
    }

    /// 失败（带原因码；默认 `internal_error`）。
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
            code: ReasonCode::InternalError,
        }
    }
}

/// 派发失败。
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    /// 配额拦下（非 `schedule` 来源直接回给 HTTP 层；`schedule` 来源落 `skipped` run）。
    #[error("autopilot quota exceeded: used {used}, reserved {reserved}, limit {limit}, reset at {reset_at}")]
    QuotaExceeded {
        /// 已用。
        used: i64,
        /// 已占位。
        reserved: i64,
        /// 上限。
        limit: i64,
        /// 额度重置时刻。
        reset_at: DateTime<Utc>,
    },
    /// run 已落 `failed`，把原因码带回调用方。
    #[error("autopilot dispatch failed: {message}")]
    Failed {
        /// 失败的 run。
        run_id: Uuid,
        /// 原因码。
        reason_code: ReasonCode,
        /// 人读信息。
        message: String,
    },
    /// 入参不合法（`dispatch_for_plan` 缺 trigger / planned_at）。
    #[error("invalid dispatch request: {0}")]
    Invalid(String),
    /// 库错。
    #[error(transparent)]
    Repo(#[from] RepoError),
}

impl DispatchError {
    /// 这次失败对应哪个原因码。
    #[must_use]
    pub fn reason_code(&self) -> Option<ReasonCode> {
        match self {
            Self::QuotaExceeded { .. } => Some(ReasonCode::QuotaExceeded),
            Self::Failed { reason_code, .. } => Some(*reason_code),
            Self::Invalid(_) | Self::Repo(_) => None,
        }
    }
}

/// autopilot 派发器：持 `PgPool` + 可选的 realtime 出口。
///
/// 为什么不持 `Db`：`mc-autopilot` 没有 `mc-db` 依赖（本波不得新增依赖），所以派发链一律走
/// `mc-repos` 的自由函数。
pub struct AutopilotDispatcher {
    pool: PgPool,
    events: Option<RealtimeHandle>,
}

impl AutopilotDispatcher {
    /// 不带事件出口。
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool, events: None }
    }

    /// 带 realtime 出口（HTTP 层传 `AppState.realtime`）。
    #[must_use]
    pub fn with_events(mut self, events: RealtimeHandle) -> Self {
        self.events = Some(events);
        self
    }

    /// 库池（`SyncRunFrom*` 等外部集成点要用）。
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 派发一次 run（`DispatchAutopilot`92 / `DispatchAutopilotManual`150）。
    ///
    /// # Errors
    ///
    /// [`DispatchError`]：配额拦下 / 入参不合法 / 库错。**跳过不算错误** —— 用
    /// [`DispatchOutcome::is_skipped`] 判。
    pub async fn dispatch(
        &self,
        req: DispatchRequest<'_>,
    ) -> Result<DispatchOutcome, DispatchError> {
        // ① 准入闸（`shouldSkipDispatch`1321）。
        if let Some(skipped) = admission::should_skip_dispatch(&self.pool, req.autopilot).await? {
            let run = self.record_skipped(&req, &skipped).await?;
            return Ok(DispatchOutcome {
                run,
                reason_code: Some(skipped.code),
                reused: false,
            });
        }
        // ② 建 run（配额准入与 insert 同事务）。
        let initial = initial_status(req.autopilot.execution_mode.as_str());
        let (run, reused) = match self.create_run_with_quota(&req, initial).await {
            Ok(pair) => pair,
            Err(CreateRunError::QuotaExceeded {
                used,
                reserved,
                limit,
                reset_at,
            }) => {
                // 计划触发无人值守：落一条 skipped run 留证，而不是把额度错抛给调度器。
                if req.source == RunSource::Schedule {
                    let skipped = DispatchSkipped::new(
                        format!(
                            "autopilot quota exceeded: used {used}, reserved {reserved}, limit {limit}"
                        ),
                        ReasonCode::QuotaExceeded,
                    );
                    let run = self.record_skipped(&req, &skipped).await?;
                    return Ok(DispatchOutcome {
                        run,
                        reason_code: Some(ReasonCode::QuotaExceeded),
                        reused: false,
                    });
                }
                return Err(DispatchError::QuotaExceeded {
                    used,
                    reserved,
                    limit,
                    reset_at,
                });
            }
            Err(CreateRunError::Repo(err)) => return Err(DispatchError::Repo(err)),
        };
        if reused {
            let reason_code = run.reason_code.as_deref().and_then(ReasonCode::parse);
            return Ok(DispatchOutcome {
                run,
                reason_code,
                reused: true,
            });
        }
        // ③ 执行（`dispatchAutopilot`556）。
        self.dispatch_run(
            req.autopilot,
            req.trigger_id,
            req.source,
            &run,
            req.actor_user_id,
        )
        .await
    }

    /// 计划触发（`DispatchAutopilotForPlan`437）：`trigger_id` 必给，幂等键 =
    /// `schedule:{trigger}:{plannedAt}`。
    ///
    /// # Errors
    ///
    /// 同 [`Self::dispatch`]；缺 `trigger_id` 返回 [`DispatchError::Invalid`]。
    pub async fn dispatch_for_plan(
        &self,
        req: DispatchRequest<'_>,
        planned_at: DateTime<Utc>,
    ) -> Result<DispatchOutcome, DispatchError> {
        let Some(trigger_id) = req.trigger_id else {
            return Err(DispatchError::Invalid("trigger_id is required".to_string()));
        };
        let mut req = req;
        req.source = RunSource::Schedule;
        req.planned_at = Some(planned_at);
        req.idempotency_key = format!(
            "schedule:{trigger_id}:{}",
            planned_at.to_rfc3339_opts(SecondsFormat::Nanos, true)
        );
        self.dispatch(req).await
    }

    /// 执行一条**已经存在**的 run（`dispatchAutopilot`556）。
    ///
    /// 公开给 M5-5：webhook ingress 的准入阶段就建好了 run（副作用延后到 worker），worker 拿这条
    /// API 补跑副作用。
    ///
    /// # Errors
    ///
    /// [`DispatchError::Failed`]：副作用失败（run 已落 `failed`）。
    pub async fn dispatch_run(
        &self,
        autopilot: &AutopilotRow,
        trigger_id: Option<Uuid>,
        source: RunSource,
        run: &AutopilotRunRow,
        actor_user_id: Option<Uuid>,
    ) -> Result<DispatchOutcome, DispatchError> {
        let side_effect = match autopilot.execution_mode.as_str() {
            "create_issue" => {
                let timezone = template::resolve_trigger_timezone(&self.pool, trigger_id).await;
                create_issue::dispatch_create_issue(
                    &self.pool,
                    autopilot,
                    run,
                    &timezone,
                    actor_user_id,
                )
                .await
            }
            "run_only" => {
                run_only::dispatch_run_only(&self.pool, autopilot, run, actor_user_id).await
            }
            other => Err(SideEffectError::failed(format!(
                "unknown execution_mode: {other}"
            ))),
        };
        match side_effect {
            Ok(updated) => {
                analytics::run_started(autopilot, &updated, source);
                self.publish_run_start(autopilot, &updated, source);
                self.touch_last_run_at(autopilot).await;
                Ok(DispatchOutcome {
                    run: updated,
                    reason_code: None,
                    reused: false,
                })
            }
            Err(SideEffectError::Skipped(skip)) => {
                // `handleDispatchSkip`1263：准入之后才发现的不可用（squad 刚归档 / 重复 issue）
                // 走 skipped，**不是** failed。
                let updated = self
                    .settle_with_quota(
                        run.id,
                        RUN_STATUS_SKIPPED,
                        None,
                        Some(&skip.reason),
                        Some(skip.code.as_str()),
                        false,
                    )
                    .await?;
                analytics::run_skipped(autopilot, &updated, source, &skip.reason);
                self.publish_run_done(autopilot, &updated);
                self.touch_last_run_at(autopilot).await;
                Ok(DispatchOutcome {
                    run: updated,
                    reason_code: Some(skip.code),
                    reused: false,
                })
            }
            Err(SideEffectError::Failed { message, code }) => {
                // DB 里一律写 `internal_error`（上游 `failRun`1296 就是这么钉的），带回调用方的
                // 原因码由 `DispatchError` 承担。
                self.fail_run(run.id, &message).await;
                analytics::run_failed(autopilot, run, source, &message);
                Err(DispatchError::Failed {
                    run_id: run.id,
                    reason_code: code,
                    message,
                })
            }
        }
    }

    /// `recordSkippedRun`1495：为「还没建 run 就被跳过」的场景补一条 `skipped` run
    /// （准入闸拦下 / 计划触发被配额拦下）。
    ///
    /// # Errors
    ///
    /// 库错。
    pub async fn record_skipped(
        &self,
        req: &DispatchRequest<'_>,
        skipped: &DispatchSkipped,
    ) -> Result<AutopilotRunRow, DispatchError> {
        skip::record_skipped_run(
            &self.pool,
            req,
            &skipped.reason,
            skipped.code,
            self.events.as_ref(),
        )
        .await
    }

    /// `UpdateAutopilotRunTerminalWithQuota` 的单条封装：终态 + 预留结算同一事务。
    async fn settle_with_quota(
        &self,
        run_id: Uuid,
        status: &str,
        result: Option<Value>,
        failure_reason: Option<&str>,
        reason_code: Option<&str>,
        consume: bool,
    ) -> Result<AutopilotRunRow, DispatchError> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let row = run_sql::update_terminal_with_quota(
            &mut conn,
            run_id,
            status,
            result,
            failure_reason,
            reason_code,
            consume,
        )
        .await?;
        Ok(row)
    }

    /// `failRun`1296：把 run 落 `failed`（**尽力而为** —— 失败路径上的失败只记日志）。
    async fn fail_run(&self, run_id: Uuid, message: &str) {
        let reason = truncate(message, 2000);
        match self
            .settle_with_quota(
                run_id,
                RUN_STATUS_FAILED,
                None,
                Some(&reason),
                Some(ReasonCode::InternalError.as_str()),
                false,
            )
            .await
        {
            Ok(_) => {}
            Err(err) => tracing::warn!(
                run_id = %run_id,
                error = %err,
                "failed to mark autopilot run as failed"
            ),
        }
    }

    /// `UpdateAutopilotLastRunAt`：尽力而为（上游同样只记日志）。
    async fn touch_last_run_at(&self, autopilot: &AutopilotRow) {
        if let Err(err) = run_sql::update_autopilot_last_run_at(&self.pool, autopilot.id).await {
            tracing::warn!(
                autopilot_id = %autopilot.id,
                error = %err,
                "failed to update autopilot last_run_at"
            );
        }
    }

    /// `EventAutopilotRunStart`（尽力而为：没有 realtime 出口就静默）。
    fn publish_run_start(&self, autopilot: &AutopilotRow, run: &AutopilotRunRow, source: RunSource) {
        let payload = serde_json::json!({
            "run_id": run.id,
            "autopilot_id": autopilot.id,
            "source": source.as_str(),
            "status": run.status,
        });
        self.publish(EVENT_AUTOPILOT_RUN_START, autopilot, payload);
    }

    /// `EventAutopilotRunDone`（终态）。
    pub(crate) fn publish_run_done(&self, autopilot: &AutopilotRow, run: &AutopilotRunRow) {
        let payload = serde_json::json!({
            "run_id": run.id,
            "autopilot_id": autopilot.id,
            "status": run.status,
        });
        self.publish(EVENT_AUTOPILOT_RUN_DONE, autopilot, payload);
    }

    /// 信封构造：`resource = "workspace"`，`resource_id = workspace_id`
    /// （本地 `/live-events` 的订阅单位是 workspace；`autopilot_id` 在载荷里）。
    fn publish(&self, event_type: &str, autopilot: &AutopilotRow, payload: Value) {
        let Some(handle) = &self.events else {
            return;
        };
        let envelope = EventEnvelope::new(
            EVENT_RESOURCE,
            autopilot.workspace_id.to_string(),
            None,
            payload,
        )
        .with_type(event_type);
        handle.publish(envelope);
    }

    /// `createAutopilotRunWithQuota`（`autopilot_quota.go`）的本地形态：幂等快路径 → `admit`
    /// →（预留成功时）insert run，**同一事务**。
    async fn create_run_with_quota(
        &self,
        req: &DispatchRequest<'_>,
        initial_status: &str,
    ) -> Result<(AutopilotRunRow, bool), CreateRunError> {
        let autopilot = req.autopilot;
        let mut new = NewAutopilotRun {
            id: Uuid::new_v4(),
            autopilot_id: autopilot.id,
            trigger_id: req.trigger_id,
            source: req.source.as_str().to_string(),
            status: initial_status.to_string(),
            trigger_payload: req.payload.clone(),
            squad_id: squad_attribution(autopilot),
            planned_at: req.planned_at,
            webhook_delivery_id: req.webhook_delivery_id,
            quota_reservation_id: None,
            reason_code: None,
        };
        let Some(policy) = quota_policy::policy_for(Id(autopilot.workspace_id)) else {
            // 配额没装（默认 `NoEntitlementPlane`）⇒ 不经预留表，唯一索引承担幂等。
            let mut conn = self.pool.acquire().await.map_err(pool_err)?;
            if let Some(existing) = find_existing_run(&mut conn, req).await? {
                return Ok((existing, true));
            }
            match run_sql::create_run(&mut conn, &new).await {
                Ok(row) => Ok((row, false)),
                Err(RepoError::Conflict(_)) => match find_existing_run(&mut conn, req).await? {
                    Some(existing) => Ok((existing, true)),
                    None => Err(CreateRunError::Repo(RepoError::Db(
                        "autopilot run insert conflicted but no existing run found".to_string(),
                    ))),
                },
                Err(err) => Err(CreateRunError::Repo(err)),
            }
        } else {
            let mut tx = self.pool.begin().await.map_err(pool_err)?;
            let existing = quota_repo::get_reservation_by_key(
                &mut *tx,
                autopilot.workspace_id,
                policy.period_start.as_datetime(),
                policy.period_end.as_datetime(),
                &req.idempotency_key,
            )
            .await?;
            let existing_has_run = match &existing {
                Some(reservation) => run_sql::find_by_quota_reservation(&mut *tx, reservation.id)
                    .await?
                    .is_some(),
                None => false,
            };
            let outcome = quota_repo::admit(
                &mut tx,
                &quota_repo::AdmitInput {
                    workspace_id: autopilot.workspace_id,
                    period_start: policy.period_start.as_datetime(),
                    period_end: policy.period_end.as_datetime(),
                    source: req.source.as_str().to_string(),
                    idempotency_key: req.idempotency_key.clone(),
                    policy_revision: policy.policy_revision,
                    subscription_version: policy.subscription_version,
                    limit: Some(policy.limit),
                    enforce: policy.action == quota_policy::QuotaAction::Enforce,
                    reason_code: ReasonCode::QuotaExceeded.as_str().to_string(),
                },
                existing_has_run,
            )
            .await?;
            match outcome {
                quota_repo::AdmitOutcome::Replayed { reservation_id } => {
                    let run = run_sql::find_by_quota_reservation(&mut *tx, reservation_id)
                        .await?
                        .ok_or_else(|| {
                            CreateRunError::Repo(RepoError::Db(
                                "quota reservation replayed without an autopilot run".to_string(),
                            ))
                        })?;
                    tx.commit().await.map_err(pool_err)?;
                    Ok((run, true))
                }
                quota_repo::AdmitOutcome::Denied {
                    used,
                    reserved,
                    limit,
                } => {
                    // 被拒也要提交：这次尝试已经记进额度账（`blocked_counts`）。
                    tx.commit().await.map_err(pool_err)?;
                    Err(CreateRunError::QuotaExceeded {
                        used,
                        reserved,
                        limit,
                        reset_at: policy.reset_at.as_datetime(),
                    })
                }
                quota_repo::AdmitOutcome::Reserved {
                    reservation_id,
                    would_block,
                } => {
                    if would_block {
                        tracing::warn!(
                            autopilot_id = %autopilot.id,
                            idempotency_key = %req.idempotency_key,
                            "autopilot quota reservation would block (observe mode)"
                        );
                    }
                    new.quota_reservation_id = Some(reservation_id);
                    let run = run_sql::create_run(&mut *tx, &new).await?;
                    tx.commit().await.map_err(pool_err)?;
                    Ok((run, false))
                }
            }
        }
    }
}

/// 建 run 的两类非成功出口。
enum CreateRunError {
    /// 配额拒绝。
    QuotaExceeded {
        /// 已用。
        used: i64,
        /// 已占位。
        reserved: i64,
        /// 上限。
        limit: i64,
        /// 重置时刻。
        reset_at: DateTime<Utc>,
    },
    /// 库错。
    Repo(RepoError),
}

impl From<RepoError> for CreateRunError {
    fn from(err: RepoError) -> Self {
        Self::Repo(err)
    }
}

/// 幂等快路径：计划线看 `(trigger_id, planned_at)`，webhook 线看投递 id
/// （本地 `autopilot_run` 没有 `idempotency_key` 列，这两处唯一索引就是幂等主键）。
async fn find_existing_run(
    conn: &mut PgConnection,
    req: &DispatchRequest<'_>,
) -> Result<Option<AutopilotRunRow>, RepoError> {
    if let (Some(trigger_id), Some(planned_at)) = (req.trigger_id, req.planned_at) {
        if let Some(run) = run_sql::find_by_trigger_and_planned(conn, trigger_id, planned_at).await?
        {
            return Ok(Some(run));
        }
    }
    if let Some(delivery_id) = req.webhook_delivery_id {
        if let Some(run) = run_sql::find_by_webhook_delivery(conn, delivery_id).await? {
            return Ok(Some(run));
        }
    }
    Ok(None)
}

/// 新 run 的初始状态：`run_only` 直接开跑，`create_issue` 等 issue 建出来才算「已建」。
fn initial_status(execution_mode: &str) -> &'static str {
    match execution_mode {
        "run_only" => "running",
        _ => "issue_created",
    }
}

/// `squad_id` 归属：只有 `assignee_type = 'squad'` 才带上（`autopilotSquadAttribution`1488）。
fn squad_attribution(autopilot: &AutopilotRow) -> Option<Uuid> {
    (autopilot.assignee_type == "squad").then_some(autopilot.assignee_id)
}

/// `isAutopilotRunComplete`：终态判定（幂等快路径用）。
#[must_use]
pub fn is_run_complete(run: &AutopilotRunRow) -> bool {
    matches!(
        run.status.as_str(),
        RUN_STATUS_COMPLETED | RUN_STATUS_FAILED | RUN_STATUS_SKIPPED
    )
}

/// `sqlx::Error` → [`DispatchError`]（`mc-repos` 的 `map_sqlx_err` 是 `pub(crate)`，拿不到）。
pub(crate) fn db_err(err: sqlx::Error) -> DispatchError {
    DispatchError::Repo(RepoError::Db(err.to_string()))
}

/// `sqlx::Error` → [`CreateRunError`] 的池错分支。
fn pool_err(err: sqlx::Error) -> CreateRunError {
    CreateRunError::Repo(RepoError::Db(err.to_string()))
}

/// `RepoError` 直通（子模块里 `?` 用）。
pub(crate) fn repo_err(err: RepoError) -> DispatchError {
    DispatchError::Repo(err)
}

/// 按字符数截断（`truncateForSummary` 的极简版）。
pub(crate) fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}
