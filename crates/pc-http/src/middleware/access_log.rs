//! 结构化访问日志。
//!
//! 与原 `paperclip/server/src/middleware/logger.ts` + `http-log-policy.ts` 等价。
//! 字段：request_id / client_ip / method / path / status / duration_ms。
//! 高频轮询端点与静态资源按 http-log-policy 静默。

use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use std::time::Instant;
use tracing::info;

use super::http_log_policy::should_silence_http_success_log;
use super::request_id::RequestId;
use super::trust_proxy::ClientIp;

pub async fn access_log_layer(req: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = req.method().clone();
    let path = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| req.uri().path().to_owned());
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|r| r.0.clone())
        .unwrap_or_default();
    let client_ip = req
        .extensions()
        .get::<ClientIp>()
        .map(|c| c.0.clone())
        .unwrap_or_default();
    let response = next.run(req).await;
    let status = response.status().as_u16();
    let duration_ms = start.elapsed().as_millis() as i64;
    if !should_silence_http_success_log(Some(method.as_str()), Some(path.as_str()), status) {
        info!(
            request_id = %request_id,
            client_ip = %client_ip,
            method = %method,
            path = %path,
            status = status,
            duration_ms = duration_ms,
            "http access"
        );
    }
    response
}

/// axum `from_fn` 包装。
#[derive(Debug, Clone, Copy, Default)]
pub struct AccessLogLayer;

impl AccessLogLayer {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_constructs() {
        let _ = AccessLogLayer::new();
    }
}
