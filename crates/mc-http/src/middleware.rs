//! 默认 middleware 链。

use axum::Router;

/// 默认 middleware 链：trace + compression + cors + body-limit。
pub fn apply_default<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(tower_http::cors::CorsLayer::permissive())
        .layer(tower_http::limit::RequestBodyLimitLayer::new(25 * 1024 * 1024))
}