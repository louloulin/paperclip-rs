//! `mc-scheduler`：cron 调度**租约内核**（M5-7 的内核 + M5-8 的两个 job）。
//!
//! **状态：M5-0 anchor（`LUM-1563` / `docs/44-M5-PLAN.md` §5.2）只落文件与边界** —— 0 类型、
//! 0 实现、0 job 注册。函数签名随 M5-7 / M5-8 落。
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

pub mod db_ops;
pub mod error;
pub mod jobs;
pub mod manager;
pub mod spec;
