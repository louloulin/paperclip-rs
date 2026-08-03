//! `X-Request-Id` 注入 + 透传。
//!
//! 与原 `paperclip/server/src/middleware/request-id.ts` 等价。
//! 缺失时生成 UUID v7，存在时透传。

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// RequestId extractor（axum handler 内可用）。
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

impl RequestId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// axum middleware factory。
pub async fn request_id_layer(mut req: Request, next: Next) -> Response {
    let header_name = HeaderName::from_static(REQUEST_ID_HEADER);
    let id = req
        .headers()
        .get(&header_name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    // 注入到 request headers 便于下游 handler 读取
    if let Ok(value) = HeaderValue::from_str(&id) {
        req.headers_mut().insert(header_name.clone(), value);
    }
    // 同时注入到 extensions，便于 handler 通过 `Extension<RequestId>` 获取
    req.extensions_mut().insert(RequestId(id.clone()));
    let mut response = next.run(req).await;
    // 回写到 response headers
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(header_name, value);
    }
    response
}

/// axum `from_fn` 适配器。
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestIdLayer;

impl RequestIdLayer {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_name_is_lowercase() {
        assert_eq!(REQUEST_ID_HEADER, "x-request-id");
    }

    #[test]
    fn request_id_display() {
        let r = RequestId("abc-123".into());
        assert_eq!(format!("{r}"), "abc-123");
    }
}
