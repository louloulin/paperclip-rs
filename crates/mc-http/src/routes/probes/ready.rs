//! `GET /healthz` + `GET /readyz` —— readiness 探针（**M10-2 原地填充**，`docs/64` §2.1 / §4.1 第 3 行）。
//!
//! 上游：`server/cmd/server/health.go::readyHandler` + `readiness()`（`f41fae6b08fb` L93-182）——
//! `db.Ping` + **所有** up 版本都已记入 `schema_migrations` ⇒ 200 或 **503**；
//! body `{status, checks:{db, migrations}}`，`migrations ∈ {ok, error, out_of_date, unknown}`。
//! **同一个 handler 挂两个路径**（不是两份实现），并复刻上游的 **3 秒缓存 + 单飞**。
//!
//! ## anchor 期（本文件由 M10-0 `LUM-2102` 建桩）
//!
//! **空** `Router::new()` ⇒ **零注册键**。🔴 **不得**在这里注册 501 占位（⑨ 里这两条路径
//! **没有** fixture，但 ⑦ 会立刻把占位算成 `implemented_placeholder` ⇒ `owners.M10` 假清零）。
//!
//! 形态：上游两条都是 plain `r.Get(...)` ⇒ 只注册**无尾斜杠**那一形态
//! （`docs/64` §1.4 实测 `dual-form required: 0`）。
//!
//! ⚠️ readiness 的"迁移齐否"判定**必须复用** `mc_migrate::verify()`（`Readiness{pending,
//! missing_tables}`），**不许**在路由里另写一份 `SELECT COUNT(*)`（`docs/64` §2.1 的取舍）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/healthz` + `/readyz` 切片（M10-2 在这里把同一个 handler 挂到两条路径上）。
pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
}
