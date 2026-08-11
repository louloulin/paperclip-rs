//! 真实 WebSocket echo server 集成测试 —— `TungsteniteWireClient` 端到端验证。
//!
//! 启动本地 WS server，模拟 OpenClaw Gateway 的 device.connect / device.run.send
//! 协议，然后让 `TungsteniteWireClient` 连接并验证：
//! - Connect 握手（带超时）
//! - device.run.send 请求 / 响应关联
//! - Server-pushed event stream（stream.chunk / run.complete）
//! - Graceful disconnect
//!
//! 关键：`ws_e2e` 用真实 TCP socket + 真实 WS 帧，而非 in-memory mock。
//! 这是 R616 的核心验证 —— adapter 从"mockable only"升级到"生产可用"。

#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, WebSocketStream};

use pc_adapter_openclaw_gateway::credentials::{DeviceIdentitySource, GatewayDeviceIdentity};
use pc_adapter_openclaw_gateway::frame_codec::GatewayEventFrame;
use pc_adapter_openclaw_gateway::wire_client::{
    build_event, build_ok_response, make_connect_options, GatewayError, GatewayHello,
    GatewayWireClient,
};
use pc_adapter_openclaw_gateway::ws_client::TungsteniteWireClient;

type ServerStream = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Spawn a local WS server that handles `device.connect` and echoes events.
async fn spawn_echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _peer) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => break,
            };
            tokio::spawn(handle_connection(stream));
        }
    });
    addr
}

async fn handle_connection(stream: tokio::net::TcpStream) {
    let ws_stream = match accept_async(stream).await {
        Ok(s) => s,
        Err(_) => return,
    };
    let (mut tx, mut rx) = ws_stream.split();

    while let Some(msg) = rx.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => return,
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
            Message::Close(_) => return,
            _ => continue,
        };

        let frame: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let frame_type = frame.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let id = frame
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();

        match frame_type {
            "req" => {
                let method = frame.get("method").and_then(|v| v.as_str()).unwrap_or("");
                if method == "device.connect" {
                    let hello = GatewayHello {
                        device_id: "server-device-1".to_owned(),
                        server_id: "server-1".to_owned(),
                        scopes: vec!["operator.admin".to_owned()],
                        expires_at_unix: Some(1_900_000_000),
                    };
                    let payload = serde_json::to_value(&hello).unwrap();
                    let response = build_ok_response(&id, Some(payload));
                    let text = serde_json::to_string(&response).unwrap();
                    let _ = tx.send(Message::Text(text)).await;
                } else if method == "device.run.send" {
                    // Echo runId/prompt back, then stream events, then run.complete.
                    let run_id = "r-1234";
                    let response = build_ok_response(
                        &id,
                        Some(json!({ "runId": run_id, "status": "running" })),
                    );
                    let text = serde_json::to_string(&response).unwrap();
                    let _ = tx.send(Message::Text(text)).await;

                    // Send 3 stream.chunk events
                    for (i, text_chunk) in ["hello ", "from ", "server"].iter().enumerate() {
                        let event = build_event(
                            "stream.chunk",
                            Some(json!({ "delta": text_chunk, "seq": i })),
                        );
                        let text = serde_json::to_string(&event).unwrap();
                        let _ = tx.send(Message::Text(text)).await;
                        // Small delay between events
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }

                    // Send run.complete
                    let complete =
                        build_event("run.complete", Some(json!({ "summary": "echo done" })));
                    let text = serde_json::to_string(&complete).unwrap();
                    let _ = tx.send(Message::Text(text)).await;
                } else {
                    // Unknown method — error response
                    let error = serde_json::json!({
                        "type": "res",
                        "id": id,
                        "ok": false,
                        "error": { "code": "UNKNOWN_METHOD", "message": format!("unknown method: {method}") }
                    });
                    let text = serde_json::to_string(&error).unwrap();
                    let _ = tx.send(Message::Text(text)).await;
                }
            }
            _ => {}
        }
    }
}

fn test_identity() -> GatewayDeviceIdentity {
    GatewayDeviceIdentity {
        device_id: "client-dev-1".to_owned(),
        public_key_raw_base64_url: "AAAA".repeat(8),
        private_key_pem: "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----\n".to_owned(),
        source: DeviceIdentitySource::Configured,
    }
}

#[tokio::test]
async fn tungstenite_client_connects_to_real_ws_server() {
    let addr = spawn_echo_server().await;
    let url = format!("ws://{addr}/ws");
    let opts = make_connect_options(url.clone(), test_identity());
    let connect_params = json!({
        "deviceId": "client-dev-1",
        "clientId": "gateway-client",
        "clientMode": "backend",
        "clientVersion": "paperclip",
        "role": "operator",
        "scopes": ["operator.admin"],
    });

    let (client, hello) =
        TungsteniteWireClient::connect(&url, &opts, connect_params, Duration::from_secs(5))
            .await
            .expect("connect should succeed");

    assert_eq!(hello.device_id, "server-device-1");
    assert_eq!(hello.server_id, "server-1");
    assert!(client.is_connected().await);

    let _ = client.disconnect().await;
}

#[tokio::test]
async fn tungstenite_client_send_request_roundtrip() {
    let addr = spawn_echo_server().await;
    let url = format!("ws://{addr}/ws");
    let opts = make_connect_options(url.clone(), test_identity());
    let connect_params = json!({});

    let (client, _hello) =
        TungsteniteWireClient::connect(&url, &opts, connect_params, Duration::from_secs(5))
            .await
            .expect("connect");

    let params = json!({
        "runId": "r-1234",
        "prompt": "echo test",
        "sessionKey": "agent:dev-1:issue-1",
    });
    let resp = client
        .send_request("device.run.send", Some(params), Duration::from_secs(5))
        .await
        .expect("send_request");
    assert_eq!(resp["runId"], "r-1234");
    assert_eq!(resp["status"], "running");

    let _ = client.disconnect().await;
}

#[tokio::test]
async fn tungstenite_client_streams_events() {
    let addr = spawn_echo_server().await;
    let url = format!("ws://{addr}/ws");
    let opts = make_connect_options(url.clone(), test_identity());
    let (client, _hello) =
        TungsteniteWireClient::connect(&url, &opts, json!({}), Duration::from_secs(5))
            .await
            .expect("connect");

    // Trigger run.send which causes server to push 3 chunks + run.complete
    let _ = client
        .send_request(
            "device.run.send",
            Some(json!({ "prompt": "x" })),
            Duration::from_secs(5),
        )
        .await
        .expect("send");

    // Read 4 events: 3 stream.chunk + 1 run.complete
    let mut collected: Vec<GatewayEventFrame> = Vec::new();
    for _ in 0..4 {
        let event = tokio::time::timeout(Duration::from_secs(3), client.next_event())
            .await
            .expect("event timeout")
            .expect("event present");
        collected.push(event);
    }

    assert_eq!(collected.len(), 4);
    assert_eq!(collected[0].event, "stream.chunk");
    assert_eq!(collected[1].event, "stream.chunk");
    assert_eq!(collected[2].event, "stream.chunk");
    assert_eq!(collected[3].event, "run.complete");

    let text: String = collected[..3]
        .iter()
        .filter_map(|e| {
            e.payload
                .as_ref()
                .and_then(|p| p.get("delta").and_then(|v| v.as_str()))
        })
        .collect();
    assert_eq!(text, "hello from server");

    let _ = client.disconnect().await;
}

#[tokio::test]
async fn tungstenite_client_unknown_method_returns_error() {
    let addr = spawn_echo_server().await;
    let url = format!("ws://{addr}/ws");
    let opts = make_connect_options(url.clone(), test_identity());
    let (client, _hello) =
        TungsteniteWireClient::connect(&url, &opts, json!({}), Duration::from_secs(5))
            .await
            .expect("connect");

    let err = client
        .send_request("device.unknown", Some(json!({})), Duration::from_secs(5))
        .await
        .expect_err("should error");
    match err {
        GatewayError {
            message,
            gateway_code,
        } => {
            assert!(message.contains("unknown method"));
            assert_eq!(gateway_code.as_deref(), Some("UNKNOWN_METHOD"));
        }
    }

    let _ = client.disconnect().await;
}

#[tokio::test]
async fn tungstenite_client_connect_to_closed_port_fails() {
    let opts = make_connect_options("ws://127.0.0.1:1", test_identity());
    let result = TungsteniteWireClient::connect(
        "ws://127.0.0.1:1",
        &opts,
        json!({}),
        Duration::from_secs(2),
    )
    .await;
    assert!(result.is_err(), "connect to closed port should fail");
}

#[tokio::test]
async fn tungstenite_client_implements_gateway_wire_client_trait() {
    use pc_adapter_openclaw_gateway::wire_client::GatewayWireClient;

    let addr = spawn_echo_server().await;
    let url = format!("ws://{addr}/ws");
    let opts = make_connect_options(url.clone(), test_identity());
    let (client, _hello) =
        TungsteniteWireClient::connect(&url, &opts, json!({}), Duration::from_secs(5))
            .await
            .expect("connect");

    // Use trait method to verify it's dyn-compatible
    let dyn_client: std::sync::Arc<dyn GatewayWireClient> = std::sync::Arc::new(client.clone());

    // Trait-level connect should return cached hello
    let hello = dyn_client.connect(&opts).await.expect("trait connect");
    assert_eq!(hello.device_id, "server-device-1");
    assert!(dyn_client.is_connected().await);

    let _ = dyn_client.disconnect().await;
    assert!(!client.is_connected().await);
}
