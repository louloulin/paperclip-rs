//! hook **job 粘合**（0 路由）：把 `plugin_hook_schedule` 的 schedule 触发器接到调度内核。
//!
//! - **写者**：M6-8（`docs/57` §3.2）。
//! - **本文件没有路由**：M6-8 的**唯一**那条注册键是
//!   `POST /api/plugin-bridge/v1/hooks/:key`（`router.go:1598`），按**路径前缀归位**落在
//!   `routes/plugin_bridge/hooks.rs`。`docs/57` §3.2 把它挂在 `routes/plugins/hooks_job.rs`
//!   名下；本 anchor 按前缀归位，注册键总数不变（57/57），差异已登记 `docs/32` §9。
//!   所以本文件保留为 **job 侧**的落点：把 hook 的定时触发注册成调度内核的一个 job。
//! - **内核复用（不要另起一套）**：调度骨架在 `mc_scheduler`（M5 已落地），租约/心跳/去重走
//!   `mc_repos::scheduler::sys_cron_executions`（M5-7 已落地）。M6 只需：
//!   1. 注册 job（`register_all` 的 plugin-hook 那一支 —— 上游 `db_ops.go` 与
//!      `mc_scheduler::jobs` 的既有形状）；
//!   2. 排下一轮：读 `plugin_hook_schedule`（`enabled` + `next_run_at`）算出本 tick 该跑的
//!      `(installation_id, hook_key, generation)`；
//!   3. 落 `plugin_invocation` 行（写侧在 `mc_repos::plugin::hook`，本文件只调用）。
//! - **⚠️ 跨波依赖（登记在案）**：`apps/mc-server/Cargo.toml` 缺 `mc-scheduler` 边（P0，归
//!   `LUM-1659`/M5-9 接线片）。**本 anchor 不动它** —— 本文件此刻不注册任何 job（无路由、
//!   不接 `AppState`），M6-8 落地时那一侧必须已经接好；否则按 `docs/57` §4.2 的降级口径
//!   只交桩级证据 + 登记缺口。
//! - **不做什么**：不改 `Cargo.lock` / `apps/mc-server`（本波由 anchor 一次性声明依赖）；
//!   不做 cron 解析（`mc_scheduler` 的 spec）。
//!
//! **状态：M6-8 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 300 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空的 router 切片（M6-8 不在此注册路由，保留合并点以便将来加管理面路由而不动 `mod.rs`）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
