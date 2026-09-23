//! `mc-scheduler`：cron 调度**租约内核**（M5-7 的内核 + M5-8 的两个 job）。
//!
//! **状态：M5-7 已落地**（`LUM-1566`，`docs/46-M5-7-SCHEDULER.md`）—— 内核 / 仓储 /
//! 集成测试 / 门禁全绿；**注册表仍为空**（`jobs::register_all` 是空实现，M5-8 往里加
//! autopilot 与 `issue_wakeup`）⇒ 本 crate 现在能「空转且独立验收」。
//!
//! ## 为什么是独立 crate（而不是塞进 `mc-http`）
//!
//! 调度器要往 `agent_task_queue` 写行、要读 M5 的领域类型，但**不能依赖 `mc-http`**（撞 800 行门
//! ⑩ 与分层）；同时它服务的不止 autopilot：M6 的 plugin-hook job 与 M3/M9 的 task-usage job
//! 共用这套内核（`docs/44` §1.2 把 `jobs_plugin_hook.go`353 / `jobs_task_usage.go`120 标成
//! 「本波只交付它要的内核」）⇒ 内核与 job 必须分层，crate 边界就是它。
//!
//! ## 上游与写者
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `src/lib.rs` / `src/error.rs` | M5-7 | crate 框架（§3.2 矩阵没有这两行，本 anchor 判给 M5-7） |
//! | `src/spec.rs` | M5-7 | `scheduler/spec.go`**261**（262） |
//! | `src/manager.rs` | M5-7 | `scheduler/manager.go`**489**（490） |
//! | `src/db_ops.rs` | M5-7 | `scheduler/db_ops.go`**402**（403） |
//! | `src/jobs/{autopilot,issue_wakeup}.rs` | M5-8 | `jobs_autopilot.go`448（449）+ `jobs_issue_wakeup.go`21（22） |
//! | `apps/mc-server/src/main.rs` 的 spawn 块 | M5-7 | ——（M5-8 再往注册表加 2 行 ⇒ 串行边） |
//!
//! ## 落地范围（M5-7）
//!
//! * `spec.rs`：`JobSpec`（+ builder / `validate`）、`CatchUpMode`、`Scope`、三种回调别名、
//!   `retry_delay`、`floor_plan`（**`plan_time` 取整契约**：与 Go `time.Time.Truncate` 同原点）。
//! * `db_ops.rs`：`try_claim`（新鲜插入 / 抢陈旧 / 重试到期三态）、`heartbeat`、`finish_success`、
//!   `finish_failure`、`mark_stale_as_failed`、`latest_plan` —— SQL 全在 `mc-repos` 的
//!   `scheduler.rs`，本层只把「影响 0 行」翻译成 [`SchedulerError::LeaseLost`]。
//! * `manager.rs`：`Options` / `Manager`（`register` / `run_once` / `spawn`）/ 每 tick 一轮 /
//!   handler 隔离（`tokio::spawn` + `timeout` + abort）/ 心跳任务 / `SchedulerHandle`（
//!   `shutdown().await` 接 `main.rs` 的关闭序列）。
//! * `error.rs`：`code()`（= 上游 `classifyError`）+ [`ErrorClass`]（重试 / 永久 / 租约已丢）
//!   —— 重试决策的**唯一**开关（`NULL` 的 `next_retry_at` 在 `RetryEligible` 里是「立刻重试」
//!   而不是「不再重试」，所以 `Permanent` 必须靠烧预算来表达）。
//!
//! ## 不变量（R2）
//!
//! `sys_cron_executions`（迁移 `113`）是**唯一同步点**：所有 job 的并发控制都走这张表的租约
//! （`try_claim` / 心跳 / 陈旧回收），**不要**在进程内再搞一套静态闸 —— 本仓在 M3-7 上已经栽过
//! 「进程级静态闸在『一进程多实例』的可测代码里是错的」这一跤（见 `docs/43` §G 系列）。
//!
//! ## 技术形态（`docs/44` §5.4 第 3 项）
//!
//! `manager.go` 是「每 tick 一次 `Run(ctx)`」⇒ 本地对应 `tokio::spawn` + `tokio::time::interval` +
//! `CancellationToken`（graceful shutdown 接 `apps/mc-server/src/main.rs` 的 `shutdown_signal`）。
//! M5-7 **不注册任何 job**（注册表空转）⇒ 它可独立验收；job 注册是 M5-8 的事。
//!
//! ## 不做什么
//!
//! - 不动 daemon 协议（只往 `agent_task_queue` 写行）
//! - 不引 `cron` crate（cron 解析在 `mc-autopilot/src/trigger.rs`，见其 `lib.rs` 的选型记录）
//! - 不新增迁移（`sys_cron_executions` 已存在）
//! - M5-7 里**没有** `main.rs` 的 spawn 代码：`apps/mc-server/Cargo.toml` 还没有
//!   `mc-scheduler` 依赖边，而本切片不改 manifest（见 `docs/46` §7 的待办与现成代码段）

pub mod db_ops;
pub mod error;
pub mod jobs;
pub mod manager;
pub mod spec;

// 便于 `main.rs` 只写一条依赖边（`mc-scheduler`）就够：内核自用的仓储类型与
// 三个入口类型在这里再导出一次。
pub use error::{ErrorClass, SchedulerError, SchedulerResult};
pub use manager::{Manager, Options, SchedulerHandle};
pub use mc_repos::scheduler::SchedulerRepo;
pub use spec::{CatchUpMode, HandlerInput, HandlerResult, JobSpec, Scope};
