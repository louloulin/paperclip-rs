//! worker 面：认领 → 限流 → 装载 → 交叉校验 → 归一化 → 派发 → 收口。
//!
//! - **写者**：M5-5（与 `admission.rs` 同一个 `impl WebhookIngress`，拆文件只为 R7 的 800 行硬上限）。
//! - **上游**：`handler/webhook_delivery_worker.go` 的 `ProcessNext` / `complete` /
//!   `retryOrFail` / `handleWebhookLeaseMutation`，以及 `service/autopilot.go` 的
//!   `DispatchAutopilotForWebhookDelivery`（含 `repairAutopilotRunTaskLink` 的**只补链接**子集）。
//!
//! 入站那 12 步在 [`super::admission`]；两段共用
//! [`WebhookIngress::admit_webhook_delivery`]（上游也是同一个 `AdmitAutopilotWebhookDelivery`
//! 既被入站调、又被 worker 调），所以「入站已建 run」与「worker 复用同一个 run」不会出现两份
//! 准入逻辑漂移。
//!
//! # `ProcessNext` 里最容易看漏的一段
//!
//! 只有在**查不到该投递的 run** 时才重查 trigger/autopilot 的可变状态（disabled / archived /
//! paused / `event_filtered`）。入站已经同步准入过，那次决定是**持久的**；若响应发出后运维立刻
//! 暂停 autopilot，重查会让一个已经答应了 provider 的 run 永远停在 `issue_created`。
//!
//! # 租约语义（`handleWebhookLeaseMutation` 的本地形态）
//!
//! `complete_claimed` / `retry_claimed` / `defer_claimed` 三条 SQL 都带
//! `lease_token = $2 AND status = 'queued'` 双条件 ⇒ 租约被抢走时返回 `None`。
//! 那是**正常竞态**（慢 worker 活过了租约，新主人负责收口），既不该报错也不该记指标 ——
//! 本地统一把 `None` 折成 `Ok(())` + debug 日志。

use chrono::{Duration, SecondsFormat, Utc};
use mc_core::autopilot::RunSource;
use mc_repos::autopilot::delivery::WebhookDeliveryRow;
use mc_repos::autopilot::ingress as sql;
use mc_repos::autopilot::run::{self as run_sql, AutopilotRunRow};
use mc_repos::autopilot::AutopilotRow;
use mc_repos::RepoError;
use serde_json::Value;
use uuid::Uuid;

use crate::dispatch::{is_run_complete, DispatchError};

use super::admission::AdmitRefusal;

use super::provider::{self, WebhookHeaders};
use super::ratelimit;
use super::{
    IgnoredReason, WebhookError, WebhookIngress, DELIVERY_STATUS_DISPATCHED,
    DELIVERY_STATUS_FAILED, DELIVERY_STATUS_IGNORED, QUOTA_EXCEEDED_MESSAGE,
    REASON_CODE_QUOTA_EXCEEDED,
};

/// worker 的派发尝试上限（上游 `webhookWorkerMaxAttempts`）。
///
/// 语义与上游一致：`dispatch_attempts + 1 >= 5` 时**直接判失败**，否则退避重试 ⇒ 最多 5 次派发、
/// 4 次退避。
pub const WEBHOOK_WORKER_MAX_ATTEMPTS: i32 = 5;

/// 退避位移上限（上游 `min(delivery.DispatchAttempts, 6)`）：1s 起步、指数增长、封顶 64s。
const MAX_BACKOFF_SHIFT: i32 = 6;

/// 归属交叉校验失败时的收口文案（上游逐字）。
const OWNERSHIP_MISMATCH: &str = "delivery ownership mismatch";

/// worker 派发的结果（上游 `DispatchAutopilotForWebhookDelivery` 的 `(*run, error)` 两元组）。
///
/// 用四态把「有 run 的错误」与「没 run 的错误」分开 —— 上游靠 `run != nil` 区分，两者的收口
/// **不同**：有 run ⇒ 投递落 `failed`（下游已经动过了，不再重试）；没 run ⇒ 重试/耗尽后失败。
pub enum WebhookDispatch {
    /// 正常拿到 run（含 `skipped` 的跳过态、以及崩溃窗口里已被别人跑完的 run）。
    Run(Box<AutopilotRunRow>),
    /// 配额拦下 ⇒ 投递落 `ignored` + `reason_code = quota_exceeded`（上游同款）。
    QuotaExceeded,
    /// 出错了，但**已经有一枚 run**（`repairAutopilotRunTaskLink` 失败面）⇒ 投递落 `failed`。
    FailedWithRun {
        /// 出错的 run。
        run: Box<AutopilotRunRow>,
        /// 人读原因（写 `webhook_delivery.error`）。
        message: String,
    },
    /// 纯错误（还没有 run）⇒ 交给重试/耗尽收口。
    Failed {
        /// 人读原因。
        message: String,
    },
}

impl WebhookIngress {
    // ── B 段：worker ────────────────────────────────────────────────────────

    /// `DispatchAutopilotForWebhookDelivery`：准入 → 崩溃窗口修补 → `dispatch_run`。
    ///
    /// 本地把上游的 `(*run, error)` 两元组折成 [`WebhookDispatch`] 四态（见其文档）。
    pub async fn dispatch_for_webhook_delivery(
        &self,
        autopilot: &AutopilotRow,
        trigger_id: Uuid,
        payload: Value,
        delivery_id: Uuid,
    ) -> WebhookDispatch {
        let run = match self
            .admit_webhook_delivery(autopilot, trigger_id, payload, delivery_id)
            .await
        {
            Ok(run) => run,
            Err(AdmitRefusal::QuotaExceeded) => return WebhookDispatch::QuotaExceeded,
            Err(AdmitRefusal::Repo(message)) => return WebhookDispatch::Failed { message },
        };

        // 已终态的 run（含准入跳过的 `skipped`，也包括崩溃窗口里已被跑完的 run）：上游在
        // `create_issue` + 有 issue 时补建任务（`ensureWebhookCreateIssueTask`）；本地建
        // issue / 挂 run / 建 task 是**同一个事务** ⇒ 那道窗口不存在，这里只记 debug 并复用
        // （偏差 D16，见 `docs/54`）。
        if is_run_complete(&run) {
            tracing::debug!(
                delivery_id = %delivery_id,
                run_id = %run.id,
                status = %run.status,
                "webhook worker: run already complete, reusing"
            );
            return WebhookDispatch::Run(Box::new(run));
        }

        // `run_only` 的任务可能在上次进程死前提交、而 `task_id` 没回填：只补链接。
        if autopilot.execution_mode != "create_issue" && run.task_id.is_none() {
            match self.repair_run_task_link(&run).await {
                Ok(Some(repaired)) => return WebhookDispatch::Run(Box::new(repaired)),
                Ok(None) => {}
                Err((run, message)) => {
                    return WebhookDispatch::FailedWithRun {
                        run: Box::new(run),
                        message,
                    }
                }
            }
        }

        match self
            .dispatcher()
            .dispatch_run(autopilot, Some(trigger_id), RunSource::Webhook, &run, None)
            .await
        {
            Ok(outcome) => WebhookDispatch::Run(Box::new(outcome.run)),
            Err(DispatchError::QuotaExceeded { .. }) => WebhookDispatch::QuotaExceeded,
            Err(DispatchError::Failed {
                run_id, message, ..
            }) => {
                // 上游此时拿到的是「已经写了失败态」的 run ⇒ 投递落 `failed`。本地按 id 回读一次，
                // 拿不到就把手上的旧行带上（投递仍然落 `failed`：下游已经动过了）。
                let fresh = run_sql::get(self.pool(), run_id).await.ok().unwrap_or(run);
                WebhookDispatch::FailedWithRun {
                    run: Box::new(fresh),
                    message,
                }
            }
            Err(err) => WebhookDispatch::Failed {
                message: err.to_string(),
            },
        }
    }

    /// `repairAutopilotRunTaskLink` 的**只补链接**子集。
    ///
    /// 上游在这里还会把「任务已终态」的 run 通过 `SyncRunFromTask` 重放成终态（并补发事件）。
    /// 本地不移植那一步（偏差 D6）：那条分支在本地**不可达** —— `run_only` 的 task 插入与
    /// `run.task_id` 回填在同一个事务里（`dispatch/run_only.rs`），所以「任务存在但 run 没挂上」
    /// 只能来自跨进程崩溃窗口，而那种情况下任务也不可能是终态。为了让不可达分支**失败安全**，
    /// 这里额外查一次任务状态：任务已终态就**不写** `running`（否则会把一个失败的 run 谎报成运行中），
    /// 只记 warn 交给运维。
    ///
    /// `Err((run, message))` = 已经有一枚 run 的失败面（上游 `return run, err`）。
    #[allow(clippy::type_complexity)] // 两态出口：补到链接 / 出错但带着 run，包成类型反而更难对读
    #[allow(clippy::result_large_err)] // 同上：那枚 run 是**必须**带出来的（上游 `return run, err`）
    async fn repair_run_task_link(
        &self,
        run: &AutopilotRunRow,
    ) -> Result<Option<AutopilotRunRow>, (AutopilotRunRow, String)> {
        let found = match sql::find_task_status_by_run(self.pool(), run.id).await {
            Ok(found) => found,
            Err(err) => {
                return Err((
                    run.clone(),
                    format!("dispatch for webhook delivery: lookup linked task: {err}"),
                ))
            }
        };
        let Some((task_id, status)) = found else {
            return Ok(None);
        };
        if matches!(status.as_str(), "completed" | "failed" | "cancelled") {
            tracing::warn!(
                run_id = %run.id,
                task_id = %task_id,
                task_status = %status,
                "webhook worker: task already terminal, not relinking (SyncRunFromTask not ported)"
            );
            return Ok(Some(run.clone()));
        }
        tracing::info!(run_id = %run.id, task_id = %task_id, "webhook worker: repairing run/task link");
        let mut conn = match self.pool().acquire().await {
            Ok(conn) => conn,
            Err(err) => {
                return Err((
                    run.clone(),
                    format!("dispatch for webhook delivery: acquire connection: {err}"),
                ))
            }
        };
        match run_sql::update_running(&mut conn, run.id, task_id).await {
            Ok(repaired) => Ok(Some(repaired)),
            Err(err) => Err((
                run.clone(),
                format!("dispatch for webhook delivery: repair task linkage: {err}"),
            )),
        }
    }

    /// `ProcessNext`：认领**一条**到期投递并把它推到终态。
    ///
    /// 返回 `Ok(None)` = 队列空；`Ok(Some(row))` = 这条投递已被处理（终态 / 推迟 / 重试）。
    ///
    /// **本片只提供这一步，不提供轮询循环**（`1s` ticker + `Notify` + 4 并发）—— 守护进程接线
    /// 属 M5-8（偏差 D8）。同步可驱动正是上游把 `ProcessNext` 公开给包内的原因，本地沿用。
    ///
    /// # Errors
    ///
    /// 认领阶段的库错；收口阶段的库错（[`WebhookError::Worker`]）。
    pub async fn process_next_delivery(&self) -> Result<Option<WebhookDeliveryRow>, WebhookError> {
        self.process_next_delivery_scoped(None).await
    }

    /// 只认领**指定 workspace** 的到期投递（[`Self::process_next_delivery`] 的作用域变体）。
    ///
    /// 生产接线（M5-8）用全局形态 —— 上游 `ClaimQueuedWebhookDelivery` 没有 workspace 过滤。
    /// 本变体给 e2e 用：认领是**整库**的，而同一个测试 binary 里的用例是并发跑的，全局认领会
    /// 把邻例刚落的 `queued` 行抢走（`deliveries_replay.rs` 正断言那条 `queued`）；每个用例有
    /// 自己的 workspace ⇒ 收窄即互不打扰。偏差登记见 `docs/54` D9。
    ///
    /// # Errors
    ///
    /// 同 [`Self::process_next_delivery`]。
    pub async fn process_next_delivery_in_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<Option<WebhookDeliveryRow>, WebhookError> {
        self.process_next_delivery_scoped(Some(workspace_id)).await
    }

    #[allow(clippy::too_many_lines)] // 108 行：上游 `ProcessNext` 就是这条线性顺序，切碎就看不出来了
    async fn process_next_delivery_scoped(
        &self,
        workspace_id: Option<Uuid>,
    ) -> Result<Option<WebhookDeliveryRow>, WebhookError> {
        // ① 认领（`FOR UPDATE SKIP LOCKED` + 2 分钟租约）。
        let claimed = match workspace_id {
            Some(workspace_id) => sql::claim_queued_in_workspace(self.pool(), workspace_id).await,
            None => sql::claim_queued(self.pool()).await,
        };
        let Some(delivery) = claimed.map_err(|err| worker_error("claim queued delivery", &err))?
        else {
            return Ok(None);
        };

        // ② per-trigger 派发预算（**只在 worker 侧消费**）。用尽 ⇒ 只推迟，**不计**派发尝试。
        if let Err(WebhookError::RateLimited { retry_after_secs }) =
            ratelimit::allow_trigger(delivery.trigger_id)
        {
            let available_at =
                Utc::now() + Duration::seconds(i64::try_from(retry_after_secs).unwrap_or(1));
            tracing::debug!(
                delivery_id = %delivery.id,
                trigger_id = %delivery.trigger_id,
                retry_after_secs,
                "webhook worker: per-trigger budget exhausted, deferring"
            );
            self.defer_delivery(&delivery, available_at).await?;
            return Ok(Some(delivery));
        }

        // ③ trigger（`ErrNoRows` 与库错都走重试/耗尽 —— 触发器可能是被并发删掉的）。
        let trigger = match sql::find_trigger_by_id(self.pool(), delivery.trigger_id).await {
            Ok(Some(trigger)) => trigger,
            Ok(None) => {
                return self
                    .retry_or_fail(&delivery, "load trigger: not found")
                    .await
                    .map(|()| Some(delivery))
            }
            Err(err) => {
                return self
                    .retry_or_fail(&delivery, &format!("load trigger: {err}"))
                    .await
                    .map(|()| Some(delivery))
            }
        };

        // ④ autopilot。
        let autopilot = match run_sql::get_autopilot(self.pool(), delivery.autopilot_id).await {
            Ok(row) => row,
            Err(RepoError::NotFound) => {
                return self
                    .retry_or_fail(&delivery, "load autopilot: not found")
                    .await
                    .map(|()| Some(delivery))
            }
            Err(err) => {
                return self
                    .retry_or_fail(&delivery, &format!("load autopilot: {err}"))
                    .await
                    .map(|()| Some(delivery))
            }
        };

        // ⑤ 归属交叉校验：trigger 必须真的属于这枚 autopilot，autopilot 必须在同一个 workspace。
        //    不一致 ⇒ `failed`（**不重试**：行本身脏了，重试多少次都一样）。
        if trigger.autopilot_id != delivery.autopilot_id
            || autopilot.workspace_id != delivery.workspace_id
        {
            tracing::warn!(
                delivery_id = %delivery.id,
                trigger_id = %trigger.id,
                "webhook worker: delivery ownership mismatch"
            );
            self.complete_delivery(
                &delivery,
                DELIVERY_STATUS_FAILED,
                None,
                Some(OWNERSHIP_MISMATCH),
                None,
            )
            .await?;
            return Ok(Some(delivery));
        }

        // ⑥ 从落库的头部子集重建 + 用原始 body 重新归一化（签名已经在校验期判过，worker 不重验）。
        let mut headers = WebhookHeaders::from_selected(
            &delivery.selected_headers,
            delivery.content_type.as_deref(),
        );
        if let Some(content_type) = delivery.content_type.as_deref() {
            headers.set("Content-Type", content_type);
        }
        let raw_body = delivery.raw_body.clone().unwrap_or_default();
        let mut envelope = match provider::normalize_webhook_payload(&raw_body, &headers) {
            Ok(envelope) => envelope,
            Err(err) => {
                self.complete_delivery(
                    &delivery,
                    DELIVERY_STATUS_FAILED,
                    None,
                    Some(&format!("normalize stored body: {err}")),
                    None,
                )
                .await?;
                return Ok(Some(delivery));
            }
        };
        // 入站时刻是**投递行的** `received_at`（重启后重放同一条投递必须得到同一个信封）。
        envelope.request.received_at = delivery
            .received_at
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let payload = match serde_json::to_value(&envelope) {
            Ok(payload) => payload,
            Err(err) => {
                return self
                    .retry_or_fail(&delivery, &format!("encode envelope: {err}"))
                    .await
                    .map(|()| Some(delivery))
            }
        };

        // ⑦ 只有在**没有已准入 run** 时才重查可变状态（崩溃窗口恢复）；有 run 说明入站已经答应过
        //    provider，那次决定是持久的（见模块头）。
        let has_admitted_run = match self.admitted_run_exists(delivery.id).await {
            Ok(found) => found,
            Err(err) => {
                return self
                    .retry_or_fail(&delivery, &format!("load admitted run: {err}"))
                    .await
                    .map(|()| Some(delivery))
            }
        };
        if !has_admitted_run {
            let ignored = if !trigger.enabled {
                Some(IgnoredReason::TriggerDisabled)
            } else if autopilot.status == "archived" {
                Some(IgnoredReason::AutopilotArchived)
            } else if autopilot.status != "active" {
                Some(IgnoredReason::AutopilotPaused)
            } else if !provider::event_allowed_by_trigger_scope(
                trigger.event_filters.as_ref(),
                &envelope,
            ) {
                Some(IgnoredReason::EventFiltered)
            } else {
                None
            };
            if let Some(reason) = ignored {
                self.complete_delivery(
                    &delivery,
                    DELIVERY_STATUS_IGNORED,
                    None,
                    Some(reason.as_str()),
                    None,
                )
                .await?;
                return Ok(Some(delivery));
            }
        }

        // ⑧ 派发（准入 + 副作用），再按结果收口。
        match self
            .dispatch_for_webhook_delivery(&autopilot, trigger.id, payload, delivery.id)
            .await
        {
            WebhookDispatch::Run(run) => {
                if run.status == "failed" {
                    let reason = run
                        .failure_reason
                        .clone()
                        .unwrap_or_else(|| "autopilot run failed".to_owned());
                    self.complete_delivery(
                        &delivery,
                        DELIVERY_STATUS_FAILED,
                        Some(run.id),
                        Some(&reason),
                        None,
                    )
                    .await?;
                    return Ok(Some(delivery));
                }
                // `last_fired_at` 打点失败只记日志：它不影响投递本身的可观测性。
                if let Err(err) = sql::touch_last_fired_at(self.pool(), trigger.id).await {
                    tracing::warn!(
                        trigger_id = %trigger.id,
                        error = %err,
                        "webhook worker: touch last_fired_at failed"
                    );
                }
                self.complete_delivery(
                    &delivery,
                    DELIVERY_STATUS_DISPATCHED,
                    Some(run.id),
                    None,
                    None,
                )
                .await?;
                Ok(Some(delivery))
            }
            WebhookDispatch::QuotaExceeded => {
                self.complete_delivery(
                    &delivery,
                    DELIVERY_STATUS_IGNORED,
                    None,
                    Some(QUOTA_EXCEEDED_MESSAGE),
                    Some(REASON_CODE_QUOTA_EXCEEDED),
                )
                .await?;
                Ok(Some(delivery))
            }
            WebhookDispatch::FailedWithRun { run, message } => {
                self.complete_delivery(
                    &delivery,
                    DELIVERY_STATUS_FAILED,
                    Some(run.id),
                    Some(&message),
                    None,
                )
                .await?;
                Ok(Some(delivery))
            }
            WebhookDispatch::Failed { message } => self
                .retry_or_fail(&delivery, &message)
                .await
                .map(|()| Some(delivery)),
        }
    }

    // ── worker 收口原语 ─────────────────────────────────────────────────────

    /// `GetAutopilotRunByWebhookDelivery` 的存在性判据（连接版查询，见 `run.rs` 签名）。
    async fn admitted_run_exists(&self, delivery_id: Uuid) -> Result<bool, RepoError> {
        let mut conn = self
            .pool()
            .acquire()
            .await
            .map_err(|err| RepoError::Db(err.to_string()))?;
        Ok(run_sql::find_by_webhook_delivery(&mut conn, delivery_id)
            .await?
            .is_some())
    }

    /// `complete`：写终态（`dispatched` / `ignored` / `failed`）。
    ///
    /// 租约被抢走（`None`）⇒ 新主人负责收口，本地静默返回（上游 `handleWebhookLeaseMutation`）。
    async fn complete_delivery(
        &self,
        delivery: &WebhookDeliveryRow,
        status: &str,
        run_id: Option<Uuid>,
        error: Option<&str>,
        reason_code: Option<&str>,
    ) -> Result<(), WebhookError> {
        let Some(lease_token) = delivery.lease_token else {
            tracing::debug!(delivery_id = %delivery.id, "webhook worker: no lease on delivery");
            return Ok(());
        };
        match sql::complete_claimed(
            self.pool(),
            delivery.id,
            lease_token,
            status,
            run_id,
            error,
            reason_code,
        )
        .await
        {
            Ok(Some(_)) => {
                tracing::debug!(
                    delivery_id = %delivery.id,
                    status,
                    "webhook worker: delivery completed"
                );
                Ok(())
            }
            Ok(None) => {
                tracing::debug!(
                    delivery_id = %delivery.id,
                    status,
                    "webhook worker: lease ownership changed"
                );
                Ok(())
            }
            Err(err) => Err(worker_error("complete claimed delivery", &err)),
        }
    }

    /// `retryOrFail`：还没到上限就退避重排，到了上限就判 `failed`。
    async fn retry_or_fail(
        &self,
        delivery: &WebhookDeliveryRow,
        cause: &str,
    ) -> Result<(), WebhookError> {
        if delivery.dispatch_attempts + 1 >= WEBHOOK_WORKER_MAX_ATTEMPTS {
            return self
                .complete_delivery(delivery, DELIVERY_STATUS_FAILED, None, Some(cause), None)
                .await;
        }
        let shift = delivery.dispatch_attempts.clamp(0, MAX_BACKOFF_SHIFT);
        let backoff = Duration::seconds(1i64 << shift);
        let available_at = Utc::now() + backoff;
        let Some(lease_token) = delivery.lease_token else {
            tracing::debug!(delivery_id = %delivery.id, "webhook worker: no lease on delivery");
            return Ok(());
        };
        match sql::retry_claimed(self.pool(), delivery.id, lease_token, available_at, cause).await {
            Ok(Some(_)) => {
                tracing::warn!(
                    delivery_id = %delivery.id,
                    attempt = delivery.dispatch_attempts + 1,
                    backoff_secs = backoff.num_seconds(),
                    cause,
                    "webhook worker: delivery deferred"
                );
                Ok(())
            }
            Ok(None) => {
                tracing::debug!(
                    delivery_id = %delivery.id,
                    "webhook worker: lease ownership changed"
                );
                Ok(())
            }
            Err(err) => Err(worker_error("retry claimed delivery", &err)),
        }
    }

    /// `DeferClaimedWebhookDelivery`：释放认领、不计数。
    async fn defer_delivery(
        &self,
        delivery: &WebhookDeliveryRow,
        available_at: chrono::DateTime<Utc>,
    ) -> Result<(), WebhookError> {
        let Some(lease_token) = delivery.lease_token else {
            tracing::debug!(delivery_id = %delivery.id, "webhook worker: no lease on delivery");
            return Ok(());
        };
        match sql::defer_claimed(self.pool(), delivery.id, lease_token, available_at).await {
            Ok(_) => Ok(()),
            Err(err) => Err(worker_error("defer claimed delivery", &err)),
        }
    }
}

/// worker 侧库错 → [`WebhookError::Worker`]（`Display` 带上下文，便于守护进程记日志）。
fn worker_error(context: &str, err: &impl std::fmt::Display) -> WebhookError {
    tracing::error!(error = %err, context, "webhook worker: internal error");
    WebhookError::Worker {
        message: format!("{context}: {err}"),
    }
}
