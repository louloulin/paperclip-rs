//! 调度租约表仓储：`sys_cron_executions`（迁移 `113`）。
//!
//! - **状态**：M5-0 anchor（`LUM-1563`）只建文件，**0 查询、0 类型**（`docs/44` §5.3）。
//! - **写者**：M5-7（**W**；`docs/44` §3.2）。M5-8 只读内核算出的结果，不写本文件。
//! - **这是 M5 唯一的跨波共享点**（R2）：M5 的 autopilot job、M6 的 plugin-hook job、
//!   M3/M9 的 task-usage job 都通过这张表做并发控制 ⇒ 本文件的操作要按「内核」写，不要按
//!   「autopilot 专用」写（别在这里出现 autopilot 语义）。
//! - **要落的入口**：原子认领（`tryClaim`110）、心跳续期、陈旧回收（`markStaleAsFailed`26）、
//!   终态（`finishFailure`46 + `RetryEligible`18）。
//! - **上游**：`scheduler/db_ops.go`402（403）；`sys_cron_executions` 的表结构见迁移 `113`。
//! - **本仓约定**：裸 `Uuid` / `chrono` 类型 + 手写 `sqlx::FromRow` +
//!   `crate::workspace::map_sqlx_err`；认领必须用 `UPDATE ... RETURNING` 之类的**单语句原子**路径。
