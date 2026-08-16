// SPDX-License-Identifier: MIT
//
// UI-1: Contract test for the OpenAPI dump that powers the
// openapi-typescript -> TS client types workflow.
//
// Verifies:
//   - build_openapi_body_with_adapters produces a valid OpenAPI 3.1 document
//   - scan_routes_for_openapi picks up enough paths (>= 600)
//   - All paths have unique operationIds (R511 guardrail)
//   - The document can be serialized to a file at the well-known location
//     paperclip-rs/openapi.json for downstream openapi-typescript consumption.

use pc_http::routes::openapi::build_openapi_body_with_adapters;
use std::collections::HashSet;
use std::path::PathBuf;



#[test]
fn ui1_openapi_dump_has_top_level_keys() {
    let body = build_openapi_body_with_adapters(vec![
        String::from("codex-local"),
        String::from("claude-local"),
    ]);
    assert_eq!(body["openapi"], "3.1.0");
    assert_eq!(body["info"]["title"], "Paperclip API");
    assert!(body["servers"].is_array());
    assert!(body["tags"].is_array());
    assert!(body["paths"].is_object());
    assert!(body["components"]["securitySchemes"].is_object());
    assert!(body["components"]["schemas"].is_object());
    assert_eq!(body["x-paperclip"]["adapters"][0], "codex-local");
    assert_eq!(body["x-paperclip"]["adapters"][1], "claude-local");
}


#[test]
fn ui1_openapi_dump_path_count_meets_threshold() {
    let body = build_openapi_body_with_adapters(vec![]);
    let paths = body["paths"].as_object().expect("paths object");
    assert!(
        paths.len() >= 600,
        "expected >= 600 paths, found {}",
        paths.len()
    );
}

#[test]
fn ui1_openapi_dump_operation_ids_are_unique() {
    let body = build_openapi_body_with_adapters(vec![]);
    let mut seen: HashSet<String> = HashSet::new();
    let mut dups: Vec<String> = Vec::new();
    if let Some(paths) = body["paths"].as_object() {
        for (_path, methods) in paths {
            if let Some(methods) = methods.as_object() {
                for (method, op) in methods {
                    if let Some(op_id) = op.get("operationId").and_then(|v| v.as_str()) {
                        if !seen.insert(op_id.to_string()) {
                            dups.push(format!("{} {}", method, op_id));
                        }
                    }
                }
            }
        }
    }
    assert!(
        dups.is_empty(),
        "duplicate operationIds found: {:?}",
        dups
    );
}


#[test]
fn ui1_openapi_dump_writes_to_well_known_path() {
    // Only runs when the user sets PAPERCLIP_DUMP_OPENAPI=1 so normal CI
    // runs don't pollute the workspace.
    if std::env::var("PAPERCLIP_DUMP_OPENAPI").is_err() {
        return;
    }

    let body = build_openapi_body_with_adapters(vec![
        String::from("codex-local"),
        String::from("claude-local"),
        String::from("cursor"),
    ]);

    // Write to <repo-root>/openapi.json. The integration test runs with
    // CARGO_MANIFEST_DIR = crates/pc-http, so go two levels up.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .ancestors()
        .nth(2)
        .expect("repo root");
    let out_path = repo_root.join("openapi.json");

    let serialized = serde_json::to_string_pretty(&body).expect("serialize");
    std::fs::write(&out_path, serialized).expect("write openapi.json");

    let n = body["paths"].as_object().map(|p| p.len()).unwrap_or(0);
    println!("UI-1 wrote {} paths to {}", n, out_path.display());
}
