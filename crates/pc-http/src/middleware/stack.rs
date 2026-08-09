//! 默认 middleware 装配。
use crate::AppState;
use axum::Router;
use axum::{middleware::from_fn, Extension};

use super::{
    access_log::access_log_layer, body_limit::body_limit_layer, cors::cors_layer, cors::CorsConfig,
    redaction::RedactionConfig, request_id::request_id_layer,
};

/// 给定 router 套上默认 middleware 栈（与原 server middleware stack 等价）。
///
/// 顺序：request_id -> access_log -> body_limit -> cors -> handler
/// auth_layer 不在此 stack 内：调用方需 Router<AppState> 后单独用
/// `axum::middleware::from_fn_with_state(state, auth_layer)` 注入。
pub fn apply_default_middleware(router: Router<AppState>) -> Router<AppState> {
    router
        .layer(from_fn(request_id_layer))
        .layer(from_fn(access_log_layer))
        .layer(from_fn(body_limit_layer))
        .layer(from_fn(cors_layer))
        .layer(Extension(CorsConfig::from_environment()))
}

/// 默认 RedactionConfig（从环境变量读取 placeholder / fields）。
pub fn default_redaction() -> RedactionConfig {
    let mut cfg = RedactionConfig::default();
    if let Ok(p) = std::env::var("PAPERCLIP_REDACTION_PLACEHOLDER") {
        cfg.placeholder = p;
    }
    cfg
}
