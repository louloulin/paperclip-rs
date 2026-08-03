//! 基础 CORS 头注入。
//!
//! 与原 `paperclip/server/src/middleware/cors.ts` 等价（简化版）。
//! 生产应替换为严格 allow-list；这里允许常见本地开发来源。

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue, Method},
    middleware::Next,
    response::Response,
};

/// 默认允许来源（dev + 本地）。
pub const DEFAULT_ALLOWED_ORIGINS: &[&str] = &[
    "http://127.0.0.1:5173",
    "http://localhost:5173",
    "http://127.0.0.1:3100",
    "http://localhost:3100",
    "tauri://localhost",
];

#[derive(Debug, Clone)]
pub struct CorsConfig {
    pub allowed_origins: Vec<String>,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: DEFAULT_ALLOWED_ORIGINS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }
}

pub async fn cors_layer(req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let cfg = req
        .extensions()
        .get::<CorsConfig>()
        .cloned()
        .unwrap_or_default();
    let mut response = next.run(req).await;
    if let Some(o) = origin {
        if cfg.allowed_origins.iter().any(|a| a == &o) {
            if let Ok(v) = HeaderValue::from_str(&o) {
                response
                    .headers_mut()
                    .insert(HeaderName::from_static("access-control-allow-origin"), v);
            }
        }
    }
    response
        .headers_mut()
        .entry(HeaderName::from_static("access-control-allow-methods"))
        .or_insert(HeaderValue::from_static(
            "GET,POST,PUT,PATCH,DELETE,OPTIONS",
        ));
    response
        .headers_mut()
        .entry(HeaderName::from_static("access-control-allow-headers"))
        .or_insert(HeaderValue::from_static(
            "content-type,authorization,x-request-id,x-csrf-token",
        ));
    response
        .headers_mut()
        .entry(HeaderName::from_static("access-control-allow-credentials"))
        .or_insert(HeaderValue::from_static("true"));
    response
        .headers_mut()
        .entry(HeaderName::from_static("access-control-max-age"))
        .or_insert(HeaderValue::from_static("600"));
    response
}

/// 直接响应 OPTIONS 预检（避免落到业务 handler）。
pub async fn handle_preflight(method: Method) -> Response {
    if method == Method::OPTIONS {
        let mut r = Response::new(axum::body::Body::empty());
        let h = r.headers_mut();
        h.insert(
            HeaderName::from_static("access-control-allow-origin"),
            HeaderValue::from_static("*"),
        );
        h.insert(
            HeaderName::from_static("access-control-allow-methods"),
            HeaderValue::from_static("GET,POST,PUT,PATCH,DELETE,OPTIONS"),
        );
        h.insert(
            HeaderName::from_static("access-control-allow-headers"),
            HeaderValue::from_static("content-type,authorization,x-request-id,x-csrf-token"),
        );
        return r;
    }
    // 实际不会到这里，axum 会先匹配具体路由
    Response::new(axum::body::Body::empty())
}

/// axum `from_fn` 适配器。
#[derive(Debug, Clone, Default)]
pub struct CorsLayer;

impl CorsLayer {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_origins_include_local_dev() {
        let cfg = CorsConfig::default();
        assert!(cfg
            .allowed_origins
            .iter()
            .any(|o| o == "http://127.0.0.1:5173"));
    }

    #[test]
    fn layer_constructs() {
        let _ = CorsLayer::new();
    }
}
