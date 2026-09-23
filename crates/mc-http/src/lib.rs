//! Multica HTTP layer：axum router + middleware + state。

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
