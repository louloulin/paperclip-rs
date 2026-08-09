//! R400 — integration tests for `command_managed_runtime` +
//! `sandbox_callback_bridge` (port of Node
//! `command-managed-runtime.ts` and `sandbox-callback-bridge.ts` from
//! `paperclip/packages/adapter-utils/src/`).
//!
//! These tests verify the cross-module flow: a sync-in operation
//! declared by an adapter passes the host-side confinement guard,
//! produces the expected shell fragments, and a companion request to
//! the sandbox callback bridge would be authorized under the
//! default route allowlist while a non-allowlisted request would be
//! rejected.

use pc_acpx::command_managed_runtime::{
    assert_post_upload_commands_confined, build_sync_in_chmod_command,
    build_sync_in_extract_directory_command, build_sync_in_rename_command,
    build_unique_staging_path, PostUploadCommand, SandboxFileMapping, SandboxSyncOperation,
};
use pc_acpx::sandbox_callback_bridge::{
    authorize_sandbox_callback_bridge_request_with_routes, build_sandbox_callback_bridge_env,
    create_sandbox_callback_bridge_token, default_sandbox_callback_bridge_route_allowlist,
    sandbox_callback_bridge_directories, sanitize_sandbox_callback_bridge_headers, BridgeEnvInput,
};
use std::collections::BTreeMap;

fn mapping(target: &str) -> SandboxFileMapping {
    SandboxFileMapping {
        source_path: "host/path".to_string(),
        target_path: target.to_string(),
        mode: 0o644,
    }
}

fn op_with(cwd: Option<&str>, files: Vec<SandboxFileMapping>) -> SandboxSyncOperation {
    SandboxSyncOperation {
        files,
        post_upload_commands: cwd
            .map(|c| {
                vec![PostUploadCommand {
                    cwd: Some(c.to_string()),
                    command: "ls -la".to_string(),
                }]
            })
            .unwrap_or_default(),
    }
}

#[test]
fn happy_path_full_sync_in_flow() {
    let tar_target = "/workspace/extract";
    let file_target = "/workspace/file.txt";

    let ops = vec![SandboxSyncOperation {
        files: vec![mapping(tar_target), mapping(file_target)],
        post_upload_commands: vec![PostUploadCommand {
            cwd: Some(tar_target.to_string()),
            command: "make test".to_string(),
        }],
    }];

    assert_post_upload_commands_confined(&ops).expect("confined sync-in must pass");

    let extract_cmd =
        build_sync_in_extract_directory_command(tar_target, "/tmp/staging/extract.tar");
    assert!(extract_cmd.contains("tar"));
    assert!(extract_cmd.contains(tar_target));

    let chmod_cmd = build_sync_in_chmod_command(0o600, file_target);
    assert!(chmod_cmd.contains("chmod"));
    assert!(chmod_cmd.contains("600"));

    let rename_cmd = build_sync_in_rename_command("/tmp/staging/old", file_target);
    assert!(rename_cmd.contains("mv"));

    let unique = build_unique_staging_path(file_target, "staging");
    assert!(unique.contains(file_target));
    assert!(unique.contains("staging"));
    assert_ne!(unique, build_unique_staging_path(file_target, "staging"));
}

#[test]
fn confinement_rejects_cwd_outside_target_root() {
    let ops = vec![op_with(
        Some("/workspace/other"),
        vec![mapping("/workspace/target")],
    )];
    let err = assert_post_upload_commands_confined(&ops).unwrap_err();
    assert!(
        err.contains("escapes the operation"),
        "unexpected message: {err}"
    );
}

#[test]
fn confinement_accepts_no_post_upload_commands() {
    let ops = vec![SandboxSyncOperation {
        files: vec![mapping("/workspace/target")],
        post_upload_commands: vec![],
    }];
    assert!(assert_post_upload_commands_confined(&ops).is_ok());
}

#[test]
fn confinement_rejects_relative_cwd() {
    let ops = vec![op_with(
        Some("relative/path"),
        vec![mapping("/workspace/target")],
    )];
    let err = assert_post_upload_commands_confined(&ops).unwrap_err();
    assert!(
        err.contains("not a confined absolute POSIX path"),
        "unexpected message: {err}"
    );
}

#[test]
fn confinement_rejects_dotdot_cwd_with_specific_message() {
    let ops = vec![op_with(
        Some("/workspace/../escape"),
        vec![mapping("/workspace/target")],
    )];
    let err = assert_post_upload_commands_confined(&ops).unwrap_err();
    assert!(
        err.contains("not a confined absolute POSIX path"),
        "unexpected message: {err}"
    );
}

#[test]
fn unique_staging_paths_are_unique_under_concurrent_invocations() {
    let n = 50;
    let paths: Vec<String> = (0..n)
        .map(|_| build_unique_staging_path("/workspace/same", "stag"))
        .collect();
    let mut sorted = paths.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        n,
        "expected {n} unique paths, got {}",
        sorted.len()
    );
}

#[test]
fn shell_quoting_does_not_break_on_spaces_or_special_chars() {
    let cmd = build_sync_in_extract_directory_command(
        "/workspace/with space/dir",
        "/tmp/staging/withe space.tar",
    );
    assert!(cmd.contains("'/workspace/with space/dir'"));
    assert!(cmd.contains("'/tmp/staging/withe space.tar'"));
}

#[test]
fn bridge_token_is_random_and_unique() {
    let t1 = create_sandbox_callback_bridge_token(None);
    let t2 = create_sandbox_callback_bridge_token(None);
    let t3 = create_sandbox_callback_bridge_token(Some(16));
    assert_ne!(t1, t2, "two default tokens must differ");
    assert_eq!(t1.len(), 32);
    assert_eq!(t3.len(), 22);
    assert!(!t1.contains('+'));
    assert!(!t1.contains('/'));
    assert!(!t3.contains('+'));
    assert!(!t3.contains('/'));
}

#[test]
fn directories_compute_correct_layout() {
    let d = sandbox_callback_bridge_directories("/bridge/root");
    assert_eq!(d.root_dir, "/bridge/root");
    assert_eq!(d.requests_dir, "/bridge/root/requests");
    assert_eq!(d.responses_dir, "/bridge/root/responses");
    assert_eq!(d.logs_dir, "/bridge/root/logs");
    assert_eq!(d.ready_file, "/bridge/root/ready.json");
    assert_eq!(d.pid_file, "/bridge/root/server.pid");
    assert_eq!(d.log_file, "/bridge/root/logs/bridge.log");
}

#[test]
fn default_route_allowlist_includes_agents_endpoint() {
    let routes = default_sandbox_callback_bridge_route_allowlist();
    assert!(authorize_sandbox_callback_bridge_request_with_routes(
        "GET",
        "/api/agents/me",
        Some(&routes)
    )
    .is_ok());
    assert!(authorize_sandbox_callback_bridge_request_with_routes(
        "GET",
        "/api/agents/abc",
        Some(&routes),
    )
    .is_ok());
    // POST /api/issues/abc/checkout is in the default allowlist
    assert!(authorize_sandbox_callback_bridge_request_with_routes(
        "POST",
        "/api/issues/abc/checkout",
        Some(&routes),
    )
    .is_ok());
}

#[test]
fn default_route_allowlist_rejects_arbitrary_path() {
    let err = authorize_sandbox_callback_bridge_request_with_routes("GET", "/etc/passwd", None)
        .unwrap_err();
    assert!(err.contains("Route not allowed"));
}

#[test]
fn default_route_allowlist_rejects_wrong_method() {
    let err =
        authorize_sandbox_callback_bridge_request_with_routes("DELETE", "/api/agents/abc", None)
            .unwrap_err();
    assert!(err.contains("Route not allowed"));
}

#[test]
fn default_route_allowlist_method_case_normalized() {
    assert!(
        authorize_sandbox_callback_bridge_request_with_routes("get", "/api/agents/me", None)
            .is_ok()
    );
}

#[test]
fn custom_routes_can_replace_defaults() {
    let custom = vec![
        pc_acpx::sandbox_callback_bridge::SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/custom/endpoint$",
        ),
    ];
    assert!(authorize_sandbox_callback_bridge_request_with_routes(
        "POST",
        "/custom/endpoint",
        Some(&custom),
    )
    .is_ok());
    assert!(authorize_sandbox_callback_bridge_request_with_routes(
        "GET",
        "/api/agents/me",
        Some(&custom),
    )
    .is_err());
}

#[test]
fn sanitize_headers_preserves_allowed_keys() {
    let mut headers = BTreeMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("X-Custom".to_string(), "should-be-removed".to_string());
    headers.insert("Accept".to_string(), "*/*".to_string());
    headers.insert("If-Match".to_string(), "etag".to_string());

    let out = sanitize_sandbox_callback_bridge_headers(&headers, None);

    assert_eq!(
        out.get("Content-Type").map(String::as_str),
        Some("application/json")
    );
    assert_eq!(out.get("Accept").map(String::as_str), Some("*/*"));
    assert_eq!(out.get("If-Match").map(String::as_str), Some("etag"));
    assert!(!out.contains_key("X-Custom"));
}

#[test]
fn sanitize_headers_with_custom_allowlist() {
    let mut headers = BTreeMap::new();
    headers.insert("Content-Type".to_string(), "v".to_string());
    headers.insert("X-Special".to_string(), "v".to_string());

    let out =
        sanitize_sandbox_callback_bridge_headers(&headers, Some(&["content-type", "x-special"]));

    assert!(out.contains_key("Content-Type"));
    assert!(out.contains_key("X-Special"));
    assert_eq!(out.len(), 2);
}

#[test]
fn bridge_env_uses_documented_defaults() {
    let input = BridgeEnvInput {
        queue_dir: "/q".to_string(),
        bridge_token: "tok".to_string(),
        host: None,
        port: None,
        poll_interval_ms: None,
        response_timeout_ms: None,
        max_queue_depth: None,
        max_body_bytes: None,
    };
    let env = build_sandbox_callback_bridge_env(&input);

    assert!(env.contains_key("PAPERCLIP_BRIDGE_QUEUE_DIR"));
    assert!(env.contains_key("PAPERCLIP_BRIDGE_TOKEN"));
    let host = env.get("PAPERCLIP_BRIDGE_HOST").map(String::as_str);
    assert!(host.is_some(), "host must default when None");
    let port = env.get("PAPERCLIP_BRIDGE_PORT").map(String::as_str);
    assert!(port.is_some(), "port must default when None");
}

#[test]
fn bridge_env_uses_overrides_for_tuning() {
    let input = BridgeEnvInput {
        queue_dir: "/q".to_string(),
        bridge_token: "tok".to_string(),
        host: Some("0.0.0.0".to_string()),
        port: Some(9123),
        poll_interval_ms: Some(250),
        response_timeout_ms: Some(15_000),
        max_queue_depth: Some(8),
        max_body_bytes: Some(1024),
    };
    let env = build_sandbox_callback_bridge_env(&input);

    assert_eq!(
        env.get("PAPERCLIP_BRIDGE_HOST").map(String::as_str),
        Some("0.0.0.0")
    );
    assert_eq!(
        env.get("PAPERCLIP_BRIDGE_PORT").map(String::as_str),
        Some("9123")
    );
    assert_eq!(
        env.get("PAPERCLIP_BRIDGE_POLL_INTERVAL_MS")
            .map(String::as_str),
        Some("250")
    );
    assert_eq!(
        env.get("PAPERCLIP_BRIDGE_RESPONSE_TIMEOUT_MS")
            .map(String::as_str),
        Some("15000")
    );
    assert_eq!(
        env.get("PAPERCLIP_BRIDGE_MAX_QUEUE_DEPTH")
            .map(String::as_str),
        Some("8")
    );
    assert_eq!(
        env.get("PAPERCLIP_BRIDGE_MAX_BODY_BYTES")
            .map(String::as_str),
        Some("1024")
    );
}

#[test]
fn cross_module_smoke_confined_sync_in_with_authorized_bridge_ping() {
    let target = "/workspace/project";
    let sync_op = SandboxSyncOperation {
        files: vec![mapping(target)],
        post_upload_commands: vec![PostUploadCommand {
            cwd: Some(target.to_string()),
            command: "echo synced".to_string(),
        }],
    };
    assert_post_upload_commands_confined(&[sync_op]).unwrap();

    let token = create_sandbox_callback_bridge_token(None);
    assert!(!token.is_empty());
    assert!(
        authorize_sandbox_callback_bridge_request_with_routes("GET", "/api/agents/me", None)
            .is_ok()
    );

    let env_input = BridgeEnvInput {
        queue_dir: "/bridge/queue".to_string(),
        bridge_token: token,
        host: Some("127.0.0.1".to_string()),
        port: Some(7777),
        poll_interval_ms: None,
        response_timeout_ms: None,
        max_queue_depth: None,
        max_body_bytes: None,
    };
    let env = build_sandbox_callback_bridge_env(&env_input);
    assert_eq!(
        env.get("PAPERCLIP_BRIDGE_QUEUE_DIR").map(String::as_str),
        Some("/bridge/queue")
    );
    assert_eq!(
        env.get("PAPERCLIP_BRIDGE_HOST").map(String::as_str),
        Some("127.0.0.1")
    );
    assert_eq!(
        env.get("PAPERCLIP_BRIDGE_PORT").map(String::as_str),
        Some("7777")
    );
}
