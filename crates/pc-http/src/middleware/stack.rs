//! 默认 middleware 装配。
use crate::AppState;
use axum::Router;
use axum::{middleware::from_fn, Extension};

use super::{
    access_log::access_log_layer,
    body_limit::body_limit_layer,
    compression::compression_layer,
    cors::{cors_layer, CorsConfig},
    private_hostname_guard::{private_hostname_guard_layer, PrivateHostnameGuardConfig},
    redaction::RedactionConfig,
    request_id::request_id_layer,
    trust_proxy::trust_proxy_layer,
};

/// 给定 router 套上默认 middleware 栈（与原 server middleware stack 等价）。
///
/// axum `.layer()` 后添加者在外层先执行，因此这里按执行顺序逆序添加：
///   configs -> request_id -> trust_proxy(client_ip) -> access_log
///   -> body_limit -> private_hostname_guard -> cors -> compression -> handler
/// auth_layer 不在此 stack 内：调用方需 Router<AppState> 后单独用
/// `axum::middleware::from_fn_with_state(state, auth_layer)` 注入。
pub fn apply_default_middleware(router: Router<AppState>) -> Router<AppState> {
    router
        .layer(from_fn(compression_layer))
        .layer(from_fn(cors_layer))
        .layer(from_fn(private_hostname_guard_layer))
        .layer(from_fn(body_limit_layer))
        .layer(from_fn(access_log_layer))
        .layer(from_fn(trust_proxy_layer))
        .layer(from_fn(request_id_layer))
        .layer(Extension(CorsConfig::from_environment()))
        .layer(Extension(PrivateHostnameGuardConfig::from_environment()))
}

/// 默认 RedactionConfig（从环境变量读取 placeholder / fields）。
pub fn default_redaction() -> RedactionConfig {
    let mut cfg = RedactionConfig::default();
    if let Ok(p) = std::env::var("PAPERCLIP_REDACTION_PLACEHOLDER") {
        cfg.placeholder = p;
    }
    cfg
}
