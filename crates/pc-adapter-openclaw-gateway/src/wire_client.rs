//! OpenClaw Gateway wire client abstraction —— 对齐 Node
//! `execute.ts::GatewayClient`（lib0 WS 双工 + frame 协议）。
//!
//! WS 层是 openclaw-gateway 的核心 IO，比 cursor-cloud HTTP 更复杂：
//! 1. 长连接（一次握手，多次请求）
//! 2. JSON frame 双向 codec（已抽出到 `frame_codec`）
//! 3. Request ID 关联响应 → resolver
//! 4. Server-pushed event 异步 stream
//! 5. 设备身份握手（Ed25519 SPKI）
//!
//! 本模块抽象为 trait + scripted FakeClient，便于 e2e + 单测。

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::frame_codec::{GatewayEventFrame, GatewayRequestFrame, GatewayResponseFrame};

// ─── Connect options ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectOptions {
    pub gateway_url: String,
    pub identity: GatewayDeviceIdentity,
    pub client_id: String,
    pub client_mode: String,
    pub client_version: String,
    pub role: String,
    pub scopes: Vec<String>,
    pub connect_timeout_ms: u64,
}

// Re-exported from `credentials` (single source of truth)
pub use crate::credentials::{DeviceIdentitySource, GatewayDeviceIdentity};

// ─── WireClient trait ────────────────────────────────────────────────

/// Gateway WS wire client 抽象。
///
/// 真实实现用 `tokio-tungstenite`；E2E 测试用 `FakeWireClient`。
#[async_trait::async_trait]
pub trait GatewayWireClient: Send + Sync {
    async fn connect(&self, opts: &ConnectOptions) -> Result<GatewayHello, GatewayError>;
    async fn disconnect(&self) -> Result<(), GatewayError>;
    async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, GatewayError>;
    /// `next_event` —— 单次拉一个 server event（不建议用于热循环 → 用 stream_events）
    async fn next_event(&self, timeout_ms: u64) -> Option<GatewayEventFrame>;
    async fn is_connected(&self) -> bool;
}

/// Boxed client alias.
pub type DynWireClient = Arc<dyn GatewayWireClient>;

/// 握手响应（Node `device.connect` 响应 payload）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayHello {
    pub device_id: String,
    pub server_id: String,
    pub scopes: Vec<String>,
    pub expires_at_unix: Option<i64>,
}

// ─── Error ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayError {
    pub message: String,
    pub gateway_code: Option<String>,
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for GatewayError {}

impl GatewayError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            gateway_code: None,
        }
    }
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.gateway_code = Some(code.into());
        self
    }
}

impl From<GatewayError> for String {
    fn from(err: GatewayError) -> String {
        err.message
    }
}

// ─── Scripted Fake Client ───────────────────────────────────────────

/// 单次脚本步骤。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScriptedStep {
    Connect {
        hello: GatewayHello,
    },
    Disconnect,
    Request {
        method: String,
        payload: Value,
    },
    Event {
        frame: GatewayEventFrame,
    },
    Error {
        message: String,
        code: Option<String>,
    },
}

/// In-memory scripted client —— 顺序消费 script。
///
/// 并发保护：内部 `Mutex<Vec<...>>` + `Mutex<Vec<...>>`。
#[derive(Debug, Default)]
pub struct FakeWireClient {
    pub script: Mutex<Vec<ScriptedStep>>,
    pub calls: Mutex<Vec<String>>,
    pub requests: Mutex<Vec<(String, Option<Value>)>>,
    pub connected: Mutex<bool>,
    pub events_received: Mutex<Vec<GatewayEventFrame>>,
    pub runtime_url: Mutex<Option<String>>,
    pub runtime_identity: Mutex<Option<crate::credentials::GatewayDeviceIdentity>>,
}

impl FakeWireClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_script(script: Vec<ScriptedStep>) -> Self {
        Self {
            script: Mutex::new(script),
            calls: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
            connected: Mutex::new(false),
            events_received: Mutex::new(Vec::new()),
            runtime_url: Mutex::new(None),
            runtime_identity: Mutex::new(None),
        }
    }

    /// 注入运行时 gateway URL + device identity，便于后续 R624.1 接入真实 WS。
    pub fn for_runtime_url(
        url: String,
        identity: crate::credentials::GatewayDeviceIdentity,
    ) -> Self {
        Self {
            runtime_url: Mutex::new(Some(url)),
            runtime_identity: Mutex::new(Some(identity)),
            ..Self::new()
        }
    }

    /// 当前注入的运行时 gateway URL（供测试断言）。
    pub fn runtime_url(&self) -> Option<String> {
        self.runtime_url.lock().expect("runtime_url").clone()
    }

    /// 当前注入的运行时 device identity。
    pub fn runtime_identity(&self) -> Option<crate::credentials::GatewayDeviceIdentity> {
        self.runtime_identity
            .lock()
            .expect("runtime_identity")
            .clone()
    }

    fn pop(&self, tag: &str) -> ScriptedStep {
        let mut script = self.script.lock().expect("script");
        if script.is_empty() {
            // Auto-generate ok responses so tests don't have to enumerate everything
            return match tag {
                "connect" => ScriptedStep::Connect {
                    hello: GatewayHello {
                        device_id: "dev-default".to_owned(),
                        server_id: "srv-default".to_owned(),
                        scopes: vec![],
                        expires_at_unix: None,
                    },
                },
                "send_request" => ScriptedStep::Request {
                    method: "default".to_owned(),
                    payload: json!({}),
                },
                "disconnect" => ScriptedStep::Disconnect,
                _ => ScriptedStep::Disconnect,
            };
        }
        script.remove(0)
    }

    /// Inject events into the in-memory queue (for the client to read via next_event).
    pub fn enqueue_events(&self, events: Vec<GatewayEventFrame>) {
        let mut q = self.events_received.lock().expect("events");
        for e in events {
            q.push(e);
        }
    }
}

#[async_trait::async_trait]
impl GatewayWireClient for FakeWireClient {
    async fn connect(&self, opts: &ConnectOptions) -> Result<GatewayHello, GatewayError> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("connect:{}", opts.gateway_url));
        match self.pop("connect") {
            ScriptedStep::Connect { hello } => {
                *self.connected.lock().expect("connected") = true;
                Ok(hello)
            }
            ScriptedStep::Error { message, code } => Err(match code {
                Some(c) => GatewayError::new(message).with_code(c),
                None => GatewayError::new(message),
            }),
            other => Err(GatewayError::new(format!(
                "unexpected scripted response in connect: {other:?}"
            ))),
        }
    }

    async fn disconnect(&self) -> Result<(), GatewayError> {
        self.calls
            .lock()
            .expect("calls")
            .push("disconnect".to_owned());
        *self.connected.lock().expect("connected") = false;
        match self.pop("disconnect") {
            ScriptedStep::Disconnect => Ok(()),
            ScriptedStep::Error { message, code } => Err(match code {
                Some(c) => GatewayError::new(message).with_code(c),
                None => GatewayError::new(message),
            }),
            other => Err(GatewayError::new(format!(
                "unexpected scripted response in disconnect: {other:?}"
            ))),
        }
    }

    async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, GatewayError> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("send_request:{method}"));
        self.requests
            .lock()
            .expect("requests")
            .push((method.to_owned(), params.clone()));
        match self.pop("send_request") {
            ScriptedStep::Request {
                method: _m,
                payload,
            } => Ok(payload),
            ScriptedStep::Error { message, code } => Err(match code {
                Some(c) => GatewayError::new(message).with_code(c),
                None => GatewayError::new(message),
            }),
            other => Err(GatewayError::new(format!(
                "unexpected scripted response in send_request: {other:?}"
            ))),
        }
    }

    async fn next_event(&self, _timeout_ms: u64) -> Option<GatewayEventFrame> {
        // First drain any directly enqueued events (via `enqueue_events`).
        {
            let mut q = self.events_received.lock().expect("events");
            if !q.is_empty() {
                return Some(q.remove(0));
            }
        }
        // Then fall back to scripted events (ScriptedStep::Event in the script).
        match self.pop("next_event") {
            ScriptedStep::Event { frame } => Some(frame),
            _ => None,
        }
    }

    async fn is_connected(&self) -> bool {
        *self.connected.lock().expect("connected")
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

/// 构造 ConnectOptions（公共入口）。
pub fn make_connect_options(
    gateway_url: impl Into<String>,
    identity: GatewayDeviceIdentity,
) -> ConnectOptions {
    ConnectOptions {
        gateway_url: gateway_url.into(),
        identity,
        client_id: crate::constants::DEFAULT_CLIENT_ID.to_owned(),
        client_mode: crate::constants::DEFAULT_CLIENT_MODE.to_owned(),
        client_version: crate::constants::DEFAULT_CLIENT_VERSION.to_owned(),
        role: crate::constants::DEFAULT_ROLE.to_owned(),
        scopes: crate::constants::DEFAULT_SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect(),
        connect_timeout_ms: crate::constants::DEFAULT_CONNECT_TIMEOUT_MS,
    }
}

/// Construct `Request frame` value (test helper).
pub fn build_request(id: &str, method: &str, params: Option<Value>) -> GatewayRequestFrame {
    GatewayRequestFrame::new(method, id, params)
}

/// Construct `Response frame` value (test helper).
pub fn build_ok_response(id: &str, payload: Option<Value>) -> GatewayResponseFrame {
    GatewayResponseFrame::ok(id, payload)
}

/// Construct `Event frame` value (test helper).
pub fn build_event(event_name: &str, payload: Option<Value>) -> GatewayEventFrame {
    GatewayEventFrame::new(event_name, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_identity() -> GatewayDeviceIdentity {
        GatewayDeviceIdentity {
            device_id: "dev-1".to_owned(),
            public_key_raw_base64_url: "AAAA".repeat(8),
            private_key_pem: "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----\n"
                .to_owned(),
            source: DeviceIdentitySource::Configured,
        }
    }

    #[tokio::test]
    async fn fake_connect_starts_connected() {
        let client = FakeWireClient::with_script(vec![ScriptedStep::Connect {
            hello: GatewayHello {
                device_id: "dev-1".to_owned(),
                server_id: "srv-1".to_owned(),
                scopes: vec!["operator.admin".to_owned()],
                expires_at_unix: Some(1234567890),
            },
        }]);
        let opts = make_connect_options("wss://gw.example/ws", test_identity());
        let hello = client.connect(&opts).await.unwrap();
        assert_eq!(hello.device_id, "dev-1");
        assert_eq!(hello.server_id, "srv-1");
        assert!(client.is_connected().await);
    }

    #[tokio::test]
    async fn fake_disconnect_ends_connected() {
        let client = FakeWireClient::with_script(vec![
            ScriptedStep::Connect {
                hello: GatewayHello {
                    device_id: "dev".into(),
                    server_id: "srv".into(),
                    scopes: vec![],
                    expires_at_unix: None,
                },
            },
            ScriptedStep::Disconnect,
        ]);
        let opts = make_connect_options("wss://x", test_identity());
        let _ = client.connect(&opts).await.unwrap();
        assert!(client.is_connected().await);
        let _ = client.disconnect().await.unwrap();
        assert!(!client.is_connected().await);
    }

    #[tokio::test]
    async fn fake_send_request_returns_payload() {
        let client = FakeWireClient::with_script(vec![ScriptedStep::Request {
            method: "device.run.send".to_owned(),
            payload: json!({"runId": "r-1", "ok": true}),
        }]);
        let resp = client
            .send_request("device.run.send", Some(json!({"prompt": "hi"})))
            .await
            .unwrap();
        assert_eq!(resp["runId"], "r-1");
        assert_eq!(resp["ok"], true);
        let recorded = client.requests.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "device.run.send");
    }

    #[tokio::test]
    async fn fake_error_propagates_with_code() {
        let client = FakeWireClient::with_script(vec![ScriptedStep::Error {
            message: "denied".to_owned(),
            code: Some("FORBIDDEN".to_owned()),
        }]);
        let opts = make_connect_options("wss://x", test_identity());
        let err = client.connect(&opts).await.unwrap_err();
        assert_eq!(err.message, "denied");
        assert_eq!(err.gateway_code.as_deref(), Some("FORBIDDEN"));
    }

    #[tokio::test]
    async fn fake_next_event_returns_enqueued_events_in_order() {
        let client = FakeWireClient::new();
        client.enqueue_events(vec![
            build_event("state.changed", Some(json!({"k": 1}))),
            build_event("stream.chunk", Some(json!({"text": "hi"}))),
        ]);
        let e1 = client.next_event(1000).await.unwrap();
        assert_eq!(e1.event, "state.changed");
        let e2 = client.next_event(1000).await.unwrap();
        assert_eq!(e2.event, "stream.chunk");
        assert!(client.next_event(1000).await.is_none());
    }

    #[tokio::test]
    async fn auto_generated_connect_hello_when_script_empty() {
        let client = FakeWireClient::new();
        let opts = make_connect_options("wss://x", test_identity());
        let hello = client.connect(&opts).await.unwrap();
        assert!(!hello.device_id.is_empty());
    }

    #[tokio::test]
    async fn call_log_records_invocation_order() {
        let client = FakeWireClient::with_script(vec![
            ScriptedStep::Connect {
                hello: GatewayHello {
                    device_id: "x".into(),
                    server_id: "y".into(),
                    scopes: vec![],
                    expires_at_unix: None,
                },
            },
            ScriptedStep::Request {
                method: "test".to_owned(),
                payload: json!({}),
            },
            ScriptedStep::Disconnect,
        ]);
        let opts = make_connect_options("wss://x", test_identity());
        let _ = client.connect(&opts).await.unwrap();
        let _ = client.send_request("test", None).await.unwrap();
        let _ = client.disconnect().await.unwrap();
        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(calls[0].starts_with("connect:"));
        assert!(calls[1].starts_with("send_request:test"));
        assert_eq!(calls[2], "disconnect");
    }

    #[test]
    fn gateway_error_displays_message() {
        let e = GatewayError::new("oops").with_code("INVALID_REQUEST");
        assert_eq!(format!("{e}"), "oops");
    }

    #[test]
    fn gateway_error_to_string_conversion() {
        let e = GatewayError::new("boom");
        let s: String = e.clone().into();
        assert_eq!(s, "boom");
    }

    #[test]
    fn build_request_constructs_correct_frame() {
        let f = build_request("r-1", "device.connect", Some(json!({"x": 1})));
        assert_eq!(f.id, "r-1");
        assert_eq!(f.method, "device.connect");
        assert_eq!(f.params.unwrap()["x"], 1);
    }

    #[test]
    fn build_ok_response_constructs_frame() {
        let f = build_ok_response("r-1", Some(json!({"ok": true})));
        assert!(f.ok);
        assert_eq!(f.payload.unwrap()["ok"], true);
    }

    #[test]
    fn build_event_constructs_frame() {
        let f = build_event("state.changed", Some(json!({"k": "v"})));
        assert_eq!(f.event, "state.changed");
        assert_eq!(f.payload.unwrap()["k"], "v");
        assert!(f.seq.is_none());
    }

    #[tokio::test]
    async fn make_connect_options_uses_constants_defaults() {
        let opts = make_connect_options("wss://x", test_identity());
        assert_eq!(opts.client_id, crate::constants::DEFAULT_CLIENT_ID);
        assert_eq!(opts.client_mode, crate::constants::DEFAULT_CLIENT_MODE);
        assert_eq!(
            opts.client_version,
            crate::constants::DEFAULT_CLIENT_VERSION
        );
        assert_eq!(opts.role, crate::constants::DEFAULT_ROLE);
        assert_eq!(
            opts.scopes,
            crate::constants::DEFAULT_SCOPES
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }
}
