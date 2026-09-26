//! dashboard 面聚合：**6 条**只读注册键分三个子文件（`docs/62-M9-PLAN.md` §4.1 的第 5 行）。
//!
//! ## ⚠️ 本文件由 M9-0 anchor（`LUM-1815`）冻结，M9 后续切片**不得**编辑
//!
//! | 文件 | 注册键 | 写者 | 上游 |
//! | --- | :-: | :-: | --- |
//! | `dashboard/usage.rs` | 2 | **M9-4** | `dashboard.go` L154 / L238 |
//! | `dashboard/runtime.rs` | 2 | **M9-4** | `dashboard.go` L370 / L461 |
//! | `dashboard/failures.rs` | 2 | **M9-4** | `dashboard.go` L532 / L579 |
//!
//! 账：2 + 2 + 2 = **6** ✓。三片同属一个写者（M9-4），分文件的理由不是并发而是**门 ⑩**
//! 与「一个文件一个面」（上游 655 行的三条 SQL 簇）。
//!
//! ## 两条只属于本面的纪律（`docs/62` §9.6 / §6.5 的 M9-4 行）
//!
//! 1. 🔴 **只读 `task_usage_hourly` / `agent_task_queue`**：6 条里 **0 条**读
//!    `task_usage_dashboard_*`（那两张 `084` legacy rollup 表已被 `101`/`103` 的 hourly 化
//!    取代）⇒ 用例要**断言不 `SELECT` 任何 `task_usage_dashboard_*`**；
//! 2. **3 条 per-agent 路由必须在服务端折叠私有 agent**（哨兵
//!    `mc_core::dashboard::RESTRICTED_AGENTS_ROW_ID`，且**合并不丢总额**）——
//!    上游注释逐字：「client-side filtering is decoration: one curl bypasses it」。
//!
//! ## 锚点期**零注册键**
//!
//! 三个子文件现在都是**空** `Router::new()` ⇒ `mount_slice_commercial()` 合并后
//! **注册键集合逐字不变**（`docs/62` §6.1 的 M9-0 行）。

pub mod failures;
pub mod runtime;
pub mod usage;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// dashboard 面的聚合 router（anchor 期 = 三次空 merge）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(usage::router())
        .merge(runtime::router())
        .merge(failures::router())
}
