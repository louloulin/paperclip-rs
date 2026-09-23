//! M5-0 anchor：autopilot **读面**（列表 / 单体 / cron-preview / usage）—— **空 router 占位**。
//!
//! - **写者**：M5-1（`docs/44` §3.2）。`mount.rs::mount_slice_autopilot()` →
//!   `autopilots::router()` 已接好，切片只需实现本文件的 `router()`，**不改** `mount.rs` /
//!   `routes/mod.rs` / `autopilots/mod.rs`。
//! - **路由**（`router.go` L2101–L2104 与 L2123）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 1 | GET | `/api/autopilots/`（+ 无斜杠别名） | `ListAutopilots` | 84 |
//! | 2 | GET | `/api/autopilots/cron-preview` | `CronPreview` | 31 (+8) |
//! | 3 | GET | `/api/autopilots/usage` | `GetAutopilotQuotaUsage` | 33 |
//! | 4 | GET | `/api/autopilots/:id/`（+ 无斜杠别名） | `GetAutopilot` | 73 |
//!
//! - **#1/#4 是双形态**（chi `Mount`，见 `autopilots/mod.rs` 的形态纪律）；#2/#3 单形态。
//! - **列表的三个派生列**（`docs/44` §4.2 M5-1）：`trigger_kinds` / `next_run_at` /
//!   `last_run_status` —— 上游是**三条子查询**，本地要么照抄三条 SELECT，要么一次 JOIN
//!   （性能取舍要在 PR 里写清；契约以上游 JSON 为准，`../dto.rs`）。
//! - **`can_write` 是 `Option<bool>`**（不带 caller 时省略），见 `../dto.rs`。
//! - **权限**：读面用 `../access.rs` 的可见性判定；`usage` 走 `mc_autopilot::quota`
//!   （限额来自 entitlement 平面，没有商业默认值可抄）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M5-1 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
