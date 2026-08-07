//! R365 集成测试 — `pc-acpx` acp.handshake 协议契约验证。
//!
//! 覆盖：MockAcpRuntime + AcpRuntime trait + 事件流 + 状态查询端到端。

use pc_acpx::acp_runtime::{
    AcpRuntime, AcpRuntimeCapabilities, AcpRuntimeControl, AcpRuntimeEnsureInput, AcpRuntimeEvent,
    AcpRuntimeGetCapabilitiesInput, AcpRuntimeGetStatusInput, AcpRuntimeMode, AcpRuntimePromptMode,
    AcpRuntimeStream, AcpRuntimeTurnInput, MockAcpRuntime,
};
use serde_json::json;

#[tokio::test]
async fn mock_runtime_supports_full_session_lifecycle() {
    let mock = MockAcpRuntime::new(vec![
        AcpRuntimeEvent::TextDelta {
            text: "first".into(),
            stream: Some(AcpRuntimeStream::Output),
            tag: None,
        },
        AcpRuntimeEvent::TextDelta {
            text: "second".into(),
            stream: Some(AcpRuntimeStream::Output),
            tag: None,
        },
        AcpRuntimeEvent::Done {
            stop_reason: Some("end_turn".into()),
        },
    ]);

    // 1. Ensure session — gets a fresh handle.
    let handle = mock
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "session-1".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            ..Default::default()
        })
        .await
        .expect("ensure_session");
    assert_eq!(handle.session_key, "session-1");
    assert_eq!(handle.backend, "claude");
    assert!(handle.runtime_session_name.is_some());

    // 2. Run turn — collects all events.
    let events = mock
        .run_turn(AcpRuntimeTurnInput {
            handle: handle.clone(),
            text: "hello".into(),
            mode: AcpRuntimePromptMode::Prompt,
            request_id: "req-1".into(),
            ..Default::default()
        })
        .await;
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], AcpRuntimeEvent::TextDelta { .. }));
    assert!(matches!(events[2], AcpRuntimeEvent::Done { .. }));

    // 3. Get status — reads handle metadata.
    let status = mock
        .get_status(AcpRuntimeGetStatusInput {
            handle: handle.clone(),
        })
        .await
        .expect("status");
    assert_eq!(status.backend_session_id, handle.backend_session_id);
    assert_eq!(status.agent_session_id, handle.agent_session_id);
}

#[tokio::test]
async fn mock_runtime_advertises_capabilities() {
    let mock = MockAcpRuntime::new(vec![]).with_capabilities(AcpRuntimeCapabilities {
        controls: vec![
            AcpRuntimeControl::SetMode,
            AcpRuntimeControl::SetConfigOption,
            AcpRuntimeControl::Status,
        ],
        config_option_keys: Some(vec!["model".into(), "permissionMode".into()]),
    });
    let caps = mock
        .get_capabilities(AcpRuntimeGetCapabilitiesInput::default())
        .await
        .expect("caps");
    assert_eq!(caps.controls.len(), 3);
    assert_eq!(caps.config_option_keys.as_ref().unwrap().len(), 2);
}

#[tokio::test]
async fn event_round_trips_through_serde() {
    let event = AcpRuntimeEvent::TextDelta {
        text: "hello".into(),
        stream: Some(AcpRuntimeStream::Output),
        tag: None,
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], json!("text_delta"));
    assert_eq!(value["text"], json!("hello"));
    let round_trip: AcpRuntimeEvent = serde_json::from_value(value).unwrap();
    assert_eq!(round_trip, event);
}

#[tokio::test]
async fn status_event_carries_breakdown_and_cost() {
    let event = AcpRuntimeEvent::Status {
        text: "100ms token".into(),
        tag: None,
        used: Some(100),
        size: Some(200),
        cost: Some(pc_acpx::acp_runtime::AcpRuntimeUsageCost {
            amount: Some(0.01),
            currency: Some("USD".into()),
        }),
        breakdown: Some(pc_acpx::acp_runtime::AcpRuntimeUsageBreakdown {
            input_tokens: Some(100),
            output_tokens: Some(50),
            cached_read_tokens: Some(0),
            cached_write_tokens: Some(0),
            thought_tokens: None,
            total_tokens: Some(150),
        }),
        available_commands: None,
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], json!("status"));
    assert_eq!(value["used"], json!(100));
    assert_eq!(value["cost"]["amount"], json!(0.01));
    assert_eq!(value["breakdown"]["input_tokens"], json!(100));
}
