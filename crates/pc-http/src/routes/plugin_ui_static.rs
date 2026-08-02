//! 插件 UI 静态资源。

use axum::{extract::Path, http::StatusCode, response::IntoResponse, routing::get, Router};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/_plugins/:plugin_id/ui/*path", get(plugin_ui_static))
}

async fn plugin_ui_static(Path((_plugin_id, _path)): Path<(String, String)>) -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "plugin UI not found in this build")
}
