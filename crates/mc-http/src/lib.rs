//! Multica HTTP layer：axum router + middleware + state。

pub mod error;
pub mod middleware;
pub mod routes;
pub mod state;

pub use error::{ApiError, ApiResult};
pub use state::{AppState, ConfigSnapshot, RuntimeHandles};

use axum::Router;
use std::sync::Arc;

/// 构造完整 axum router。
pub fn router() -> Router<Arc<AppState>> {
    routes::router()
}

/// 默认 middleware 链：trace + compression + cors + body-limit。
pub fn apply_default_middleware<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    middleware::apply_default(router)
}