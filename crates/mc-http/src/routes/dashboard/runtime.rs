//! `/api/dashboard/{agent-runtime,runtime/daily}` 2 条（**写者 M9-4**）。
//!
//! ## anchor 期（本文件由 M9-0 `LUM-1815` 建桩，`docs/62` §5）
//!
//! **空** `Router::new()` ⇒ **零注册键**（本片 ⑦ 读数逐字不变：`local 474 / baseline 473 /
//! implemented 389 real + 3 placeholder / known_gap 64 / owners.M9 33 / local_only 8`）。
//! 🔴 **不得**在这里注册 501 占位：这 2 条是**上游键**，占位会让 ⑦ 把它们算成
//! `implemented_placeholder`（`owners.M9` 假清零），而 ⑨ 会从 `unmounted` 变 `mismatch`。
//!
//! 上游：`internal/handler/dashboard.go` 的 `GetDashboardAgentRunTime` / `GetDashboardRunTimeDaily`。
//! ⚠️ `agent-runtime` **没有**日期维度 ⇒ 用 `CutoffConvention::ExactDays`（恰好 N 天），
//! 而 `runtime/daily` 用 `HeadroomDay`（N+1 天）—— 两半口径在 `mc_core::dashboard` 里钉住。
//!
//! 形态（`docs/62` §1.4 实测 `declared 34 / dual-form required: 3`）：本波**只有**
//! `/api/notification-preferences` 那 3 条需要补尾斜杠形态；本文件的路由**只按上游字面量
//! 注册那一形态**。路径参数写 `:name`（matchit 0.7 把 `{name}` 当字面量 ⇒ 编译通过且恒 404）。
//!
//! 同 path+method 重复注册 ⇒ axum 在**启动时 panic**（`docs/15` §9.6.6）—— 这是本 anchor
//! 把合并点与子文件分开的唯一理由。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// runtime 两条切片（M9-4）：只读 `agent_task_queue` ⋈ `agent` ⋈ `issue`。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
