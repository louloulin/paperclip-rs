//! HTTP middleware stack。
//!
//! 与原 `paperclip/server/src/middleware/*` 等价：
//! - `request_id`：每个请求注入 / 透传 `X-Request-Id`
//! - `body_limit`：拒绝超过 N 字节的 body
//! - `access_log`：结构化访问日志（含 request_id / method / path / status / duration_ms）
//! - `redaction`：从日志中移除敏感字段（`password` / `secret` / `token` / `key`）
//! - `cors`：基础 CORS 头注入（生产应替换为严格 allow-list）

pub mod access_log;
pub mod auth;
pub mod body_limit;
pub mod cors;
pub mod csrf;
pub mod redaction;
pub mod request_id;
pub mod stack;

pub use access_log::AccessLogLayer;
pub use auth::{auth_layer, require_auth, require_company_access};
pub use body_limit::BodyLimitLayer;
pub use cors::{CorsConfig, CorsLayer, DEFAULT_ALLOWED_ORIGINS};
pub use redaction::{redact_json, redact_text, RedactionConfig};
pub use request_id::{RequestId, RequestIdLayer, REQUEST_ID_HEADER};
pub use csrf::{
    csrf_decision, csrf_layer, csrf_set_cookie, generate_csrf_token, CsrfDenial,
    CSRF_COOKIE_NAME, CSRF_HEADER_NAME, CSRF_TOKEN_BYTES,
};
pub use stack::{apply_default_middleware, default_redaction};
