//! R372 SubprocessAcpRuntime.start_turn streaming tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use pc_acpx::{
    AcpRuntime, AcpRuntimeEnsureInput, AcpRuntimeEvent, AcpRuntimeMode, AcpRuntimeTurnInput,
    AcpRuntimeTurnResult, SubprocessAcpRuntime, SubprocessAcpRuntimeSpec,
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

#[tokio::test]
async fn start_turn_streams_text_delta_then_done() {
    // session/new response, then session/prompt request that produces a
    // text_delta notification followed by a completion response.
    let body = "\
#!/bin/sh
# 1. session/new handshake
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"backend_session_id\":\"backend-1\",\"agent_session_id\":\"agent-1\"}}'
# 2. session/prompt -> emit a text_delta notification, then respond with completed
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/event\",\"params\":{\"type\":\"text_delta\",\"text\":\"hello\",\"stream\":\"output\"}}'
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"stopReason\":\"end_turn\"}}'
while read line; do :; done
";
    let script = unique_temp_script("start-turn-stream", body);
    let runtime = runtime_for_script(&script);
    let handle = runtime
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "k".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            cwd: Some("/tmp".to_string()),
            ..Default::default()
        })
        .await
        .expect("ensure_session");

    let turn = runtime.start_turn(AcpRuntimeTurnInput {
        handle: handle.clone(),
        request_id: "req-1".into(),
        text: "hi".into(),
        mode: pc_acpx::AcpRuntimePromptMode::Prompt,
        ..Default::default()
    });

    let mut events: Vec<AcpRuntimeEvent> = Vec::new();
    let mut stream = turn.events;
    let collect_deadline = Duration::from_secs(3);
    let started = std::time::Instant::now();
    while started.elapsed() < collect_deadline {
        match tokio::time::timeout(Duration::from_millis(200), stream.next()).await {
            Ok(Some(event)) => events.push(event),
            _ => {
                if !events.is_empty() {
                    break;
                }
            }
        }
    }

    // We expect at least one text_delta event.
    let text_deltas: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AcpRuntimeEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !text_deltas.is_empty(),
        "expected at least one text_delta, got {events:?}"
    );
    assert_eq!(text_deltas[0], "hello");

    let result = turn.result.future.await;
    matches!(result, AcpRuntimeTurnResult::Completed { .. });

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

#[tokio::test]
async fn start_turn_done_event_terminates_result() {
    let body = "\
#!/bin/sh
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"backend_session_id\":\"backend-2\",\"agent_session_id\":\"agent-2\"}}'
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"stopReason\":\"end_turn\"}}'
while read line; do :; done
";
    let script = unique_temp_script("start-turn-done", body);
    let runtime = runtime_for_script(&script);
    let handle = runtime
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "k".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            cwd: Some("/tmp".to_string()),
            ..Default::default()
        })
        .await
        .expect("ensure_session");
    let turn = runtime.start_turn(AcpRuntimeTurnInput {
        handle: handle.clone(),
        request_id: "req-2".into(),
        text: "x".into(),
        mode: pc_acpx::AcpRuntimePromptMode::Prompt,
        ..Default::default()
    });
    let result = turn.result.future.await;
    if let AcpRuntimeTurnResult::Completed { stop_reason } = result {
        assert_eq!(stop_reason.as_deref(), Some("end_turn"));
    } else {
        panic!("expected Completed result, got {result:?}");
    }
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

#[tokio::test]
async fn start_turn_failed_response_maps_to_failed_result() {
    let body = "\
#!/bin/sh
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"backend_session_id\":\"backend-3\",\"agent_session_id\":\"agent-3\"}}'
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32603,\"message\":\"internal boom\"}}'
while read line; do :; done
";
    let script = unique_temp_script("start-turn-failed", body);
    let runtime = runtime_for_script(&script);
    let handle = runtime
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "k".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            cwd: Some("/tmp".to_string()),
            ..Default::default()
        })
        .await
        .expect("ensure_session");
    let turn = runtime.start_turn(AcpRuntimeTurnInput {
        handle: handle.clone(),
        request_id: "req-3".into(),
        text: "x".into(),
        mode: pc_acpx::AcpRuntimePromptMode::Prompt,
        ..Default::default()
    });
    let result = turn.result.future.await;
    if let AcpRuntimeTurnResult::Failed { error } = result {
        assert!(error.message.contains("internal boom"));
    } else {
        panic!("expected Failed result, got {result:?}");
    }
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

#[tokio::test]
async fn start_turn_streams_tool_call_event() {
    let body = "\
#!/bin/sh
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"backend_session_id\":\"backend-4\",\"agent_session_id\":\"agent-4\"}}'
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/event\",\"params\":{\"type\":\"tool_call\",\"text\":\"running bash\",\"toolCallId\":\"tool-1\",\"status\":\"in_progress\",\"title\":\"Bash\",\"kind\":\"exec\"}}'
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"stopReason\":\"end_turn\"}}'
while read line; do :; done
";
    let script = unique_temp_script("start-turn-tool", body);
    let runtime = runtime_for_script(&script);
    let handle = runtime
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "k".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            cwd: Some("/tmp".to_string()),
            ..Default::default()
        })
        .await
        .expect("ensure_session");
    let turn = runtime.start_turn(AcpRuntimeTurnInput {
        handle: handle.clone(),
        request_id: "req-4".into(),
        text: "x".into(),
        mode: pc_acpx::AcpRuntimePromptMode::Prompt,
        ..Default::default()
    });
    let mut events: Vec<AcpRuntimeEvent> = Vec::new();
    let mut stream = turn.events;
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        match tokio::time::timeout(Duration::from_millis(200), stream.next()).await {
            Ok(Some(event)) => {
                let is_tool = matches!(event, AcpRuntimeEvent::ToolCall { .. });
                events.push(event);
                if is_tool {
                    break;
                }
            }
            _ => {
                if events
                    .iter()
                    .any(|event| matches!(event, AcpRuntimeEvent::ToolCall { .. }))
                {
                    break;
                }
            }
        }
    }
    let tool_event = events
        .iter()
        .find(|event| matches!(event, AcpRuntimeEvent::ToolCall { .. }));
    assert!(
        tool_event.is_some(),
        "expected tool_call event, got {events:?}"
    );
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

#[tokio::test]
async fn start_turn_error_event_maps_to_failed_result() {
    let body = "\
#!/bin/sh
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"backend_session_id\":\"backend-5\",\"agent_session_id\":\"agent-5\"}}'
read line
printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/event\",\"params\":{\"type\":\"error\",\"message\":\"agent reported error\",\"code\":\"E_AGENT\"}}'
while read line; do :; done
";
    let script = unique_temp_script("start-turn-error", body);
    let runtime = runtime_for_script(&script);
    let handle = runtime
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "k".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            cwd: Some("/tmp".to_string()),
            ..Default::default()
        })
        .await
        .expect("ensure_session");
    let turn = runtime.start_turn(AcpRuntimeTurnInput {
        handle: handle.clone(),
        request_id: "req-5".into(),
        text: "x".into(),
        mode: pc_acpx::AcpRuntimePromptMode::Prompt,
        ..Default::default()
    });
    let mut events: Vec<AcpRuntimeEvent> = Vec::new();
    let mut stream = turn.events;
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        match tokio::time::timeout(Duration::from_millis(200), stream.next()).await {
            Ok(Some(event)) => events.push(event),
            _ => {
                if !events.is_empty() {
                    break;
                }
            }
        }
    }
    let has_error = events
        .iter()
        .any(|event| matches!(event, AcpRuntimeEvent::Error { .. }));
    assert!(has_error, "expected error event, got {events:?}");
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
