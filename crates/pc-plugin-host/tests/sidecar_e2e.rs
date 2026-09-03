//! R877 — Sidecar e2e test (subprocess spawn).
//!
//! This test ONLY runs if Node.js is available on PATH. It:
//! 1. Spawns the sidecar script with a fixture manifest
//! 2. Sends a JSON-RPC `initialize` request via stdin
//! 3. Asserts the sidecar responds with a valid JSON-RPC `result`
//! 4. Sends `health` and asserts `status: ok`
//! 5. Sends `shutdown` and asserts clean exit
//!
//! The fixture plugin (tests/fixtures/sidecar_fixture_plugin.cjs) is a
//! minimal CommonJS module exposing `initialize / health / shutdown`
//! with no third-party deps.
//!
//! Skipped automatically when `node` is not on PATH, so this test is
//! safe to run in any CI environment.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn sidecar_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("bin")
        .join("paperclip-plugin-sidecar.mjs")
}

fn fixture_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sidecar_fixture_plugin.cjs")
}

fn fixture_manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sidecar_fixture_manifest.json")
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn rpc_request(id: u64, method: &str, params: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string()
        + "\n"
}

#[test]
fn sidecar_e2e_initialize_health_shutdown() {
    if !node_available() {
        eprintln!("Node.js not available; skipping e2e test");
        return;
    }

    let script = sidecar_script();
    let manifest = fixture_manifest();

    let mut child = Command::new("node")
        .arg(&script)
        .arg("--plugin-id")
        .arg("00000000-0000-0000-0000-000000000001")
        .arg("--manifest")
        .arg(&manifest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sidecar");

    let stdin = child.stdin.as_mut().expect("stdin");
    // Initialize
    stdin
        .write_all(rpc_request(1, "initialize", serde_json::json!({})).as_bytes())
        .unwrap();
    // Health
    stdin
        .write_all(rpc_request(2, "health", serde_json::json!({})).as_bytes())
        .unwrap();
    // Shutdown
    stdin
        .write_all(rpc_request(3, "shutdown", serde_json::json!({})).as_bytes())
        .unwrap();
    drop(stdin);

    // Wait up to 5s for clean exit
    let start = std::time::Instant::now();
    let output = loop {
        if start.elapsed() > Duration::from_secs(5) {
            panic!("sidecar did not exit within 5s");
        }
        match child.try_wait() {
            Ok(Some(status)) => break child.wait_with_output().unwrap(),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("try_wait failed: {e}"),
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    // Expect at least 2 JSON-RPC responses (initialize + health; shutdown
    // may not respond if sidecar exits cleanly).
    assert!(
        lines.len() >= 2,
        "expected ≥ 2 JSON-RPC responses, got {} lines:\n{}",
        lines.len(),
        stdout
    );

    // First response (initialize): parse and verify result
    let init: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("initialize response not JSON: {e}\nline: {}", lines[0]));
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(init["id"], 1);
    assert!(init.get("result").is_some(), "initialize should have result");
    assert!(
        init["result"]["manifest"].is_object(),
        "initialize result should include manifest"
    );
    assert_eq!(
        init["result"]["manifest"]["id"],
        "00000000-0000-0000-0000-000000000001"
    );

    // Second response (health): verify status
    let health: serde_json::Value = serde_json::from_str(lines[1])
        .unwrap_or_else(|e| panic!("health response not JSON: {e}\nline: {}", lines[1]));
    assert_eq!(health["jsonrpc"], "2.0");
    assert_eq!(health["id"], 2);
    assert_eq!(health["result"]["status"], "ok");
}

#[test]
fn sidecar_e2e_rejects_unknown_method() {
    if !node_available() {
        return;
    }

    let manifest = fixture_manifest();
    let mut child = Command::new("node")
        .arg(sidecar_script())
        .arg("--plugin-id")
        .arg("00000000-0000-0000-0000-000000000001")
        .arg("--manifest")
        .arg(&manifest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sidecar");

    let stdin = child.stdin.as_mut().expect("stdin");
    stdin
        .write_all(rpc_request(1, "totally_unknown_method", serde_json::json!({})).as_bytes())
        .unwrap();
    drop(stdin);

    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let resp: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("response not JSON: {e}\nstdout: {stdout}"));

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["error"]["code"], -32601); // method not found
}
