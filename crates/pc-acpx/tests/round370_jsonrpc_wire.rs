//! R370 JSON-RPC 2.0 wire + subprocess handle tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use pc_acpx::{
    decode_jsonrpc_frame, encode_jsonrpc_error, encode_jsonrpc_notification,
    encode_jsonrpc_request, encode_jsonrpc_response, jsonrpc_error_from_value, next_jsonrpc_id,
    parse_jsonrpc_line, AcpxError, JsonRpcErrorBody, JsonRpcFrame, JsonRpcIdAllocator,
    JsonRpcRequest, JsonRpcResponse, SpawnAcpxInput, SubprocessHandle, SubprocessTermination,
    JSONRPC_VERSION,
};

fn unique_temp_script(label: &str, body: &str) -> PathBuf {
    let pid = std::process::id();
    let uuid = uuid::Uuid::new_v4();
    let dir = std::env::temp_dir().join(format!("pc-acpx-{label}-{pid}-{uuid}"));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let script = dir.join("fake-acpx.sh");
    std::fs::write(&script, body).expect("write");
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    script
}

fn cleanup(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

// ============================================================================
// jsonrpc_wire tests
// ============================================================================

#[test]
fn jsonrpc_version_is_2_point_0() {
    assert_eq!(JSONRPC_VERSION, "2.0");
}

#[test]
fn next_jsonrpc_id_starts_at_one_and_monotonic() {
    let alloc = JsonRpcIdAllocator::new();
    assert_eq!(next_jsonrpc_id(&alloc), 1);
    assert_eq!(next_jsonrpc_id(&alloc), 2);
    assert_eq!(next_jsonrpc_id(&alloc), 3);
}

#[test]
fn encode_jsonrpc_request_emits_single_line_with_id() {
    let body = encode_jsonrpc_request(
        1,
        "session/prompt",
        Some(serde_json::json!({
            "sessionKey": "abc",
            "text": "hi",
        })),
    );
    assert!(!body.contains('\n'), "must be single line: {body}");
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["id"], 1);
    assert_eq!(parsed["method"], "session/prompt");
    assert_eq!(parsed["params"]["sessionKey"], "abc");
}

#[test]
fn encode_jsonrpc_request_with_no_params_omits_field() {
    let body = encode_jsonrpc_request(7, "session/status", None);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert!(parsed.get("params").is_none());
    assert_eq!(parsed["id"], 7);
}

#[test]
fn encode_jsonrpc_response_carries_result() {
    let body = encode_jsonrpc_response(3, &serde_json::json!({"ok": true}));
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["id"], 3);
    assert_eq!(parsed["result"]["ok"], true);
    assert!(parsed.get("error").is_none());
}

#[test]
fn encode_jsonrpc_error_carries_code_message_id() {
    let body = encode_jsonrpc_error(
        9,
        JsonRpcErrorBody {
            code: -32601,
            message: "Method not found".into(),
            data: Some(serde_json::json!({"method": "foo"})),
        },
    );
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["id"], 9);
    assert_eq!(parsed["error"]["code"], -32601);
    assert_eq!(parsed["error"]["message"], "Method not found");
    assert_eq!(parsed["error"]["data"]["method"], "foo");
}

#[test]
fn encode_jsonrpc_notification_omits_id() {
    let body = encode_jsonrpc_notification(
        "session/event",
        Some(serde_json::json!({
            "kind": "text_delta",
            "text": "hello",
        })),
    );
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert!(parsed.get("id").is_none());
    assert_eq!(parsed["method"], "session/event");
    assert_eq!(parsed["params"]["text"], "hello");
}

#[test]
fn parse_jsonrpc_line_routes_by_shape() {
    let req = parse_jsonrpc_line(
        r#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"x":1}}"#,
    )
    .expect("request");
    match req {
        pc_acpx::JsonRpcFrame::Request(r) => {
            assert_eq!(r.id, 1);
            assert_eq!(r.method, "session/prompt");
            assert_eq!(r.params.unwrap()["x"], 1);
        }
        other => panic!("expected request, got {other:?}"),
    }

    let resp =
        parse_jsonrpc_line(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).expect("response");
    match resp {
        pc_acpx::JsonRpcFrame::Response(r) => {
            assert_eq!(r.id, 1);
            assert_eq!(r.result.unwrap()["ok"], true);
        }
        other => panic!("expected response, got {other:?}"),
    }

    let err =
        parse_jsonrpc_line(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#)
            .expect("error");
    match err {
        pc_acpx::JsonRpcFrame::Error { id, error } => {
            assert_eq!(id, 1);
            assert_eq!(error.code, -32601);
            assert_eq!(error.message, "nope");
        }
        other => panic!("expected error, got {other:?}"),
    }

    let notif =
        parse_jsonrpc_line(r#"{"jsonrpc":"2.0","method":"session/event","params":{"x":1}}"#)
            .expect("notification");
    match notif {
        pc_acpx::JsonRpcFrame::Notification(n) => {
            assert_eq!(n.method, "session/event");
            assert!(n.id.is_none());
        }
        other => panic!("expected notification, got {other:?}"),
    }
}

#[test]
fn parse_jsonrpc_line_rejects_non_object() {
    let err = parse_jsonrpc_line("[]").expect_err("must reject array");
    assert!(matches!(err, AcpxError::JsonRpcParse { .. }));
}

#[test]
fn parse_jsonrpc_line_rejects_wrong_version() {
    let err =
        parse_jsonrpc_line(r#"{"jsonrpc":"1.0","id":1,"result":{}}"#).expect_err("must reject 1.0");
    assert!(format!("{err}").contains("jsonrpc"));
}

#[test]
fn decode_jsonrpc_frame_round_trips() {
    let body = encode_jsonrpc_request(42, "session/prompt", Some(serde_json::json!({"x": 1})));
    let frame = decode_jsonrpc_frame(&body).expect("frame");
    match frame {
        pc_acpx::JsonRpcFrame::Request(r) => {
            assert_eq!(r.id, 42);
            assert_eq!(r.method, "session/prompt");
        }
        other => panic!("expected request, got {other:?}"),
    }
}

#[test]
fn jsonrpc_error_from_value_extracts_code_and_message() {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {"code": -32600, "message": "Invalid request"}
    });
    let err = jsonrpc_error_from_value(&body).expect("err");
    assert_eq!(err.code, -32600);
    assert_eq!(err.message, "Invalid request");
    let missing =
        jsonrpc_error_from_value(&serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}}));
    assert!(missing.is_none());
}

#[test]
fn jsonrpc_id_allocator_handles_concurrency() {
    use std::sync::Arc;
    let alloc = Arc::new(JsonRpcIdAllocator::new());
    let mut handles = Vec::new();
    for _ in 0..8 {
        let a = Arc::clone(&alloc);
        handles.push(std::thread::spawn(move || {
            let mut local = Vec::new();
            for _ in 0..50 {
                local.push(next_jsonrpc_id(&a));
            }
            local
        }));
    }
    let mut all: Vec<u64> = Vec::new();
    for handle in handles {
        all.extend(handle.join().unwrap());
    }
    all.sort();
    let unique: std::collections::BTreeSet<u64> = all.iter().copied().collect();
    assert_eq!(unique.len(), all.len(), "ids must be unique");
    assert_eq!(unique.len(), 400);
}

#[test]
fn jsonrpc_request_serializes_back_to_value() {
    let req = JsonRpcRequest {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id: 5,
        method: "session/prompt".into(),
        params: Some(serde_json::json!({"k": "v"})),
    };
    let value = serde_json::to_value(&req).expect("value");
    assert_eq!(value["id"], 5);
    assert_eq!(value["method"], "session/prompt");
    let resp = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id: 5,
        result: Some(serde_json::json!({"ok": true})),
    };
    let value = serde_json::to_value(&resp).expect("value");
    assert_eq!(value["result"]["ok"], true);
}

// ============================================================================
// subprocess_handle tests
// ============================================================================

#[tokio::test]
async fn subprocess_handle_tracks_pid_and_exit_code() {
    // Script that exits 0 quickly.
    let script = unique_temp_script(
        "ok",
        "#!/bin/sh\necho '{\"jsonrpc\":\"2.0\",\"method\":\"ready\"}'\nexit 0\n",
    );
    let handle = SubprocessHandle::spawn(SpawnAcpxInput {
        command: script.to_string_lossy().into_owned(),
        args: Vec::new(),
        cwd: None,
        env: HashMap::new(),
        stdin_request_capacity: 4,
    })
    .await
    .expect("spawn");
    let pid = handle.pid();
    assert!(pid > 0, "pid must be positive");
    let outcome = handle.wait().await.expect("wait");
    assert!(matches!(outcome, SubprocessTermination::Exited(0)));
    cleanup(&script.parent().unwrap().to_path_buf());
}

#[tokio::test]
async fn subprocess_handle_reports_non_zero_exit_code() {
    let script = unique_temp_script(
        "nonzero",
        "#!/bin/sh\necho '{\"jsonrpc\":\"2.0\",\"method\":\"ready\"}'\nexit 7\n",
    );
    let handle = SubprocessHandle::spawn(SpawnAcpxInput {
        command: script.to_string_lossy().into_owned(),
        args: Vec::new(),
        cwd: None,
        env: HashMap::new(),
        stdin_request_capacity: 4,
    })
    .await
    .expect("spawn");
    let outcome = handle.wait().await.expect("wait");
    assert!(matches!(outcome, SubprocessTermination::Exited(7)));
    cleanup(&script.parent().unwrap().to_path_buf());
}

#[tokio::test]
async fn subprocess_handle_cancel_kills_long_running_child() {
    let script = unique_temp_script("hang", "#!/bin/sh\ntrap 'exit 0' TERM INT\nsleep 30\n");
    let handle = SubprocessHandle::spawn(SpawnAcpxInput {
        command: script.to_string_lossy().into_owned(),
        args: Vec::new(),
        cwd: None,
        env: HashMap::new(),
        stdin_request_capacity: 4,
    })
    .await
    .expect("spawn");
    let pid = handle.pid();
    handle.cancel().await.expect("cancel");
    let outcome = handle.wait().await.expect("wait");
    assert!(
        matches!(
            outcome,
            SubprocessTermination::Exited(_) | SubprocessTermination::Signalled { .. }
        ),
        "child should die after cancel, got {outcome:?}"
    );
    assert!(pid > 0);
    cleanup(&script.parent().unwrap().to_path_buf());
}

#[tokio::test]
async fn subprocess_handle_writes_request_to_stdin_and_reads_stdout_line() {
    let script = unique_temp_script(
        "echo",
        "#!/bin/sh\nread line\necho \"echo:$line\" >&2\necho '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}'\nexit 0\n",
    );
    let mut handle = SubprocessHandle::spawn(SpawnAcpxInput {
        command: script.to_string_lossy().into_owned(),
        args: Vec::new(),
        cwd: None,
        env: HashMap::new(),
        stdin_request_capacity: 4,
    })
    .await
    .expect("spawn");
    handle
        .write_request(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
        .await
        .expect("write");
    handle.close_stdin().await.expect("close stdin");
    let line = handle
        .read_response_line(Duration::from_secs(5))
        .await
        .expect("line");
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(parsed["id"], 1);
    assert_eq!(parsed["result"]["ok"], true);
    let outcome = handle.wait().await.expect("wait");
    assert!(matches!(outcome, SubprocessTermination::Exited(0)));
    cleanup(&script.parent().unwrap().to_path_buf());
}

#[tokio::test]
async fn subprocess_handle_spawn_fails_for_missing_command() {
    let err = SubprocessHandle::spawn(SpawnAcpxInput {
        command: "/nonexistent/path/to/binary".to_string(),
        args: Vec::new(),
        cwd: None,
        env: HashMap::new(),
        stdin_request_capacity: 4,
    })
    .await
    .expect_err("missing binary should error");
    assert!(matches!(err, AcpxError::Spawn { .. }));
}
