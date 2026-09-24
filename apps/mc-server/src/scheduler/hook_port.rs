//! M6-8（`LUM-1673`）：`mc_scheduler::jobs::plugin_hook::PluginHookPort` 的生产实现。
//!
//! # 为什么是一个薄适配层
//!
//! hook job 的**全部业务语义**都在 `mc_http::routes::plugins::hooks_job` 的引擎里（限流、
//! 熔断、`net:` 目的地检查、出站 HMAC、四个头、调用记录）—— 那是「宿主调出去」的唯一一处
//! 实现，路由（`POST /api/plugin-bridge/v1/hooks/:key`）与 job 共用它。本文件只做两件事：
//!
//! 1. 拼出引擎要的 [`HookRuntime`]（三个字段：`Db` / 部署密钥 / 开关目录）；
//! 2. 把 `SchedulerResult` 与 `HookError` 互相折算。
//!
//! # 为什么拿不到 `AppState`
//!
//! `apps/mc-server/src/main.rs`（M5-9 的接线）调的是 `scheduler::start(&db, daemon_hub)` ——
//! 一行不改是**硬约束**（见 issue 的写集修订）⇒ 端口实现只有 `Db` + `Hub`。
//! 引擎侧因此把依赖面收窄成 [`HookRuntime`]（`db` + `plugin_key` + `feature_flags`），
//! 而不是整份 `AppState`；`HookRuntime::standalone` 就是这条路径的构造点。
//!
//! # 错误口径
//!
//! | 来源 | 折成 | 理由 |
//! | --- | --- | --- |
//! | 仓储 / SQL | [`SchedulerError::Repo`] | 与 `mc-repos` 的 `RepoError::Db` 同语义（可重试） |
//! | hook 引擎的调用失败 | [`SchedulerError::Handler`] | 业务失败；`Display` 保留插件面的码与文案（进 `sys_cron_executions.error_msg`） |
//! | 「跳过」（未启用 / 换代 / manifest 变了 / 熔断） | **不是错误** | 上游把它们收成 `Skipped` 终态，本层原样透传 |

use mc_core::Id;
use mc_db::Db;
use mc_http::routes::plugins::hooks_job::{
    self, HookRuntime, ScheduledHookOutcome, ScheduledHookRequest,
};
use mc_repos::plugin::hook::HookScheduleRow;
use mc_repos::RepoError;
use mc_scheduler::error::{SchedulerError, SchedulerResult};
use mc_scheduler::jobs::plugin_hook::{PluginHookPort, ScheduleDispatchRequest, ScheduleOutcome};
use mc_scheduler::jobs::PortFuture;

/// 生产 hook 端口：把内核的四个方法接到 `mc-http` 的 hook 引擎上。
#[derive(Clone)]
pub struct McPluginHookPort {
    runtime: HookRuntime,
}

impl McPluginHookPort {
    /// 装配（调度循环只有 `Db`；部署密钥与开关目录由 [`HookRuntime::standalone`] 读）。
    #[must_use]
    pub fn new(db: &Db) -> Self {
        Self {
            runtime: HookRuntime::standalone(db.clone()),
        }
    }
}

impl PluginHookPort for McPluginHookPort {
    fn list_enabled_schedules(&self) -> PortFuture<'_, SchedulerResult<Vec<HookScheduleRow>>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            hooks_job::list_enabled_schedules(&runtime)
                .await
                .map_err(repo_err)
        })
    }

    fn load_schedule(
        &self,
        schedule_id: uuid::Uuid,
    ) -> PortFuture<'_, SchedulerResult<Option<HookScheduleRow>>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            hooks_job::load_schedule(&runtime, Id::from(schedule_id))
                .await
                .map_err(repo_err)
        })
    }

    fn dispatch_schedule(
        &self,
        request: ScheduleDispatchRequest,
    ) -> PortFuture<'_, SchedulerResult<ScheduleOutcome>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            let request = ScheduledHookRequest {
                schedule_id: Id::from(request.schedule_id),
                generation: Id::from(request.generation),
                plan_time: request.plan_time,
                attempt: request.attempt,
                last_attempt: request.last_attempt,
            };
            match hooks_job::dispatch_scheduled_hook(&runtime, request).await {
                Ok(ScheduledHookOutcome::Delivered { delivery_id }) => {
                    Ok(ScheduleOutcome::Delivered { delivery_id })
                }
                Ok(ScheduledHookOutcome::Skipped(reason)) => {
                    Ok(ScheduleOutcome::Skipped(reason.to_owned()))
                }
                // 调用失败（且不是最后一次尝试）：让内核按重试预算收尾。
                Err(error) => Err(SchedulerError::Handler(error.to_string())),
            }
        })
    }

    fn advance_next_run(
        &self,
        schedule_id: uuid::Uuid,
        generation: uuid::Uuid,
        plan_time: chrono::DateTime<chrono::Utc>,
    ) -> PortFuture<'_, SchedulerResult<u64>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            hooks_job::advance_schedule_next_run(
                &runtime,
                Id::from(schedule_id),
                Id::from(generation),
                plan_time,
            )
            .await
            .map_err(repo_err)
        })
    }
}

/// `mc-http` 侧的 hook 引擎回的是可读文案（它不引 `mc-scheduler` 的类型，方向也不该有这条边）
/// ⇒ 这里折成内核的**可重试**分类（与 `schedule_port.rs` 的 `repo_err` 同语义：
/// 「这一轮这一行没跑成，下个 tick 再试」）。
fn repo_err(message: String) -> SchedulerError {
    SchedulerError::Repo(RepoError::Db(message))
}
