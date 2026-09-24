//! job 粘合（`plugin_hook_schedule` 的一格）+ 端口实现要的三个入口。
//!
//! 从 `hooks_job.rs` 拆出来是门 ⑩ 的 800 行硬上限。

use super::{
    hook_allows_trigger, hook_breaker_open, invoke_hook, parse_installation_manifest,
    schedule_delivery_id, DateTime, HookActor, HookError, HookInvocation, HookRuntime,
    HookScheduleRepo, HookScheduleRow, HookTransport, HookTrigger, Id, InstallationRepo,
    InstallationRow, Manifest, RepoError, Utc,
};

// ---------------------------------------------------------------------------
// job 粘合：`plugin_hook_schedule` 的一格
// ---------------------------------------------------------------------------

/// 一次计划投递（`mc_scheduler::jobs::plugin_hook` 的端口入参）。
#[derive(Debug, Clone, Copy)]
pub struct ScheduledHookRequest {
    pub schedule_id: Id,
    pub generation: Id,
    pub plan_time: DateTime<Utc>,
    pub attempt: i32,
    /// 本轮是最后一次尝试（失败后必须推进 `next_run_at`，否则整条时间线卡住）。
    pub last_attempt: bool,
}

/// 一格投递的结论。`Skipped` 的字符串就是上游 `skipped_reason` 的取值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduledHookOutcome {
    Delivered { delivery_id: String },
    Skipped(&'static str),
}

/// 上游 `pluginHookScheduleHandler` 的**数据面**（纯逻辑部分在
/// `mc-scheduler/src/jobs/plugin_hook.rs`）。
///
/// 顺序逐条对齐上游：开关 → 日程行 → **换代检查** → 安装行 → 启用 → manifest 与日程一致 →
/// 熔断 → 调用。
///
/// # Errors
///
/// 只有「这一格没跑成」才回错误（调用失败且**不是**最后一次尝试）；其余全部折成
/// [`ScheduledHookOutcome::Skipped`]，让内核按终态收尾。
pub async fn dispatch_scheduled_hook(
    runtime: &HookRuntime,
    request: ScheduledHookRequest,
) -> Result<ScheduledHookOutcome, HookError> {
    if !runtime.plugins_v1_enabled() {
        return Ok(ScheduledHookOutcome::Skipped("feature_disabled"));
    }
    let schedule_repo = HookScheduleRepo::new(runtime.db.clone());
    let Some(row) = schedule_repo
        .get(request.schedule_id)
        .await
        .map_err(|error| HookError::unavailable(error.to_string()))?
    else {
        return Ok(ScheduledHookOutcome::Skipped("schedule_not_found"));
    };
    if !row.enabled || row.generation != request.generation.0 {
        return Ok(ScheduledHookOutcome::Skipped("schedule_generation_changed"));
    }
    let Some(installation) = load_installation(runtime, row.installation_id())
        .await
        .map_err(|error| HookError::unavailable(error.to_string()))?
    else {
        return Ok(ScheduledHookOutcome::Skipped("installation_not_found"));
    };
    if !installation.enabled {
        return Ok(ScheduledHookOutcome::Skipped("installation_disabled"));
    }
    let Ok(manifest) = parse_installation_manifest(&installation.manifest.0) else {
        return Ok(ScheduledHookOutcome::Skipped("manifest_changed"));
    };
    let Some(hook) = manifest
        .contributes
        .hooks
        .iter()
        .find(|hook| hook.key == row.hook_key)
        .cloned()
    else {
        return Ok(ScheduledHookOutcome::Skipped("manifest_changed"));
    };
    // 日程必须与**已同意的 manifest** 逐字一致：不一致说明投影落后于 manifest（或反过来），
    // 照旧跑就会按一个管理员没同意过的节奏调用。
    let matches_manifest = hook.schedule.as_ref().is_some_and(|schedule| {
        schedule.cron == row.cron_expression && schedule.timezone == row.timezone
    });
    if !matches_manifest
        || !hook_allows_trigger(&hook, HookTrigger::Schedule)
        || hook.transport.kind != HookTransport::Http.as_str()
    {
        return Ok(ScheduledHookOutcome::Skipped("manifest_changed"));
    }

    if hook_breaker_open(runtime, installation.id(), &hook.key).await {
        advance_next_run(runtime, &row, request.plan_time).await;
        return Ok(ScheduledHookOutcome::Skipped("circuit_open"));
    }

    let delivery_id = schedule_delivery_id(
        installation.id(),
        &hook.key,
        row.generation(),
        request.plan_time,
    );
    let invocation = HookInvocation {
        installation,
        hook,
        trigger: HookTrigger::Schedule,
        event_type: None,
        // 没有「人」可借 ⇒ 写归属到安装本身（上游 `HookActor{Type: "plugin", ID: installation.ID}`）。
        actor: HookActor {
            kind: mc_plugin_host::token::ActorKind::Plugin,
            id: row.installation_id(),
        },
        issue_id: None,
        input: None,
        delivery_id: Some(delivery_id.clone()),
        planned_at: Some(request.plan_time),
        attempt: request.attempt.max(1),
    };
    match invoke_hook(runtime, invocation).await {
        Ok(_) => {
            advance_next_run(runtime, &row, request.plan_time).await;
            Ok(ScheduledHookOutcome::Delivered { delivery_id })
        }
        Err(error) => {
            if request.last_attempt {
                advance_next_run(runtime, &row, request.plan_time).await;
            }
            Err(error)
        }
    }
}

/// 推进展示用的 `next_run_at`（上游 `advancePluginHookNextRun`）。
///
/// cron 解析失败**静默 return**（上游同）：`next_run_at` 只是展示列，派发正确性不依赖它。
async fn advance_next_run(runtime: &HookRuntime, row: &HookScheduleRow, plan_time: DateTime<Utc>) {
    let Ok(Some(next)) = mc_autopilot::cron::next_occurrence_after_utc(
        &row.cron_expression,
        &row.timezone,
        plan_time,
    ) else {
        return;
    };
    HookScheduleRepo::new(runtime.db.clone())
        .advance_next_run(row.id(), row.generation(), Some(next))
        .await
        .ok();
}

/// 上游 `pluginHookScheduleScopes` 的读面：本 tick 要处理的日程。
///
/// # Errors
///
/// 库错折成可读文案（端口层再折成 `SchedulerError::Repo`）。
pub async fn list_enabled_schedules(runtime: &HookRuntime) -> Result<Vec<HookScheduleRow>, String> {
    if !runtime.plugins_v1_enabled() {
        return Ok(Vec::new());
    }
    HookScheduleRepo::new(runtime.db.clone())
        .list_enabled()
        .await
        .map_err(|error| error.to_string())
}

/// 按 id 重读日程（端口层的 `load_schedule`）。
///
/// # Errors
///
/// 库错折成可读文案。
pub async fn load_schedule(
    runtime: &HookRuntime,
    id: Id,
) -> Result<Option<HookScheduleRow>, String> {
    HookScheduleRepo::new(runtime.db.clone())
        .get(id)
        .await
        .map_err(|error| error.to_string())
}

/// 端口层的 `advance_next_run`：读行 → 算下一格 → 带**代守卫**地写回。
///
/// # Errors
///
/// 库错 / 日程不存在 ⇒ 可读文案（端口层再折成 `SchedulerError::Repo`）。
/// cron 解析失败**不是**错误（`next_run_at` 只是展示列）：回 `Ok(0)`。
pub async fn advance_schedule_next_run(
    runtime: &HookRuntime,
    schedule_id: Id,
    generation: Id,
    plan_time: DateTime<Utc>,
) -> Result<u64, String> {
    let repo = HookScheduleRepo::new(runtime.db.clone());
    let Some(row) = repo
        .get(schedule_id)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Err(format!("plugin hook schedule {schedule_id} not found"));
    };
    let Ok(Some(next)) = mc_autopilot::cron::next_occurrence_after_utc(
        &row.cron_expression,
        &row.timezone,
        plan_time,
    ) else {
        return Ok(0);
    };
    repo.advance_next_run(schedule_id, generation, Some(next))
        .await
        .map_err(|error| error.to_string())
}

/// 按 id 读安装行（job 侧与路由侧共用的那一处）。
async fn load_installation(
    runtime: &HookRuntime,
    id: Id,
) -> Result<Option<InstallationRow>, RepoError> {
    match InstallationRepo::new(runtime.db.clone())
        .get_by_id(id)
        .await
    {
        Ok(row) => Ok(Some(row)),
        Err(RepoError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

// ---------------------------------------------------------------------------
// 日程投影的对齐（上游 `service/plugin_schedule.go`）
// ---------------------------------------------------------------------------
//
// **跨片缺口回填**：`docs/32` §9.6 的 M6-5-D2 写着「`plugin_hook_schedule` 的读写全部归 M6-8，
// 安装/升级/启停三处的 `reconcilePluginHookSchedules` 由 M6-8 补」。本节的三个函数就是那个回填，
// 调用点在 `install/lifecycle.rs`（创建/升级）与 `install/settings.rs`（启停）。

/// 上游 `reconcilePluginHookSchedules`：把投影对齐到**管理员同意过的** manifest。
///
/// # Errors
///
/// manifest 里的 cron/时区解析不了 ⇒ 400（上游 `schedule for hook %q is invalid`）——
/// 宁可让安装失败，也不要落一个运行期算不出来的日程。
pub(crate) async fn reconcile_schedules_tx(
    tx: &mut mc_repos::plugin::installation::Tx<'_>,
    installation: &InstallationRow,
    manifest: &Manifest,
) -> Result<(), HookError> {
    let now = Utc::now();
    let mut inputs = Vec::new();
    for hook in &manifest.contributes.hooks {
        let Some(schedule) = hook.schedule.as_ref() else {
            continue;
        };
        if !hook_allows_trigger(hook, HookTrigger::Schedule) {
            continue;
        }
        inputs.push(mc_repos::plugin::hook::HookScheduleInput {
            hook_key: hook.key.clone(),
            cron_expression: schedule.cron.clone(),
            timezone: schedule.timezone.clone(),
            // 停用的安装没有有意义的「下一次」（上游同）。
            next_run_at: if installation.enabled {
                next_run_after(&schedule.cron, &schedule.timezone, now).map_err(|()| {
                    HookError::invalid(format!("schedule for hook {:?} is invalid", hook.key))
                })?
            } else {
                None
            },
        });
    }
    mc_repos::plugin::hook::reconcile_tx(tx, installation, &inputs)
        .await
        .map_err(|_| HookError::unavailable("reconcile plugin hook schedules"))
}

/// 上游 `setPluginHookSchedulesEnabled`：停用清 `next_run_at`；启用**换一代**再开。
///
/// # Errors
///
/// 库错 ⇒ 502（调用方在启停事务里，失败即整个启停失败）。
pub(crate) async fn set_schedules_enabled_tx(
    tx: &mut mc_repos::plugin::installation::Tx<'_>,
    installation_id: Id,
    enabled: bool,
) -> Result<(), HookError> {
    mc_repos::plugin::hook::set_enabled_tx(tx, installation_id, enabled, |row| {
        if !enabled {
            return None;
        }
        next_run_after(&row.cron_expression, &row.timezone, Utc::now())
            .ok()
            .flatten()
    })
    .await
    .map_err(|_| HookError::unavailable("update plugin hook schedules"))
}

/// 上游 `pluginScheduleNextRun`：算下一次（`Err(())` = cron/时区非法；`Ok(None)` = 视界内没有了）。
fn next_run_after(
    cron_expression: &str,
    timezone: &str,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, ()> {
    mc_autopilot::cron::next_occurrence_after_utc(cron_expression, timezone, now).map_err(|_| ())
}
