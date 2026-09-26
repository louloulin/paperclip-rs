//! Multica HTTP layer：axum router + middleware + state。

// M9-0 anchor（`LUM-1815` / `docs/62-M9-PLAN.md` §2.1）：**机器凭据闸**（上游
// `RequireHumanActor` 的等价物）。它必须在**顶层**（与 `middleware` 同级）而不是
// 藏在某个路由目录里：挂载者是 M9-1/M9-2 的账户级路由组，而它自己不属于任何路由面。
pub mod actor_guard;
pub mod daemon_requests;
pub mod error;
pub mod middleware;
pub mod routes;
pub mod state;

pub use error::{ApiError, ApiResult};
pub use state::{AppState, ConfigSnapshot, RuntimeHandles};

use axum::Router;

/// 构造完整 axum router（state 在路由组装期注入，供 `from_fn_with_state`
/// 中间件与 handler 的 `State` 提取器共同使用）。
pub fn router(state: std::sync::Arc<AppState>) -> Router<std::sync::Arc<AppState>> {
    routes::router(state)
}

/// 默认 middleware 链：trace + compression + cors + body-limit。
pub fn apply_default_middleware<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    middleware::apply_default(router)
}
