//! M5-0 anchor：autopilot **执行面**（手工触发 / runs 读面）—— **空 router 占位**。
//!
//! - **写者**：M5-4（`docs/44` §3.2）。切片只实现本文件的 `router()`；deliveries 三条在
//!   `../delivery.rs`（同片）。
//! - **路由**（`router.go` L2115–L2117）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 15 | POST | `/api/autopilots/:id/trigger` | `TriggerAutopilot` | 73 (+15) |
//! | 16 | GET | `/api/autopilots/:id/runs` | `ListAutopilotRuns` | 50 |
//! | 17 | GET | `/api/autopilots/:id/runs/:runId` | `GetAutopilotRun` | 44 |
//!
//! - **三条都是单形态** ⇒ 不要加尾斜杠别名。
//! - **#15 走与调度同一条派发路径**（`mc_autopilot::dispatch`），只是计划时间 = 现在 ⇒
//!   别在路由层另写一套「手工触发」逻辑，否则幂等/跳过（`shouldSkipDispatch`98）会分叉。
//! - **runs 读面**：`autopilot_run.status ∈ {issue_created, running, completed, failed, skipped}`
//!   （`043` + `079`）；列表用 `runToResponseSlim`88 的投影（`../dto.rs`）。
//! - **`planned_at`**（`124`）与 `quota_reservation_id`（**无外键**）是对账字段，读面要能暴露。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M5-4 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
