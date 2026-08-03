//! 请求体大小限制。
//!
//! 与原 `paperclip/server/src/middleware/body-limits.ts` 等价。
//! 在 body 完全读入前通过 `Content-Length` 头快速拒绝。

use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

const DEFAULT_MAX_BODY_BYTES: u64 = 25 * 1024 * 1024; // 25 MiB

/// Body 限制层。
#[derive(Debug, Clone, Copy)]
pub struct BodyLimitLayer {
    pub max_bytes: u64,
}

impl Default for BodyLimitLayer {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }
}

impl BodyLimitLayer {
    #[must_use]
    pub const fn new(max_bytes: u64) -> Self {
        Self { max_bytes }
    }
}

pub async fn body_limit_layer(req: Request, next: Next) -> Response {
    let max = req
        .extensions()
        .get::<BodyLimitLayer>()
        .copied()
        .unwrap_or_default()
        .max_bytes;
    if let Some(len) = req
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
    {
        if len > max {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("request body too large: {len} > {max}"),
            )
                .into_response();
        }
    }
    next.run(req).await
}

/// 把 `BodyLimitLayer` 应用到 router 的便捷函数。
pub fn with_max_bytes(max_bytes: u64) -> BodyLimitLayer {
    BodyLimitLayer::new(max_bytes)
}

#[allow(dead_code)]
fn _check_body_marker(_: Body) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_25mib() {
        let l = BodyLimitLayer::default();
        assert_eq!(l.max_bytes, 25 * 1024 * 1024);
    }

    #[test]
    fn custom_limit() {
        let l = BodyLimitLayer::new(1024);
        assert_eq!(l.max_bytes, 1024);
    }
}
