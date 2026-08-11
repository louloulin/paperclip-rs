//! R577 — OpenAPI UI client paths 集成验证。
//!
//! 验证 13 个 UI client 真实调用但 OpenAPI 之前未注册的路径，
//! 现在都有 path_schema_hint 条目。

#![allow(clippy::doc_markdown)]

use pc_http::routes::openapi::path_schema_hint;

const UI_PATHS: &[(&str, &str)] = &[
    ("/api/health", "GET"),
    ("/api/health/dev-server/restart", "GET"),
    ("/api/auth/get-session", "GET"),
    ("/api/auth/profile", "GET"),
    ("/api/adapters/{type}/ui-parser.js", "GET"),
    ("/api/assets/{asset_id}/content", "GET"),
    ("/api/companies/{company_id}/audit/agent-actions.csv", "GET"),
    ("/api/companies/{company_id}/events/ws", "GET"),
    ("/api/issues/{issue_id}/file-resources/content", "GET"),
    ("/api/v1/runs", "GET"),
    ("/api/plugins/{plugin_id}/actions/{key}", "GET"),
    ("/api/plugins/{plugin_id}/data/{key}", "GET"),
    ("/api/plugins/{plugin_id}/bridge/stream/{channel}", "GET"),
];

#[test]
fn r577_all_13_ui_paths_have_hints() {
    let mut found = 0;
    for (path, method) in UI_PATHS {
        assert!(
            path_schema_hint(path, method).is_some(),
            "missing hint for {method} {path}"
        );
        found += 1;
    }
    assert_eq!(found, 13);
}

#[test]
fn r577_hints_carry_response_schema_names() {
    // Verify that each hint has a non-None response schema (so the
    // OpenAPI registry emits a typed response block).
    for (path, method) in UI_PATHS {
        let hint = path_schema_hint(path, method).expect("hint");
        assert!(
            hint.response.is_some(),
            "hint for {method} {path} must have a response schema"
        );
    }
}

#[test]
fn r577_patch_endpoint_has_request_schema() {
    // PATCH /api/auth/profile needs a request schema (the request body).
    let hint = path_schema_hint("/api/auth/profile", "PATCH").expect("PATCH hint");
    assert!(hint.request.is_some(), "PATCH must have request schema");
}

#[test]
fn r577_unknown_method_returns_none() {
    // Even with hint registered for GET, an unknown method (e.g. POST)
    // for that path should return None.
    assert!(path_schema_hint("/api/health", "POST").is_none());
    assert!(path_schema_hint("/api/v1/runs", "POST").is_none());
    assert!(path_schema_hint("/api/auth/profile", "DELETE").is_none());
}
