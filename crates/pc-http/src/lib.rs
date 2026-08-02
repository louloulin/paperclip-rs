//! HTTP 层：axum 路由 + middleware + 错误映射（与原 server 56 路由一一对应）。
pub mod auth;
pub mod error;
pub mod routes;
pub mod state;
pub use error::{ApiError, ApiResult};
pub use state::{require_user_id, AppState, ConfigSnapshot};
