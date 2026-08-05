//! Plugin UI static asset delivery.
//!
//! Plugin bundles may carry a `ui/` directory of static files; this route
//! resolves `/<plugin-id>/ui/<path>` to the underlying provider storage
//! object. Plugins opt-in to UI by declaring `ui.entry` and `ui.assets` in
//! the manifest; missing or unauthorized plugins return 404.

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use bytes::Bytes;
use uuid::Uuid;

use crate::AppState;
use pc_repos::plugin::PluginRepo;

pub fn router() -> Router<AppState> {
    Router::new().route("/_plugins/:plugin_id/ui/*path", get(plugin_ui_static))
}

async fn plugin_ui_static(
    State(state): State<AppState>,
    Path((plugin_id, rel_path)): Path<(String, String)>,
) -> impl IntoResponse {
    // Resolve plugin metadata to discover its UI bucket + prefix.
    let plugin_uuid = Uuid::parse_str(&plugin_id).ok();
    let ui_block: Option<serde_json::Value> = if let Some(pid) = plugin_uuid {
        PluginRepo::new(&state.db)
            .get_by_id(pid)
            .await
            .ok()
            .flatten()
            .map(|row| row.manifest_json.get("ui").cloned().unwrap_or(serde_json::json!({})))
    } else {
        None
    };

    let Some(ui) = ui_block else {
        return (StatusCode::NOT_FOUND, "plugin UI not found").into_response();
    };
    let entry = ui
        .get("entry")
        .and_then(|v| v.as_str())
        .unwrap_or("index.html");
    let prefix = ui
        .get("assetsPrefix")
        .and_then(|v| v.as_str())
        .unwrap_or("ui/");

    // If the request is for the entry point, redirect to the plugin's UI URL
    // rendered by the Vite-backed `/ui/plugins/<id>` route. For arbitrary
    // static assets, look them up in the configured object storage provider.
    if rel_path == entry || rel_path == "index.html" {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, format!("/ui/plugins/{plugin_id}/{entry}"))],
            Bytes::new(),
        )
            .into_response();
    }

    let provider = match state.storage.resolve("plugin-ui") {
        Ok(p) => p,
        Err(_) => {
            return (StatusCode::NOT_FOUND, "plugin UI storage not configured").into_response()
        }
    };
    let key = format!("{plugin_id}/{prefix}{rel_path}");
    let target = pc_storage::StorageLocation {
        bucket: "plugin-ui".into(),
        key: pc_storage::ObjectKey::new(key),
    };
    match provider.get_object(&target).await {
        Ok(bytes) => {
            let content_type = guess_content_type(&rel_path);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, content_type)],
                bytes,
            )
                .into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "asset not found").into_response(),
    }
}

fn guess_content_type(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".json") {
        "application/json; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else {
        "application/octet-stream"
    }
}
