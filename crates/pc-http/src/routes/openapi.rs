//! `OpenAPI 3` 文档端点。
//!
//! 与 Node 上游 [`openapi.ts`] 的合约保持一致：
//! - `/openapi.json` 是规范入口（Node 上游用 `/api/openapi`）
//! - `/api/openapi` 与 `/api/openapi.json` 是 alias
//! - 生成的 paths 通过源码扫描 `crates/pc-http/src/routes/*.rs` 自动
//!   注入（避免逐路径手写 600+ 端点）
//!
//! 设计原则：
//! - 路径规范化：`/:param` → `/{param}`（OpenAPI 路径参数语法）
//! - 方法标签：每个路径生成 `{operationId, summary, description, tags}`
//! - 字段命名保持与 Node 上游 1:1（snake_case / camelCase 都接受）

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        // Canonical mount points used by the Rust server itself.
        .route("/openapi.json", get(document))
        .route("/api/openapi", get(document))
        // Alias matching the Node upstream contract (`/api/openapi.json`) so
        // parity tests and shared OpenAPI consumers can use one URL.
        .route("/api/openapi.json", get(document))
}

/// Convert a Rust `:param` style path to OpenAPI `{param}` style.
fn normalize_path(path: &str) -> String {
    // Replace `:foo` with `{foo}` per OpenAPI path templating.
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ':' {
            // Read identifier chars
            let mut ident = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_ascii_alphanumeric() || nc == '_' {
                    ident.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push('{');
            out.push_str(&ident);
            out.push('}');
        } else {
            out.push(c);
        }
    }
    out
}

/// Infer a stable operationId from method + path. Mirrors Node upstream
/// (snake_case verb_noun form).
fn operation_id(method: &str, path: &str) -> String {
    let normalized = path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .replace(|c: char| !c.is_ascii_alphanumeric(), "_")
        .replace("__", "_")
        .trim_matches('_')
        .to_string();
    if normalized.is_empty() {
        return format!("root_{}", method.to_lowercase());
    }
    format!("{}_{}", method.to_lowercase(), normalized)
}

/// Infer a tag from the first non-empty path segment after `/api/`.
fn infer_tag(path: &str) -> String {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.len() >= 2 && parts[0] == "api" && !parts[1].is_empty() {
        // Use the resource name as the tag, stripping trailing ':id' or 'me'.
        let raw = parts[1];
        // Take just the first segment if hyphenated (e.g. `company-skills` -> `company`)
        raw.split('-').next().unwrap_or(raw).to_string()
    } else if parts.is_empty() || parts[0].is_empty() {
        "root".to_string()
    } else {
        parts[0].to_string()
    }
}

async fn document(State(state): State<AppState>) -> impl IntoResponse {
    // Scan `crates/pc-http/src/routes/*.rs` at request time so adding a new
    // route module automatically extends the spec without code changes.
    let paths = scan_routes_for_openapi();
    let adapters = state
        .adapters
        .descriptors()
        .into_iter()
        .map(|d| d.adapter_type)
        .collect::<Vec<_>>();
    let body = json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Paperclip API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "REST API for the Paperclip AI agent management platform"
        },
        "servers": [{ "url": "/" }],
        "tags": build_tag_list(&paths),
        "paths": paths,
        "components": {
            "securitySchemes": {
                "session": { "type": "apiKey", "in": "cookie", "name": "paperclip_session" },
                "apiKey": { "type": "apiKey", "in": "header", "name": "X-Paperclip-Api-Key" }
            }
        },
        "x-paperclip": { "adapters": adapters }
    });
    (StatusCode::OK, Json(body))
}

/// Build a deduplicated tag list from the inferred tag of each path.
fn build_tag_list(paths: &BTreeMap<String, Value>) -> Vec<Value> {
    let mut tags: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for value in paths.values() {
        if let Some(obj) = value.as_object() {
            for (_method, op) in obj {
                if let Some(tag) = op
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                {
                    tags.insert(tag.to_string());
                }
            }
        }
    }
    tags.into_iter()
        .map(|name| json!({ "name": name }))
        .collect()
}

/// Strip Rust line comments (`//...`) and block comments (`/* ... */`)
/// so they are not scanned as if they were route declarations.
fn strip_rust_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Line comment
        if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // Block comment
        if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            continue;
        }
        // String literal — preserve verbatim
        if chars[i] == '\'' || chars[i] == '\"' {
            let q = chars[i];
            out.push(q);
            i += 1;
            while i < chars.len() && chars[i] != q {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    out.push(chars[i]);
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                out.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Scan all route files for `.route("/path", get|post|...)` (chained or not)
/// and produce a `{path: {method: op}}` map suitable for OpenAPI paths.
///
/// Mirrors the regex used in `scripts/diff-routes.sh` so the OpenAPI
/// document stays consistent with the diff metric.
fn scan_routes_for_openapi() -> BTreeMap<String, Value> {
    let mut paths: BTreeMap<String, Value> = BTreeMap::new();
    let routes_dir = locate_routes_dir();
    let Ok(entries) = std::fs::read_dir(&routes_dir) else {
        return paths;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let Ok(raw_src) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Drop comments so example paths inside doc comments don't leak
        // into the spec.
        let src = strip_rust_comments(&raw_src);
        for chunk in src.split(".route(").skip(1) {
            // Find the leading quoted path.
            let trimmed = chunk.trim_start();
            let path_start = trimmed.find(['\'', '\"']).unwrap_or(usize::MAX);
            if path_start == usize::MAX {
                continue;
            }
            let quote = trimmed.as_bytes()[path_start] as char;
            let after_quote = &trimmed[path_start + 1..];
            let path_end = after_quote.find(quote).unwrap_or(usize::MAX);
            if path_end == usize::MAX {
                continue;
            }
            let raw_path = &after_quote[..path_end];
            if !raw_path.starts_with('/') {
                continue;
            }
            // Restrict to the same `.route(...)` invocation: stop at the
            // first top-level close paren. Track paren depth starting from 1.
            let mut depth = 1i32;
            let mut tail = String::new();
            for ch in after_quote[path_end + 1..].chars() {
                if ch == '(' {
                    depth += 1;
                } else if ch == ')' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                tail.push(ch);
            }
            // Find chained methods in the tail.
            let mut verbs = std::collections::BTreeSet::new();
            // Also include the leading verb (after the comma).
            for token in tail.split(|c: char| c == '(' || c == ')' || c == ',' || c.is_whitespace())
            {
                if matches!(token, "get" | "post" | "put" | "patch" | "delete") {
                    verbs.insert(token.to_string());
                }
            }
            // Skip if the path doesn't look like a real route path
            if raw_path.is_empty() || raw_path.len() < 2 {
                continue;
            }
            let normalized_path = normalize_path(raw_path);
            let tag = infer_tag(raw_path);
            let entry = paths
                .entry(normalized_path.clone())
                .or_insert_with(|| json!({}));
            if let Some(obj) = entry.as_object_mut() {
                for verb in &verbs {
                    let method = verb.to_lowercase();
                    obj.insert(
                        method.clone(),
                        json!({
                            "operationId": operation_id(verb, &normalized_path),
                            "summary": format!("{} {}", verb.to_uppercase(), normalized_path),
                            "tags": [tag.clone()],
                            "responses": {
                                "200": { "description": "OK" },
                                "401": { "description": "Unauthorized" },
                                "404": { "description": "Not Found" }
                            }
                        }),
                    );
                }
            }
        }
    }
    paths
}

/// Locate the routes directory by trying relative paths from CARGO_MANIFEST_DIR.
fn locate_routes_dir() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is set at compile time to `crates/pc-http`.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let primary = std::path::Path::new(manifest_dir).join("src/routes");
    if primary.is_dir() {
        return primary;
    }
    // Fallback: scan upward for the `crates/pc-http/src/routes` directory.
    let mut cursor = std::path::Path::new(manifest_dir).to_path_buf();
    for _ in 0..6 {
        let candidate = cursor.join("crates/pc-http/src/routes");
        if candidate.is_dir() {
            return candidate;
        }
        if !cursor.pop() {
            break;
        }
    }
    primary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_converts_param_style() {
        assert_eq!(
            normalize_path("/api/companies/:company_id"),
            "/api/companies/{company_id}"
        );
        assert_eq!(
            normalize_path("/api/issues/:id/comments/:comment_id"),
            "/api/issues/{id}/comments/{comment_id}"
        );
        assert_eq!(normalize_path("/api/health"), "/api/health");
    }

    #[test]
    fn operation_id_is_stable() {
        assert_eq!(
            operation_id("GET", "/api/companies/{id}"),
            "get_api_companies_id"
        );
        assert_eq!(
            operation_id("POST", "/api/issues/{id}/comments"),
            "post_api_issues_id_comments"
        );
    }

    #[test]
    fn tag_inference_uses_first_resource() {
        assert_eq!(infer_tag("/api/companies"), "companies");
        assert_eq!(infer_tag("/api/companies/{id}/agents"), "companies");
        assert_eq!(infer_tag("/api/company-skills"), "company");
        assert_eq!(infer_tag("/health"), "health");
    }
}
