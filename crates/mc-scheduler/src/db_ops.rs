//! 租约表的读写（`sys_cron_executions`）。
//!
//! - **写者**：M5-7。
//! - **上游**：`scheduler/db_ops.go`402（403）—— `tryClaim`110 + `markStaleAsFailed`26 +
//!   `finishFailure`46 + `RetryEligible`18。
//! - **租约四条**（`docs/44` §6.2）：认领要原子（`RETURNING`）、心跳要续期、
//!   陈旧要回收（`markStaleAsFailed`）、失败要按 `RetryEligible` 决定是否留待重试。
//! - **仓库层**：本 crate **不**直接拿 `sqlx::Pool` 写 SQL；表访问走 `mc_repos::scheduler`
//!   （M5-7 的仓储片，写者也是 M5-7，所以同一 PR 内自洽；别的切片不要加查询）。
