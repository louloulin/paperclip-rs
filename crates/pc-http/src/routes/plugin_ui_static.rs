//! Plugin UI static asset delivery.
//!
//! Node 端契约 (server/src/routes/plugin-ui-static.ts):
//! - GET /api/_plugins/:pluginId/ui/*filePath
//! - pluginId 可为 DB UUID 或 plugin key
//! - 仅 status='ready' + manifest 声明 ui 的 plugin 提供 UI
//! - 路径遍历防护 (../, %2F 等)
//! - 内容哈希文件名 -> immutable/1y; 其他 -> must-revalidate + ETag
//! - entry 文件 -> 重定向到 /ui/plugins/<id>/<entry>
//!
//! R517: 修正 /api 前缀, 增加状态校验与路径遍历防护。

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use bytes::Bytes;
use uuid::Uuid;

use crate::AppState;
use pc_repos::plugin::PluginRepo;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/_plugins/:plugin_id/ui/*path", get(plugin_ui_static))
}

async fn plugin_ui_static(
    State(state): State<AppState>,
    Path((plugin_id, rel_path)): Path<(String, String)>,
    headers_in: HeaderMap,
) -> Response {
    if !is_safe_rel_path(&rel_path) {
        return (StatusCode::BAD_REQUEST, "invalid file path").into_response();
    }

    let plugin_uuid = Uuid::parse_str(&plugin_id).ok();
    let repo = PluginRepo::new(&state.db);
    let row = if let Some(pid) = plugin_uuid {
        repo.get_by_id(pid).await.ok().flatten()
    } else {
        repo.get_by_key(&plugin_id).await.ok().flatten()
    };
    let Some(row) = row else {
        return (StatusCode::NOT_FOUND, "plugin not found").into_response();
    };
    if row.status != "ready" {
        return (
            StatusCode::FORBIDDEN,
            format!("plugin UI not available (status: {})", row.status),
        )
            .into_response();
    }

    let ui_entrypoints = row
        .manifest_json
        .get("entrypoints")
        .and_then(|v| v.get("ui"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            row.manifest_json
                .get("ui")
                .and_then(|v| v.get("entry"))
                .and_then(|v| v.as_str())
        });
    if ui_entrypoints.is_none() {
        return (StatusCode::NOT_FOUND, "plugin does not declare a UI bundle").into_response();
    }

    let entry_name = row
        .manifest_json
        .get("ui")
        .and_then(|v| v.get("entry"))
        .and_then(|v| v.as_str())
        .unwrap_or("index.html");
    if rel_path.is_empty() || rel_path == entry_name || rel_path == "index.html" {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [(
                header::LOCATION,
                format!("/ui/plugins/{plugin_id}/{entry_name}"),
            )],
            Bytes::new(),
        )
            .into_response();
    }

    if let Some(package_path) = &row.package_path {
        let ui_root = std::path::Path::new(package_path);
        let candidate = ui_root.join(&rel_path);
        let canonical_root = match std::fs::canonicalize(ui_root) {
            Ok(p) => p,
            Err(_) => {
                return (StatusCode::NOT_FOUND, "plugin UI directory not found").into_response()
            }
        };
        let canonical = match std::fs::canonicalize(&candidate) {
            Ok(p) => p,
            Err(_) => return (StatusCode::NOT_FOUND, "asset not found").into_response(),
        };
        if !canonical.starts_with(&canonical_root) {
            return (StatusCode::FORBIDDEN, "path traversal blocked").into_response();
        }
        let bytes = match std::fs::read(&canonical) {
            Ok(b) => b,
            Err(_) => return (StatusCode::NOT_FOUND, "asset not found").into_response(),
        };
        let metadata = std::fs::metadata(&canonical).ok();
        return serve_file(bytes, &rel_path, metadata.as_ref(), &headers_in);
    }

    let provider = match state.storage.resolve("plugin-ui") {
        Ok(p) => p,
        Err(_) => {
            return (StatusCode::NOT_FOUND, "plugin UI storage not configured").into_response()
        }
    };
    let key = format!("{plugin_id}/{rel_path}");
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

fn is_safe_rel_path(p: &str) -> bool {
    if p.is_empty() {
        return false;
    }
    let p_lower = p.to_lowercase();
    if p.contains("..") || p.contains('\\') {
        return false;
    }
    if p_lower.contains("%2e") || p_lower.contains("%2f") || p_lower.contains("%5c") {
        return false;
    }
    if p.starts_with("//") {
        return false;
    }
    true
}

fn serve_file(
    bytes: Vec<u8>,
    rel_path: &str,
    metadata: Option<&std::fs::Metadata>,
    headers_in: &HeaderMap,
) -> Response {
    let content_type = guess_content_type(rel_path);
    let is_hashed = is_content_hashed(rel_path);
    let cache_control = if is_hashed {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=0, must-revalidate"
    };
    let etag = metadata.map(|m| {
        let mtime = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        compute_etag(m.len() as usize, mtime)
    });
    if let Some(etag_value) = &etag {
        if let Some(if_none_match) = headers_in.get(header::IF_NONE_MATCH) {
            if let Ok(v) = if_none_match.to_str() {
                if v == etag_value {
                    return (
                        StatusCode::NOT_MODIFIED,
                        [(header::ETAG, HeaderValue::from_str(etag_value).unwrap())],
                    )
                        .into_response();
                }
            }
        }
    }
    let mut resp = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache_control),
        ],
        Bytes::from(bytes),
    )
        .into_response();
    if let Some(etag_value) = etag {
        if let Ok(hv) = HeaderValue::from_str(&etag_value) {
            resp.headers_mut().insert(header::ETAG, hv);
        }
    }
    resp
}

fn is_content_hashed(name: &str) -> bool {
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.' || bytes[i] == b'-' {
            let mut j = i + 1;
            let hex_start = j;
            while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j - hex_start >= 8 {
                if j < bytes.len() && bytes[j] == b'.' {
                    let ext_start = j + 1;
                    if ext_start < bytes.len() {
                        let mut ok = true;
                        for k in ext_start..bytes.len() {
                            if bytes[k] == b'/' || bytes[k] == b'\\' {
                                ok = false;
                                break;
                            }
                        }
                        if ok {
                            return true;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    false
}

fn compute_etag(size: usize, mtime: u64) -> String {
    let combined = format!("v2:{}-{}", size, mtime);
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in combined.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("\"{:016x}\"", hash)
}

fn guess_content_type(path: &str) -> &'static str {
    if path.ends_with(".html") || path.ends_with(".htm") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") || path.ends_with(".mjs") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".json") {
        "application/json; charset=utf-8"
    } else if path.ends_with(".map") {
        "application/json; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg"
    } else if path.ends_with(".gif") {
        "image/gif"
    } else if path.ends_with(".webp") {
        "image/webp"
    } else if path.ends_with(".woff") {
        "font/woff"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else if path.ends_with(".ttf") {
        "font/ttf"
    } else if path.ends_with(".eot") {
        "application/vnd.ms-fontobject"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else if path.ends_with(".txt") {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}
