//! R371 SubprocessAcpRuntime tests — verify the JSON-RPC runtime impl
//! against fake `acpx` shell scripts that emit canned frames.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use pc_acpx::{
    AcpRuntime, AcpRuntimeCapabilities, AcpRuntimeControl, AcpRuntimeEnsureInput, AcpRuntimeEvent,
    AcpRuntimeGetCapabilitiesInput, AcpRuntimeGetStatusInput, AcpRuntimeMode,
    AcpRuntimeSetConfigOptionInput, AcpRuntimeSetModeInput, AcpRuntimeTurnInput,
    SubprocessAcpRuntime, SubprocessAcpRuntimeSpec,
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

fn runtime_for_script(script: &PathBuf) -> SubprocessAcpRuntime {
    let spec = SubprocessAcpRuntimeSpec {
        command: script.to_string_lossy().into_owned(),
        args: Vec::new(),
        cwd: None,
        env: HashMap::new(),
        response_timeout: Duration::from_secs(5),
    };
    SubprocessAcpRuntime::new(spec).expect("runtime")
}

// ============================================================================
// ensure_session tests
// ============================================================================

#[tokio::test]
async fn ensure_session_handshakes_with_session_new_response() {
    let body = "\
#!/bin/sh
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"backend_session_id\":\"backend-xyz\",\"agent_session_id\":\"agent-xyz\"}}'
while read line; do :; done
";
    let script = unique_temp_script("session-new", body);
    let runtime = runtime_for_script(&script);
    let handle = runtime
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "s1".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            cwd: Some("/tmp".to_string()),
            ..Default::default()
        })
        .await
        .expect("ensure_session");
    assert_eq!(handle.session_key, "s1");
    assert_eq!(handle.backend_session_id.as_deref(), Some("backend-xyz"));
    assert_eq!(handle.agent_session_id.as_deref(), Some("agent-xyz"));
    assert_eq!(handle.backend, "claude");
    runtime
        .close(pc_acpx::AcpRuntimeCloseInput {
            handle: handle.clone(),
            reason: "test cleanup".into(),
            discard_persistent_state: Some(false),
        })
        .await
        .expect("close");
    cleanup(&script.parent().unwrap().to_path_buf());
}

#[tokio::test]
async fn ensure_session_propagates_session_new_error() {
    let body = "\
#!/bin/sh
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32602,\"message\":\"session/new not allowed\"}}'
";
    let script = unique_temp_script("session-new-error", body);
    let runtime = runtime_for_script(&script);
    let err = runtime
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "s2".into(),
            agent: "codex".into(),
            mode: AcpRuntimeMode::OneShot,
            cwd: Some("/tmp".to_string()),
            ..Default::default()
        })
        .await
        .expect_err("session/new error must propagate");
    assert!(format!("{err}").contains("session/new not allowed"));
    cleanup(&script.parent().unwrap().to_path_buf());
}

// ============================================================================
// get_capabilities / get_status tests
// ============================================================================

#[tokio::test]
async fn get_capabilities_returns_advertised_controls() {
    let body = "\
#!/bin/sh
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"controls\":[\"set_mode\",\"status\"],\"config_option_keys\":[\"model\",\"effort\"]}}'
while read line; do :; done
";
    let script = unique_temp_script("capabilities", body);
    let runtime = runtime_for_script(&script);
    let caps = runtime
        .get_capabilities(AcpRuntimeGetCapabilitiesInput::default())
        .await
        .expect("caps");
    assert_eq!(
        caps.controls,
        vec![AcpRuntimeControl::SetMode, AcpRuntimeControl::Status]
    );
    assert_eq!(
        caps.config_option_keys.as_deref(),
        Some(&["model".to_string(), "effort".to_string()][..])
    );
    runtime
        .close(pc_acpx::AcpRuntimeCloseInput {
            handle: pc_acpx::AcpRuntimeHandle::default(),
            reason: "cleanup".into(),
            discard_persistent_state: None,
        })
        .await
        .ok();
    cleanup(&script.parent().unwrap().to_path_buf());
}

#[tokio::test]
async fn get_status_returns_session_handle_fields() {
    let body = "\
#!/bin/sh
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"summary\":\"idle\",\"agent_session_id\":\"agent-1\"}}'
while read line; do :; done
";
    let script = unique_temp_script("status", body);
    let runtime = runtime_for_script(&script);
    let handle = pc_acpx::AcpRuntimeHandle {
        session_key: "k".into(),
        backend: "claude".into(),
        agent_session_id: Some("agent-1".into()),
        ..Default::default()
    };
    let status = runtime
        .get_status(AcpRuntimeGetStatusInput {
            handle: handle.clone(),
        })
        .await
        .expect("status");
    assert_eq!(status.summary.as_deref(), Some("idle"));
    assert_eq!(status.agent_session_id.as_deref(), Some("agent-1"));
    runtime
        .close(pc_acpx::AcpRuntimeCloseInput {
            handle,
            reason: "cleanup".into(),
            discard_persistent_state: None,
        })
        .await
        .ok();
    cleanup(&script.parent().unwrap().to_path_buf());
}

// ============================================================================
// set_mode / set_config_option tests
// ============================================================================

#[tokio::test]
async fn set_mode_succeeds_with_session_set_mode_response() {
    let body = "\
#!/bin/sh
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}'
while read line; do :; done
";
    let script = unique_temp_script("set-mode", body);
    let runtime = runtime_for_script(&script);
    runtime
        .set_mode(AcpRuntimeSetModeInput {
            handle: pc_acpx::AcpRuntimeHandle::default(),
            mode: "persistent".to_string(),
        })
        .await
        .expect("set_mode");
    runtime
        .close(pc_acpx::AcpRuntimeCloseInput {
            handle: pc_acpx::AcpRuntimeHandle::default(),
            reason: "cleanup".into(),
            discard_persistent_state: None,
        })
        .await
        .ok();
    cleanup(&script.parent().unwrap().to_path_buf());
}

#[tokio::test]
async fn set_config_option_succeeds_with_ok_response() {
    let body = "\
#!/bin/sh
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}'
while read line; do :; done
";
    let script = unique_temp_script("set-config", body);
    let runtime = runtime_for_script(&script);
    runtime
        .set_config_option(AcpRuntimeSetConfigOptionInput {
            handle: pc_acpx::AcpRuntimeHandle::default(),
            key: "model".into(),
            value: "gpt-5".into(),
        })
        .await
        .expect("set_config_option");
    runtime
        .close(pc_acpx::AcpRuntimeCloseInput {
            handle: pc_acpx::AcpRuntimeHandle::default(),
            reason: "cleanup".into(),
            discard_persistent_state: None,
        })
        .await
        .ok();
    cleanup(&script.parent().unwrap().to_path_buf());
}

// ============================================================================
// cancel / close / doctor tests
// ============================================================================

#[tokio::test]
async fn cancel_kills_long_running_child() {
    let body = "\
#!/bin/sh
trap 'exit 0' TERM INT
sleep 30
";
    let script = unique_temp_script("hang", body);
    let runtime = runtime_for_script(&script);
    runtime
        .cancel(pc_acpx::AcpRuntimeCancelInput {
            handle: pc_acpx::AcpRuntimeHandle::default(),
            reason: Some("test".to_string()),
        })
        .await
        .expect("cancel");
    cleanup(&script.parent().unwrap().to_path_buf());
}

#[tokio::test]
async fn close_shuts_down_child() {
    let body = "\
#!/bin/sh
trap 'exit 0' TERM
while read line; do :; done
";
    let script = unique_temp_script("shutdown", body);
    let runtime = runtime_for_script(&script);
    runtime
        .close(pc_acpx::AcpRuntimeCloseInput {
            handle: pc_acpx::AcpRuntimeHandle::default(),
            reason: "test shutdown".into(),
            discard_persistent_state: None,
        })
        .await
        .expect("close");
    cleanup(&script.parent().unwrap().to_path_buf());
}

#[tokio::test]
async fn doctor_reports_ok_when_process_is_alive() {
    let body = "\
#!/bin/sh
while read line; do :; done
";
    let script = unique_temp_script("doctor", body);
    let runtime = runtime_for_script(&script);
    let report = runtime.doctor().await.expect("doctor report");
    assert!(report.ok, "doctor must report ok while child is alive");
    runtime
        .close(pc_acpx::AcpRuntimeCloseInput {
            handle: pc_acpx::AcpRuntimeHandle::default(),
            reason: "cleanup".into(),
            discard_persistent_state: None,
        })
        .await
        .ok();
    cleanup(&script.parent().unwrap().to_path_buf());
}

#[tokio::test]
async fn start_turn_placeholder_returns_empty_event_stream() {
    // R371 ships ensure_session + control methods only. start_turn is
    // scheduled for R372; for now it must produce a synthetic empty
    // stream so the trait contract is satisfied end-to-end.
    let body = "\
#!/bin/sh
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"backend_session_id\":\"backend-3\",\"agent_session_id\":\"agent-3\"}}'
while read line; do :; done
";
    let script = unique_temp_script("start-turn-placeholder", body);
    let runtime = runtime_for_script(&script);
    let handle = runtime
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "s3".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            cwd: Some("/tmp".to_string()),
            ..Default::default()
        })
        .await
        .expect("ensure_session");
    let turn = runtime.start_turn(AcpRuntimeTurnInput {
        handle: handle.clone(),
        request_id: "r1".into(),
        text: "hello".into(),
        mode: pc_acpx::AcpRuntimePromptMode::Prompt,
        ..Default::default()
    });
    use futures::StreamExt;
    let mut events: Vec<AcpRuntimeEvent> = Vec::new();
    let mut stream = turn.events;
    while let Some(event) = tokio::time::timeout(Duration::from_millis(50), stream.next())
        .await
        .ok()
        .flatten()
    {
        events.push(event);
    }
    assert!(events.is_empty());
    runtime
        .close(pc_acpx::AcpRuntimeCloseInput {
            handle,
            reason: "cleanup".into(),
            discard_persistent_state: None,
        })
        .await
        .ok();
    cleanup(&script.parent().unwrap().to_path_buf());
}

#[allow(dead_code)]
fn _ensure_caps_exported(_caps: &AcpRuntimeCapabilities) {}
