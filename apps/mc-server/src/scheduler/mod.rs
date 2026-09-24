//! M5-9（`LUM-1659`）：把 `mc-scheduler` 的**内核 + 两个 job**接进 `multica-server`。
//!
//! # 这一片解决什么问题
//!
//! M5-7 交付了调度内核、M5-8 交付了 `autopilot` / `issue_wakeup` 两个 job，但
//! `apps/mc-server/Cargo.toml` 里**没有 `mc-scheduler` 依赖边** ⇒ 那些代码只存在于工作区里，
//! 二进制既不注册 job 也不起循环：`sys_cron_executions` 永远没有新行，autopilot 的 schedule
//! trigger 不会被派发，issue wakeup 的收据永远不消费。本模块补上最后一段：
//!
//! 1. `Cargo.toml` 的三条边（`mc-repos` / `mc-autopilot` / `mc-scheduler`）—— 让代码真的被链接；
//! 2. 两个缺失的**数据面端口**生产实现（[`schedule_port`] / [`wakeup_port`]）；
//! 3. [`start`]：装配 3 个端口 + `register_all` + `spawn()`，返回 `SchedulerHandle`
//!    （`main.rs` 在 graceful shutdown 里 `await` 它的 `shutdown()`）。
//!
//! # 错误口径（两个端口统一）
//!
//! | 来源 | 变体 | 理由 |
//! | --- | --- | --- |
//! | `sqlx::Error`（本地手写 SQL） | [`SchedulerError::Repo`] | 与 `mc-repos` 的 `RepoError::Db` 同语义；`error.rs` 里它按**可重试**分类 |
//! | `WakeupError` / `DispatchError` | [`SchedulerError::Handler`] | 上游 handler 直接把错误 `return` ⇒ `classifyError` 的 default 分支（可重试）。`Display` 文案已与库/上游日志逐字对齐（M5-6 的设计） |
//!
//! 换句话说：**没有** `Permanent` —— 端口层的失败都是「这一轮这一行没跑成」，
//! 下一轮 tick 再试（与上游一致）。真正不可重试的只有 job 侧的 cron/时区解析失败（M5-8 的 D3）。
//!
//! # 与 `docs/55` §3.3 代码段的差异（逐条）
//!
//! | 差异 | 理由 |
//! | --- | --- |
//! | `McWakeupDispatchPort::new(db, hub)` 而不是 `(db, realtime)` | 第 7 步的两个出口都在 `Arc<mc_ws::hub::Hub>`（`notify_task_queued` 用户面 + `notify_task_available` daemon）；`RealtimeHandle` 是 `/live-events` 的另一条总线 |
//! | 端口在 `AppState` **之后**构造 | 需要 `state.daemon_hub`；`docs/55` 的片段写这段时 `AppState` 还没有 hub 这个设计 |
//! | 注册 3 个端口要 6 条依赖边（多 `sqlx`/`uuid`/`chrono`） | trait 签名里就有 `Uuid` / `DateTime<Utc>`，实现要写 `PgPool` / `PgConnection`；三条都是 workspace 依赖，不引入新的外部版本 |

pub mod schedule_port;
pub mod wakeup_port;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use mc_db::Db;
use mc_repos::RepoError;
use mc_scheduler::error::SchedulerError;
use mc_scheduler::jobs::JobPorts;
use mc_scheduler::{Manager, Options, SchedulerHandle, SchedulerRepo};
use mc_ws::hub::Hub;

pub use schedule_port::McAutopilotSchedulePort;
pub use wakeup_port::McWakeupDispatchPort;

/// 装配并启动调度循环（`main.rs` 的「6. 装配 axum 路由」之后、`axum::serve` 之前）。
///
/// 默认 `Options`：`runner_id = "mc-server-<pid>"`（审计行里一眼看出是哪个进程）、
/// tick 间隔取内核默认（30s，小于两个 job 的 cadence）。
///
/// # Errors
///
/// 只有 [`build`] 会失败（`JobSpec::validate` 的规格错误 —— 那是**开发者错误**，
/// 启动期就该炸，而不是让调度器静默少跑一个 job）。
pub fn start(db: &Db, hub: Arc<Hub>) -> anyhow::Result<SchedulerHandle> {
    Ok(build(
        db,
        hub,
        Options::default().with_runner_id(format!("mc-server-{}", std::process::id())),
    )?
    .spawn())
}

/// 装配（不启动）：注册两个 job，返回还没 `spawn` 的 `Manager`。
///
/// **顺序有语义**：`register_all` 必须在 `Manager::spawn` 之前（`spawn` 消费 `self`，
/// 注册表在那一刻冻结）。这个函数也是真库用例的缝：测试可以断言 `manager.jobs()`
/// 里确实有两个 job（注册是纯内存的，不需要数据库），再自己 `spawn()` 观察租约行。
pub fn build(db: &Db, hub: Arc<Hub>, options: Options) -> anyhow::Result<Manager> {
    let pool = db.pool().clone();
    let ports = JobPorts::new(
        Arc::new(McAutopilotSchedulePort::new(db)),
        // 派发面 M5-4 已有真实现，这里不重复实现准入 / 幂等 / 建 run。
        Arc::new(mc_autopilot::dispatch::AutopilotDispatcher::new(pool)),
        Arc::new(McWakeupDispatchPort::new(db, hub)),
    );
    let mut scheduler = Manager::new(SchedulerRepo::new(db.clone()), options);
    mc_scheduler::jobs::register_all(&mut scheduler, &ports)
        .map_err(|err| anyhow::anyhow!("register scheduler jobs: {err}"))?;
    Ok(scheduler)
}

/// `sqlx::Error` → [`SchedulerError::Repo`]（`RepoError::Db` 是 `mc-repos` 的既有约定）。
#[allow(clippy::needless_pass_by_value)] // 作为 `map_err` 的函数指针必须按值接收。
pub(super) fn repo_err(err: sqlx::Error) -> SchedulerError {
    SchedulerError::Repo(RepoError::Db(err.to_string()))
}

/// 「本轮这一行没跑成」的错误（`WakeupError` / `DispatchError` 都走这里）。
///
/// 用 `Handler(String)` 而不是 `Repo`：这些错误来自业务层，`error.rs` 的 `ErrorClass`
/// 把两者都算**可重试**，但 `Handler` 的 `Display` 会保留业务错误文案（写进 `sys_cron_executions`）。
pub(super) fn handler_err(err: impl std::fmt::Display) -> SchedulerError {
    SchedulerError::Handler(err.to_string())
}
