//! M5 anchor scaffold（LUM-1563）：`/api/webhooks*` 面路由切片聚合 —— **空 router 占位**。
//!
//! 归属：`docs/44-M5-PLAN.md` §1.1（#21 是本波**唯一**无认证入口路由）与 §3.2（写集矩阵）。
//! 本文件是公共聚合点（anchor 预建，此后**任何切片都不改本文件**）。
//!
//! | 子模块 | 路由 | 切片 |
//! | --- | --- | --- |
//! | [`autopilots`] | #21 `POST /api/webhooks/autopilots/:token` | M5-5 |
//!
//! `mount.rs::mount_slice_autopilot()` 已合并本文件的 `router()`，本文件再合并 1 个子 router
//! ⇒ M5-5 **只需实现 `autopilots.rs` 的 `router()`**，不必改 `mount.rs` / `routes/mod.rs`。
//!
//! ⚠️ 这是**无认证面**（R5）：切片实现时不要在路由层套 `require_workspace_member` 一类的中间件
//! ——token 才是唯一入口凭证；权限判定在 ingress 侧按 token 反查 trigger/workspace 之后做，
//! 且 workspace 作用域**绝不能**取自客户端提供的参数。
//!
//! 形态：`router.go:1487` 是 `r.Post("/api/webhooks/autopilots/{token}", …)` 的 **plain 路由**
//! ⇒ 只有**一个**形态，不要加尾斜杠别名（会被门 ⑦ 判 `EXTRA_ALIAS`）；路径参数写 `:token`
//! （matchit 0.7 把 `{token}` 当字面量段：编译过、恒 404）。

pub mod autopilots;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// webhook 面聚合切片：1 个子模块的 `router()` 在这里合并。
///
/// 子 router 仅声明路由表，不在内部 `with_state` —— 真正的 state 由
/// `apps/mc-server/src/main.rs` 在 `mc_http::routes::router().with_state(state)` 时一次性注入。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().merge(autopilots::router())
}
