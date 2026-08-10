//! HTTP 层：axum 路由 + middleware + 错误映射（与原 server 56 路由一一对应）。
pub mod auth;
pub mod hooks;
pub mod error;
pub mod middleware;
pub mod routes;
pub mod state;
pub use error::{ApiError, ApiResult};
pub use middleware::{
    redact_json, redact_text, AccessLogLayer, BodyLimitLayer, CorsConfig, CorsLayer,
    RedactionConfig, RequestId, RequestIdLayer, REQUEST_ID_HEADER,
};
pub use state::{require_user_id, AppState, ConfigSnapshot};
