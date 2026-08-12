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
pub mod compression;
pub mod cors;
pub mod csrf;
pub mod http_log_policy;
pub mod private_hostname_guard;
pub mod redaction;
pub mod request_id;
pub mod stack;
pub mod trust_proxy;

pub use access_log::AccessLogLayer;
pub use auth::{auth_layer, require_auth, require_company_access};
pub use body_limit::BodyLimitLayer;
pub use compression::{compression_layer, SupportedEncoding, API_COMPRESSION_THRESHOLD_BYTES};
pub use cors::{CorsConfig, CorsLayer, DEFAULT_ALLOWED_ORIGINS};
pub use csrf::{
    csrf_decision, csrf_layer, csrf_set_cookie, generate_csrf_token, CsrfDenial, CSRF_COOKIE_NAME,
    CSRF_HEADER_NAME, CSRF_TOKEN_BYTES,
};
pub use http_log_policy::should_silence_http_success_log;
pub use private_hostname_guard::{
    blocked_hostname_message, extract_hostname, is_loopback_hostname, private_hostname_guard_layer,
    resolve_private_hostname_allow_set, should_enable_private_hostname_guard,
    PrivateHostnameGuardConfig,
};
pub use redaction::{redact_json, redact_text, RedactionConfig};
pub use request_id::{RequestId, RequestIdLayer, REQUEST_ID_HEADER};
pub use stack::{apply_default_middleware, default_redaction};
pub use trust_proxy::{
    parse_trust_proxy_env, resolve_client_ip, ClientIp, TrustProxyConfig, TrustProxyValue,
};
pub mod board_mutation_guard;
pub mod validate;
pub use board_mutation_guard::{
    board_mutation_guard_layer, is_trusted_board_mutation_request, parse_origin,
    trusted_origins_for_request,
};
pub use validate::{validate_body, zod_details};
