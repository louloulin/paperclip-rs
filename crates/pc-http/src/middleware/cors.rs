//! 基础 CORS 头注入。
//!
//! 与原 `paperclip/server/src/middleware/cors.ts` 等价（简化版）。
//! 生产应替换为严格 allow-list；这里允许常见本地开发来源。

use axum::{
    extract::Request,
    http::{header, HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
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

impl CorsConfig {
    /// Builds the process-wide CORS policy from the development defaults and
    /// the explicit comma-separated allow-list supplied by the operator.
    pub fn from_environment() -> Self {
        let mut config = Self::default();
        if let Ok(origins) = std::env::var("PAPERCLIP_CORS_ALLOWED_ORIGINS") {
            for origin in origins
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if !config
                    .allowed_origins
                    .iter()
                    .any(|allowed| allowed == origin)
                {
                    config.allowed_origins.push(origin.to_owned());
                }
            }
        }
        config
    }

    fn allows(&self, origin: &str) -> bool {
        self.allowed_origins.iter().any(|allowed| allowed == origin)
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
        .unwrap_or_else(CorsConfig::from_environment);
    let is_preflight = req.method() == Method::OPTIONS;
    let mut response = if is_preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(req).await
    };
    if let Some(o) = origin {
        if cfg.allows(&o) {
            if let Ok(v) = HeaderValue::from_str(&o) {
                response
                    .headers_mut()
                    .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
            }
        }
    }
    response
        .headers_mut()
        .entry(header::ACCESS_CONTROL_ALLOW_METHODS)
        .or_insert(HeaderValue::from_static(
            "GET,POST,PUT,PATCH,DELETE,OPTIONS",
        ));
    response
        .headers_mut()
        .entry(header::ACCESS_CONTROL_ALLOW_HEADERS)
        .or_insert(HeaderValue::from_static(
            "content-type,authorization,x-request-id,x-csrf-token",
        ));
    response
        .headers_mut()
        .entry(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
        .or_insert(HeaderValue::from_static("true"));
    response
        .headers_mut()
        .entry(header::ACCESS_CONTROL_MAX_AGE)
        .or_insert(HeaderValue::from_static("600"));
    response
        .headers_mut()
        .entry(header::VARY)
        .or_insert(HeaderValue::from_static(
            "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
        ));
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
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        middleware::from_fn,
        routing::get,
        Router,
    };
    use tower::ServiceExt;

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

    #[tokio::test]
    async fn allowed_preflight_short_circuits_method_routing() {
        let app = Router::new()
            .route("/api/health", get(|| async { "ok" }))
            .layer(from_fn(cors_layer));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/api/health")
                    .header(header::ORIGIN, "http://localhost:5173")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .header(
                        header::ACCESS_CONTROL_REQUEST_HEADERS,
                        "content-type,x-request-id",
                    )
                    .body(Body::empty())
                    .expect("preflight request"),
            )
            .await
            .expect("preflight response");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("http://localhost:5173"))
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
            Some(&HeaderValue::from_static("true"))
        );
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("GET")));
    }

    #[tokio::test]
    async fn disallowed_origin_is_not_reflected() {
        let app = Router::new()
            .route("/api/health", get(|| async { "ok" }))
            .layer(from_fn(cors_layer));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .header(header::ORIGIN, "https://evil.example")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }
}
