//! M28 — 4 个 missing 路由契约测试（Rust 端新增 + 静态契约）：
//! - `GET /api/_plugins/:plugin_id/ui/*file_path` (pc-plugin-ui-static 真实文件 + ETag + cache)
//! - `GET /api/companies/:company_id/search/extract` (alias + handler 存在契约)
//! - `POST /api/cases/:case_id/links` (alias 路由注册契约)
//! - `POST /api/dev-server/restart` (真实文件 IO + 三种 status code)

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pc_dev_server_status::{
    read_persisted_status, restart_required as dev_restart_required, write_restart_request,
    DevServerRestartRequest,
};
use pc_plugin_ui_static::{
    cache_control_for, compute_etag, is_content_hashed_name, mime_for_extension,
    safe_resolve_within, PluginUiError,
};
use serde_json::Value;
use std::path::PathBuf;
use tower::ServiceExt;

// ===== M28.1 plugin UI static 真实文件服务 =====

#[tokio::test]
async fn r528_plugin_ui_static_serves_real_file_with_etag() {
    use axum::Router;
    use axum::routing::get;
    use std::sync::Arc;

    // 真实文件 + ETag 头
    let tmp = tempfile::tempdir().expect("tempdir");
    let ui_dir = tmp.path().join("dist/ui");
    std::fs::create_dir_all(&ui_dir).unwrap();
    let f = ui_dir.join("index-abc12345.js");
    std::fs::write(&f, b"console.log('test')").unwrap();
    let meta = std::fs::metadata(&f).unwrap();
    let size = meta.len();
    let mtime_ms = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let expected_etag = compute_etag(size, mtime_ms);

    // 用 pc-plugin-ui-static 直接做 HTTP 适配层的子集验证：
    // 文件存在 + safe_resolve + ETag 计算
    let resolved = safe_resolve_within(&ui_dir, "index-abc12345.js").expect("resolve");
    let bytes = std::fs::read(&resolved).unwrap();
    assert_eq!(bytes, b"console.log('test')");
    assert_eq!(compute_etag(bytes.len() as u64, mtime_ms), expected_etag);
    // content-hashed → immutable
    assert_eq!(
        cache_control_for("index-abc12345.js"),
        pc_plugin_ui_static::CACHE_CONTROL_IMMUTABLE
    );
}

#[test]
fn r528_plugin_ui_static_mime_table() {
    assert!(mime_for_extension("js").starts_with("application/javascript"));
    assert!(mime_for_extension("woff2").starts_with("font/woff2"));
    assert_eq!(mime_for_extension("xyz"), "application/octet-stream");
}

#[test]
fn r528_plugin_ui_static_content_hash_detection() {
    assert!(is_content_hashed_name("index-abcdef01.js"));
    assert!(is_content_hashed_name("chunk.deadbeef.mjs"));
    assert!(!is_content_hashed_name("index.js"));
    assert!(!is_content_hashed_name("app.css"));
}

#[test]
fn r528_plugin_ui_static_safe_resolve_rejects_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let ui = tmp.path().join("ui");
    std::fs::create_dir_all(&ui).unwrap();
    std::fs::write(ui.join("ok.js"), b"x").unwrap();
    let sibling = tmp.path().join("escape.js");
    std::fs::write(&sibling, b"evil").unwrap();
    let r = safe_resolve_within(&ui, "../escape.js");
    assert!(
        matches!(r, Err(PluginUiError::PathTraversal(_))),
        "must reject ../ escape"
    );
}

#[test]
fn r528_plugin_ui_static_safe_resolve_rejects_protocol_override() {
    let tmp = tempfile::tempdir().unwrap();
    let ui = tmp.path().join("ui");
    std::fs::create_dir_all(&ui).unwrap();
    let r = safe_resolve_within(&ui, "https://evil.com/x");
    assert!(matches!(r, Err(PluginUiError::InvalidPath(_))));
}

// ===== M28.2 cases/:id/links alias 路由静态契约 =====

#[test]
fn r528_cases_links_alias_route_registered() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/routes/cases.rs"),
    )
    .expect("read cases.rs");
    assert!(
        src.contains("\"/api/cases/:case_id/links\""),
        "M28: POST /api/cases/:case_id/links alias must be registered"
    );
}

// ===== M28.3 search/extract 静态契约（已有 R516,Rust 端已 register） =====

#[test]
fn r528_companies_search_extract_route_registered() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/routes/companies.rs"),
    )
    .expect("read companies.rs");
    assert!(
        src.contains("\"/api/companies/:company_id/search/extract\""),
        "M28: search/extract must be registered"
    );
}

// ===== M28.4 dev-server/restart 真实 IO 契约 =====

#[test]
fn r528_dev_server_restart_writes_request_file() {
    let tmp = tempfile::tempdir().unwrap();
    let status_file = tmp.path().join("status.json");
    let env_file = status_file.to_string_lossy().to_string();
    let request = DevServerRestartRequest::manual_restart_now();
    let ok = write_restart_request(&request, Some(&env_file)).expect("write");
    assert!(ok);
    let expected = tmp.path().join("dev-server-restart-request.json");
    assert!(expected.exists(), "restart-request.json must be written next to status.json");
    let body = std::fs::read_to_string(&expected).unwrap();
    assert!(body.contains("manual_restart_now"));
    assert!(body.contains(&request.requested_at));
}

#[test]
fn r528_dev_server_restart_returns_false_when_env_unset() {
    let req = DevServerRestartRequest::manual_restart_now();
    let ok = write_restart_request(&req, None).expect("write");
    assert!(!ok, "no env → no file → false");
}

#[test]
fn r528_dev_server_restart_required_logic() {
    let s1 = pc_dev_server_status::PersistedDevServerStatus {
        dirty: false,
        changed_path_count: 0,
        pending_migrations: vec![],
        ..Default::default()
    };
    assert!(!dev_restart_required(&s1));
    let s2 = pc_dev_server_status::PersistedDevServerStatus {
        dirty: true,
        ..Default::default()
    };
    assert!(dev_restart_required(&s2));
}

#[test]
fn r528_dev_server_status_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let status_file = tmp.path().join("status.json");
    let env_file = status_file.to_string_lossy().to_string();
    // 写 status JSON
    let json = serde_json::json!({
        "dirty": true,
        "changedPathCount": 2,
        "changedPathsSample": ["a.rs", "b.rs"],
        "pendingMigrations": ["m1.sql"]
    });
    std::fs::write(&status_file, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    let status = read_persisted_status(Some(&env_file)).expect("read").expect("present");
    assert!(status.dirty);
    assert_eq!(status.changed_path_count, 2);
    assert_eq!(status.pending_migrations, vec!["m1.sql"]);
    assert!(dev_restart_required(&status));
}

#[test]
fn r528_dev_server_status_missing_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("not-exist.json");
    let env_file = missing.to_string_lossy().to_string();
    let s = read_persisted_status(Some(&env_file)).expect("read");
    assert!(s.is_none());
}

#[test]
fn r528_dev_server_restart_route_static_contract() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/routes/dev_server_restart.rs"),
    )
    .expect("read route file");
    assert!(
        src.contains("\"/api/dev-server/restart\""),
        "POST /api/dev-server/restart must be registered"
    );
    assert!(
        src.contains("dev_server_supervisor_unavailable"),
        "must return 404 with this code"
    );
    assert!(
        src.contains("restart_not_required"),
        "must return 409 with this code"
    );
    assert!(
        src.contains("restart_requested"),
        "must return 202 with this status"
    );
    // 必须先调 read_persisted_status 检查,再调 write_restart_request
    let read_idx = src.find("read_persisted_status").expect("read call");
    let write_idx = src.find("write_restart_request").expect("write call");
    assert!(
        read_idx < write_idx,
        "M28: must read status before writing restart request"
    );
}
