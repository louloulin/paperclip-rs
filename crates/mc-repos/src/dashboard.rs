//! `DashboardRepo` —— dashboard 6 条的**只读聚合**（**写者 M9-4** / `docs/62` §9.6）。
//!
//! # anchor 期是**空桩**
//!
//! 本文件由 M9-0 anchor 建目录格（`crates/mc-repos/src/lib.rs` 的 `pub mod dashboard;`），
//! M9-4 原地填充。
//!
//! # M9-4 要填什么（**只读**，不自建聚合）
//!
//! | 路由 | 表 | 口径要点 |
//! | --- | --- | --- |
//! | `usage/daily` | `task_usage_hourly` | `SUM` 四类 token + `cost_usd_ticks` + `task_count`，按 `DATE(bucket_hour AT TIME ZONE tz)` + `LOWER(provider)` + `model` 分组；`uncosted_*` 用 `COALESCE(uncosted_x, x)` |
//! | `usage/by-agent` | `task_usage_hourly` | 同上，分组换 `agent_id` |
//! | `agent-runtime` | `agent_task_queue` ⋈ `agent` ⋈ `issue` | `SUM(EXTRACT(EPOCH FROM (completed_at - started_at)))` + 三个计数 + 「计过费」的 `EXISTS (SELECT 1 FROM task_usage …)` |
//! | `runtime/daily` | 同上 | 按 `DATE(completed_at AT TIME ZONE tz)` 分组 |
//! | `failures/daily` / `failures/by-agent` | 同上 | 按 `failure_reason` 计数（含"从未 started"的任务） |
//!
//! 🔴 **6 条里 0 条读 `task_usage_dashboard_*`**（`084` 的两张 legacy rollup 表已被
//! `101`/`103` 的 hourly 化取代 —— `103_drop_legacy_daily_rollups.up.sql` 逐字
//! 「drop legacy daily rollups」）⇒ M9-4 的用例要**断言不 `SELECT` 任何
//! `task_usage_dashboard_*`**。
//!
//! # 三个可复用面（**禁止重复实现**，`docs/62` §2.3）
//!
//! - `crates/mc-repos/src/runtime/usage.rs`（`task_usage_hourly` 的读写，322 行）—— **只读**；
//! - `crates/mc-repos/src/task/*`（`agent_task_queue` 的既有访问）；
//! - `crates/mc-http/src/routes/runtimes/usage.rs`（既有 per-runtime 口径 ——
//!   dashboard 的 `provider`/`model` 维度**故意**与它一致）。
//!
//! # 行形状与口径常量不在本文件
//!
//! 六个行结构 + `days`/`tz`/cutoff/折叠哨兵全在 [`mc_core::dashboard`]
//! （anchor 定形、此后冻结）⇒ M9-4 的 SQL 与用例读同一份类型。

use mc_db::Db;

use crate::RepoWithDb;

/// dashboard 的只读聚合（**M9-4 填充**）。
#[derive(Clone)]
pub struct DashboardRepo {
    db: Db,
}

impl DashboardRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for DashboardRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
