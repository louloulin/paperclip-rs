//! 编排面：入站 12 步 + worker 认领/派发/收口。
//!
//! - **写者**：M5-5。
//! - **上游**：`handler/autopilot_webhook.go` 的 `HandleAutopilotWebhook`（12 步）、
//!   `handler/webhook_delivery_worker.go` 的 `ProcessNext`/`complete`/`retryOrFail`/
//!   `handleWebhookLeaseMutation`，以及 `service/autopilot.go` 的
//!   `AdmitAutopilotWebhookDelivery` / `DispatchAutopilotForWebhookDelivery`（含
//!   `repairAutopilotRunTaskLink` 的**只补链接**子集）。
//!
//! # 两段式（本片最重要的一条设计，见 `mod.rs` 模块头）
//!
//! - **A 段 = [`WebhookIngress::handle_inbound`]**：同步走到「run 已准入」为止 ——
//!   `should_skip_dispatch` → `record_skipped`，或 `initial_status` → `create_run_with_quota`。
//!   **到此为止**：不建 issue、不建 task、不派发。
//! - **B 段 = [`WebhookIngress::process_next_delivery`]**：认领 → 限流 → 装载 → 交叉校验 →
//!   归一化 → `dispatch_run`（真正的副作用面）→ 终态收口。
//!
//! 两段共用 [`WebhookIngress::admit_webhook_delivery`]（上游也是同一个
//! `AdmitAutopilotWebhookDelivery` 既被入站调、又被 worker 调）。所以「入站已建 run」与
//! 「worker 复用同一个 run」走的是同一段代码，不会出现两份准入逻辑漂移。
//!
//! # worker 的「已准入」判据（`ProcessNext` 里最容易看漏的一段）
//!
//! 上游 worker 只有在**查不到该投递的 run** 时才重查 trigger/autopilot 的可变状态
//! （disabled / archived / paused / `event_filtered`）。原因是：入站已经同步准入过，
//! 那次决定是**持久的**；若响应发出后运维立刻暂停 autopilot，重查会让一个已经答应了
//! provider 的 run 永远停在 `issue_created`。本地逐字照抄这个分支（见
//! [`WebhookIngress::process_next_delivery`] ⑥）。
//!
//! # 租约语义（`handleWebhookLeaseMutation` 的本地形态）
//!
//! `complete_claimed` / `retry_claimed` / `defer_claimed` 三条 SQL 都带
//! `lease_token = $2 AND status = 'queued'` 双条件 ⇒ 租约被抢走时返回 `None`。
//! 那是**正常竞态**（慢 worker 活过了租约，新主人负责收口），既不该报错也不该记指标 ——
//! 本地统一把 `None` 折成 `Ok(())` + debug 日志。

use mc_repos::autopilot::ingress as sql;
use mc_repos::autopilot::ingress::{CreateDeliveryOutcome, NewWebhookDelivery};
use mc_repos::autopilot::run::{self as run_sql, AutopilotRunRow};
use mc_repos::autopilot::{AutopilotRow, AutopilotTriggerRow};
use mc_repos::RepoError;
use serde_json::Value;
use uuid::Uuid;

use crate::dispatch::admission as dispatch_admission;
use crate::dispatch::admission::CreateRunError;
use crate::dispatch::DispatchRequest;

use super::provider;
use super::ratelimit;
use super::signature;
use super::{
    IgnoredReason, InboundOutcome, InboundRequest, RejectedReason, SigStatus, WebhookEnvelope,
    WebhookError, WebhookIngress, DELIVERY_STATUS_IGNORED, DELIVERY_STATUS_REJECTED,
    MAX_WEBHOOK_BODY_BYTES, QUOTA_EXCEEDED_MESSAGE, REASON_CODE_QUOTA_EXCEEDED,
};

// worker 面的常量（`WEBHOOK_WORKER_MAX_ATTEMPTS` 等）与结果枚举 `WebhookDispatch` 在
// `webhook::worker` —— 同一个 `impl WebhookIngress` 的另一半，拆文件只为 R7 的 800 行硬上限。

/// 准入失败面（上游 `AdmitAutopilotWebhookDelivery` 的 `error`）。
///
/// `pub(super)`：worker 面（`webhook::worker`）也要分这两个错。
pub(super) enum AdmitRefusal {
    /// 配额拒绝。
    QuotaExceeded,
    /// 库错 / 准入闸报错。**原始信息只进日志与 `WebhookDispatch::Failed` 的 message**。
    Repo(String),
}

impl WebhookIngress {
    // ── A 段：入站 ──────────────────────────────────────────────────────────

    /// `HandleAutopilotWebhook`（12 步）。返回 [`InboundOutcome`]；HTTP 状态码映射在 `mc-http`。
    ///
    /// # Errors
    ///
    /// [`WebhookError`]：`NotFound`（空/未知 token、autopilot 缺失、workspace 交叉校验失败）、
    /// `Invalid`（body 不是 JSON 对象/数组）、`PayloadTooLarge`、`RateLimited`（两道 IP 闸）、
    /// `Internal` / `AdmitFailed`。
    pub async fn handle_inbound(
        &self,
        req: &InboundRequest<'_>,
    ) -> Result<InboundOutcome, WebhookError> {
        let (trigger, autopilot) = self.resolve_webhook_target(req).await?;

        // ⑤ 归一化（上游第 5 步）。失败 ⇒ 400 且**不落库**：无法解析的 body 里没有可信的去重标识，
        //    重放它也没有意义。
        let envelope = provider::normalize_webhook_payload(req.body, &req.headers)
            .map_err(|message| WebhookError::Invalid { message })?;
        // 上游在同一位置 `json.Marshal(envelope)`；本地 `WebhookEnvelope` 只含 String/Value，
        //    序列化不可能失败（真失败了也只是把 500 文案折成 `internal error`，见 `docs/54` D7）。
        let payload = serde_json::to_value(&envelope).map_err(|err| {
            tracing::error!(error = %err, "webhook: failed to encode envelope");
            WebhookError::Internal
        })?;

        // ⑥ provider + 去重键 + 签名（上游第 6 步）。
        let provider_name = provider::provider_or_default(&trigger.provider);
        let (dedupe_key, dedupe_source) = provider::extract_dedupe_key(provider_name, &req.headers);
        let sig_status = signature::verify_signature(
            // 上游 `trigger.SigningSecret` 是 TEXT NOT NULL（可为空串）：空串 ⇒ `not_required`。
            trigger.signing_secret.as_deref().unwrap_or(""),
            &req.headers,
            req.body,
        );

        // ⑦ 落库（上游第 7 步）。`create_delivery` 是「INSERT … ON CONFLICT DO NOTHING + 回读 +
        //    自增」，与上游「先 INSERT、撞 23505 再回读」等价且并发安全。
        let delivery_id = Uuid::new_v4();
        let created = sql::create_delivery(
            self.pool(),
            &NewWebhookDelivery {
                id: delivery_id,
                workspace_id: autopilot.workspace_id,
                autopilot_id: autopilot.id,
                trigger_id: trigger.id,
                provider: provider_name.to_owned(),
                event: envelope.event.clone(),
                dedupe_key,
                dedupe_source,
                signature_status: sig_status.as_str().to_owned(),
                status: super::DELIVERY_STATUS_QUEUED.to_owned(),
                selected_headers: req.headers.to_selected_json(),
                content_type: envelope.request.content_type.clone(),
                raw_body: req.body.to_vec(),
                reason_code: None,
            },
        )
        .await
        .map_err(|err| Self::internal_error("persist delivery failed", &err))?;

        match created {
            // ⑧ 去重命中（上游第 7 步的 dup 分支）：不写响应字段、不改状态，只自增 attempt_count。
            CreateDeliveryOutcome::Duplicate(delivery) => {
                let mut run_id = delivery.autopilot_run_id;
                if run_id.is_none() {
                    // `find_by_webhook_delivery` 要 `&mut PgConnection`（它不是池版查询）。
                    let mut conn = match self.pool().acquire().await {
                        Ok(conn) => conn,
                        Err(err) => {
                            return Err(Self::internal_error("resolve duplicate run failed", &err))
                        }
                    };
                    match run_sql::find_by_webhook_delivery(&mut conn, delivery.id).await {
                        Ok(Some(run)) => run_id = Some(run.id),
                        Ok(None) => {}
                        Err(err) => {
                            return Err(Self::internal_error("resolve duplicate run failed", &err))
                        }
                    }
                }
                tracing::debug!(
                    delivery_id = %delivery.id,
                    attempt_count = delivery.attempt_count,
                    "webhook: duplicate delivery"
                );
                Ok(InboundOutcome::Duplicate {
                    delivery_id: delivery.id,
                    run_id,
                })
            }
            CreateDeliveryOutcome::Created(delivery) => {
                self.settle_inbound(
                    &trigger,
                    &autopilot,
                    &envelope,
                    payload,
                    sig_status,
                    delivery.id,
                    req.peer_ip,
                )
                .await
            }
        }
    }

    /// 落库之后的分支（上游第 8–12 步）：签名拒 → 状态 ignored → 事件过滤 → 同步准入。
    ///
    /// 抽出来只因 R7 的行数纪律；顺序与上游**逐条对应**，不要重排。
    #[allow(clippy::too_many_arguments)] // 7 个入参各自都是上游第 8–12 步的输入，装结构体反而更难对读
    #[allow(clippy::too_many_lines)] // 12 步里第 8–12 步的线性展开，拆函数会把「顺序即契约」打散
    async fn settle_inbound(
        &self,
        trigger: &AutopilotTriggerRow,
        autopilot: &AutopilotRow,
        envelope: &WebhookEnvelope,
        payload: Value,
        sig_status: SigStatus,
        delivery_id: Uuid,
        peer_ip: Option<&str>,
    ) -> Result<InboundOutcome, WebhookError> {
        // ⑨ 签名不合法 / 缺失 ⇒ `rejected` + 401，并给 IP 记一笔坏凭据债。
        if matches!(sig_status, SigStatus::Invalid | SigStatus::Missing) {
            let reason = if sig_status == SigStatus::Missing {
                RejectedReason::MissingSignature
            } else {
                RejectedReason::InvalidSignature
            };
            let outcome = InboundOutcome::Rejected {
                delivery_id,
                reason,
            };
            ratelimit::charge_bad_credential(peer_ip);
            self.finalise_terminal(
                delivery_id,
                DELIVERY_STATUS_REJECTED,
                reason.as_str(),
                None,
                401,
                &outcome.body(),
            )
            .await;
            return Ok(outcome);
        }

        // ⑩ trigger 停用 / autopilot 归档 / 暂停 ⇒ `ignored` + 200（让对方的 webhook 重试机
        //    制停下；原因留在 delivery 行上供运维查看）。
        let ignored = if !trigger.enabled {
            Some(IgnoredReason::TriggerDisabled)
        } else if autopilot.status == "archived" {
            Some(IgnoredReason::AutopilotArchived)
        } else if autopilot.status != "active" {
            Some(IgnoredReason::AutopilotPaused)
        } else {
            None
        };
        if let Some(reason) = ignored {
            let outcome = InboundOutcome::Ignored {
                delivery_id,
                reason,
            };
            self.finalise_terminal(
                delivery_id,
                DELIVERY_STATUS_IGNORED,
                reason.as_str(),
                None,
                200,
                &outcome.body(),
            )
            .await;
            return Ok(outcome);
        }

        // ⑪ 事件不在 trigger 作用域内 ⇒ `ignored` + 200（响应里回显事件名便于排障）。
        if !provider::event_allowed_by_trigger_scope(trigger.event_filters.as_ref(), envelope) {
            let outcome = InboundOutcome::EventFiltered {
                delivery_id,
                event: envelope.event.clone(),
            };
            self.finalise_terminal(
                delivery_id,
                DELIVERY_STATUS_IGNORED,
                IgnoredReason::EventFiltered.as_str(),
                None,
                200,
                &outcome.body(),
            )
            .await;
            return Ok(outcome);
        }

        // ⑫ 同步准入（**没有副作用**，见模块头两段式）。
        match self
            .admit_webhook_delivery(autopilot, trigger.id, payload, delivery_id)
            .await
        {
            Ok(run) => {
                let outcome = if run.status == "skipped" {
                    InboundOutcome::Skipped {
                        delivery_id,
                        run_id: run.id,
                        reason: run.failure_reason,
                    }
                } else {
                    InboundOutcome::Accepted {
                        delivery_id,
                        run_id: run.id,
                        autopilot_id: autopilot.id,
                        trigger_id: trigger.id,
                    }
                };
                self.acknowledge(delivery_id, &outcome).await;
                Ok(outcome)
            }
            Err(AdmitRefusal::QuotaExceeded) => {
                let outcome = InboundOutcome::QuotaExceeded { delivery_id };
                self.finalise_terminal(
                    delivery_id,
                    DELIVERY_STATUS_IGNORED,
                    QUOTA_EXCEEDED_MESSAGE,
                    Some(REASON_CODE_QUOTA_EXCEEDED),
                    200,
                    &outcome.body(),
                )
                .await;
                Ok(outcome)
            }
            Err(AdmitRefusal::Repo(message)) => {
                // 上游：**不动** delivery 行（留在 `queued`），叫醒 worker 让它稍后重试准入，
                // 然后回 500。本地无轮询循环（M5-8 才有），投递照样留在队列里等下一次 sweep。
                tracing::warn!(
                    delivery_id = %delivery_id,
                    trigger_id = %trigger.id,
                    autopilot_id = %autopilot.id,
                    reason = %message,
                    "webhook: admission failed, delivery stays queued"
                );
                Err(WebhookError::AdmitFailed)
            }
        }
    }

    /// 上游第 1–4 步：空 token → 两道 IP 闸 → token 查 trigger → body 上限 → autopilot + 交叉校验。
    async fn resolve_webhook_target(
        &self,
        req: &InboundRequest<'_>,
    ) -> Result<(AutopilotTriggerRow, AutopilotRow), WebhookError> {
        // ① 空 token 在任何限流**之前**返回 404，也不记账。
        if req.token.is_empty() {
            return Err(WebhookError::NotFound);
        }

        // ② 绝对 IP 天花板（消费）+ 坏凭据债（非消费 check）。
        ratelimit::gate_before_lookup(req.peer_ip)?;

        // ③ token → trigger。上游刻意区分「没有行」（404，且**不泄漏哪些 token 存在过**）与
        //    「库错」（500）：把库错也折成 404 会让一次瞬时故障静默丢投递 ——
        //    provider 不会对 404 重试。
        let lookup = sql::find_webhook_trigger_by_token(self.pool(), req.token)
            .await
            .map_err(|err| Self::internal_error("token lookup failed", &err))?;
        let Some(lookup) = lookup else {
            ratelimit::charge_bad_credential(req.peer_ip);
            return Err(WebhookError::NotFound);
        };

        // ④ body 上限由 HTTP 层在读流阶段执行（超限直接 413）；服务层再断言一次，防止
        //    其它调用方（worker / 测试）绕过 HTTP 层把超大 body 送进来。
        if req.body.len() > MAX_WEBHOOK_BODY_BYTES {
            return Err(WebhookError::PayloadTooLarge);
        }

        // ⑤ autopilot 装载 + workspace 交叉校验（在**落库之前** —— `webhook_delivery.workspace_id`
        //    是 NOT NULL，等 INSERT 才发现脏 FK 就等于白读了一遍 body）。
        let autopilot = match run_sql::get_autopilot(self.pool(), lookup.trigger.autopilot_id).await
        {
            Ok(row) => row,
            Err(RepoError::NotFound) => return Err(WebhookError::NotFound),
            Err(err) => return Err(Self::internal_error("autopilot lookup failed", &err)),
        };
        if autopilot.workspace_id != lookup.autopilot_workspace_id {
            tracing::warn!(
                trigger_id = %lookup.trigger.id,
                autopilot_id = %autopilot.id,
                "webhook: trigger workspace mismatch"
            );
            return Err(WebhookError::NotFound);
        }

        Ok((lookup.trigger, autopilot))
    }

    // ── 准入（入站与 worker 共用）───────────────────────────────────────────

    /// `AdmitAutopilotWebhookDelivery`：建出（或复用）该投递的幂等 run，**不做任何副作用**。
    ///
    /// 幂等由两处保证：`create_run_with_quota` 内部的
    /// `find_existing_run`（`webhook_delivery_id` 线）与其冲突回读，以及
    /// `uq_autopilot_run_webhook_delivery` 唯一索引。上游还额外做了一次
    /// `GetAutopilotRunByWebhookDelivery` 预查 —— 本地那次预查已经在 `find_existing_run` 里，
    /// 重复一次只会多一次往返。
    pub(super) async fn admit_webhook_delivery(
        &self,
        autopilot: &AutopilotRow,
        trigger_id: Uuid,
        payload: Value,
        delivery_id: Uuid,
    ) -> Result<AutopilotRunRow, AdmitRefusal> {
        let req =
            DispatchRequest::for_webhook(autopilot, Some(trigger_id), Some(payload), delivery_id);
        match dispatch_admission::should_skip_dispatch(self.pool(), autopilot).await {
            Ok(Some(skipped)) => self
                .dispatcher()
                .record_skipped(&req, &skipped)
                .await
                .map_err(|err| AdmitRefusal::Repo(err.to_string())),
            Ok(None) => {
                let initial = dispatch_admission::initial_status(&autopilot.execution_mode);
                match self.dispatcher().create_run_with_quota(&req, initial).await {
                    Ok((run, _reused)) => Ok(run),
                    Err(CreateRunError::QuotaExceeded { .. }) => Err(AdmitRefusal::QuotaExceeded),
                    Err(CreateRunError::Repo(err)) => Err(AdmitRefusal::Repo(err.to_string())),
                }
            }
            Err(err) => Err(AdmitRefusal::Repo(err.to_string())),
        }
    }

    // ── 响应记录 ────────────────────────────────────────────────────────────

    /// `AcknowledgeWebhookDelivery`：把已发出的 200 响应写回去，**不动 `status`**。
    ///
    /// 失败只记日志（上游同款）：响应已经在飞了，事后写不进去不该改变对 provider 的答复。
    /// 与 [`Self::finalise_terminal`] 分开写是为了让「已接受」这条路径**结构上**不可能顺手改状态。
    async fn acknowledge(&self, delivery_id: Uuid, outcome: &InboundOutcome) {
        let body = response_body(outcome);
        if let Err(err) = sql::acknowledge(self.pool(), delivery_id, 200, &body).await {
            tracing::warn!(
                delivery_id = %delivery_id,
                error = %err,
                "webhook: persist acknowledgement metadata failed"
            );
        }
    }

    /// `UpdateWebhookDeliveryTerminal`：入站即终态（`rejected` / `ignored`）。
    ///
    /// 失败只记日志（上游同款）。
    async fn finalise_terminal(
        &self,
        delivery_id: Uuid,
        status: &str,
        error: &str,
        reason_code: Option<&str>,
        http_status: i32,
        outcome_body: &Value,
    ) {
        let body = outcome_body.to_string();
        if let Err(err) = sql::update_terminal(
            self.pool(),
            delivery_id,
            status,
            Some(error),
            reason_code,
            Some(http_status),
            Some(&body),
        )
        .await
        {
            tracing::warn!(
                delivery_id = %delivery_id,
                status,
                error = %err,
                "webhook: finalise terminal failed"
            );
        }
    }

    /// 库错 → 500 形态。**原始信息只进日志**（无认证入口不回显内部细节）。
    fn internal_error(context: &str, err: &impl std::fmt::Display) -> WebhookError {
        tracing::error!(error = %err, context, "webhook: internal error");
        WebhookError::Internal
    }
}
/// 响应体的 JSON 串（与 `mc-http` 写出去的那份**必须**逐字节一致）。
fn response_body(outcome: &InboundOutcome) -> String {
    outcome.body().to_string()
}
