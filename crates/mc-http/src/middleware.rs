//! 默认 middleware 链。

use axum::Router;

// mc-server 从 `mc_http::middleware::` 路径引用默认链；此处 re-export 保持兼容。
pub use crate::apply_default_middleware;

pub mod authn;

/// 默认 middleware 链：trace + compression + cors + body-limit。
pub fn apply_default<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(tower_http::cors::CorsLayer::permissive())
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            25 * 1024 * 1024,
        ))
}
