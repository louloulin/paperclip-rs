//! OpenClaw Gateway real WebSocket client —— 用 `tokio-tungstenite`
//! 实现 `GatewayWireClient` trait。
//!
//! 与 `FakeWireClient` 的关系：
//! - 同 `GatewayWireClient` trait —— 可直接注入 `execute_with_client`
//! - 真实环境用 `TungsteniteWireClient::connect(url)` 创建
//! - 测试环境用 `FakeWireClient::with_script(...)` 创建
//!
//! 设计要点：
//! - **Outbound channel**：调用方 `send_request` 把文本推入 `outbound_tx`，
//!   后台 pump task drain + send
//! - **Pending map**：`Arc<Mutex<HashMap<id, oneshot::Sender<Result<Value, GatewayError>>>>>`
//!   由 pump task 收到 response 后取出并 send
//! - **Event queue**：`mpsc::UnboundedSender<GatewayEventFrame>` 由 pump 推、`next_event` 拉
//! - **Graceful shutdown**：`disconnect()` 关闭 outbound → pump 看到 EOF → 退出

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::frame_codec::{GatewayEventFrame, GatewayResponseFrame};
use crate::wire_client::{
    build_request, ConnectOptions, GatewayError, GatewayHello, GatewayWireClient,
};

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Default)]
struct Inner {
    connected: bool,
    server_url: String,
    device_id: String,
    /// Shared with the pump task — single source of truth for in-flight requests.
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, GatewayError>>>>>,
    events_rx: Option<mpsc::UnboundedReceiver<GatewayEventFrame>>,
    outbound_tx: Option<mpsc::UnboundedSender<String>>,
    pump_task: Option<JoinHandle<()>>,
    outbound_closed: bool,
}

#[derive(Clone)]
pub struct TungsteniteWireClient {
    inner: Arc<Mutex<Inner>>,
}

impl TungsteniteWireClient {
    pub async fn connect(
        url: &str,
        _opts: &ConnectOptions,
        signed_connect_params: Value,
        connect_timeout: Duration,
    ) -> Result<(Self, GatewayHello), GatewayError> {
        let connect_frame =
            build_request("connect-1", "device.connect", Some(signed_connect_params));
        let connect_json = serde_json::to_string(&connect_frame)
            .map_err(|e| GatewayError::new(format!("serialize connect frame: {e}")))?;

        let ws_stream = tokio::time::timeout(connect_timeout, connect_async(url))
            .await
            .map_err(|_| GatewayError::new("ws connect timeout"))?
            .map_err(|e| GatewayError::new(format!("ws connect: {e}")))?
            .0;

        let (mut ws_tx, mut ws_rx) = ws_stream.split();

        ws_tx
            .send(Message::Text(connect_json))
            .await
            .map_err(|e| GatewayError::new(format!("ws send connect: {e}")))?;

        let response_msg = tokio::time::timeout(connect_timeout, ws_rx.next())
            .await
            .map_err(|_| GatewayError::new("connect response timeout"))?
            .ok_or_else(|| GatewayError::new("ws closed before connect response"))?
            .map_err(|e| GatewayError::new(format!("ws recv connect: {e}")))?;

        let response_text = match response_msg {
            Message::Text(t) => t,
            Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
            Message::Close(c) => {
                return Err(GatewayError::new(format!(
                    "server closed during connect: {c:?}"
                )));
            }
            other => {
                return Err(GatewayError::new(format!(
                    "unexpected ws message during connect: {other:?}"
                )));
            }
        };

        let response_frame: GatewayResponseFrame =
            serde_json::from_str(&response_text).map_err(|e| {
                GatewayError::new(format!("parse connect response: {e}; raw={response_text}"))
            })?;

        if !response_frame.ok {
            let err_msg = response_frame
                .error
                .as_ref()
                .and_then(|e| e.message.as_ref())
                .and_then(|v| v.as_str())
                .unwrap_or("connect failed")
                .to_owned();
            let code = response_frame
                .error
                .as_ref()
                .and_then(|e| e.code.as_ref())
                .and_then(|v| v.as_str())
                .map(String::from);
            return Err(match code {
                Some(c) => GatewayError::new(err_msg).with_code(c),
                None => GatewayError::new(err_msg),
            });
        }

        let hello: GatewayHello = match response_frame.payload.clone() {
            Some(v) => serde_json::from_value(v)
                .map_err(|e| GatewayError::new(format!("parse hello payload: {e}")))?,
            None => {
                return Err(GatewayError::new("connect response missing payload"));
            }
        };

        let device_id = hello.device_id.clone();

        let mut stream = ws_tx
            .reunite(ws_rx)
            .map_err(|e| GatewayError::new(format!("ws reunite: {e}")))?;

        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<String>();
        let (events_tx, events_rx) = mpsc::unbounded_channel::<GatewayEventFrame>();

        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, GatewayError>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        // (Same Arc shared with pump task below and Inner above — single source of truth.)

        let pending_for_task = pending.clone();

        let pump_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    msg = stream.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                eprintln!("[DBG pump] routing text len={}", text.len());
                                if let Err(e) = route_text(&text, &pending_for_task, &events_tx) {
                                    tracing::warn!("ws route error: {e}");
                                }
                            }
                            Some(Ok(Message::Binary(bytes))) => {
                                let text = String::from_utf8_lossy(&bytes).to_string();
                                if let Err(e) = route_text(&text, &pending_for_task, &events_tx) {
                                    tracing::warn!("ws route error: {e}");
                                }
                            }
                            Some(Ok(Message::Close(_))) => break,
                            Some(Ok(Message::Ping(_)))
                            | Some(Ok(Message::Pong(_)))
                            | Some(Ok(Message::Frame(_))) => {}
                            Some(Err(e)) => {
                                tracing::warn!("ws recv error: {e}");
                                break;
                            }
                            None => break,
                        }
                    }
                    Some(text) = outbound_rx.recv() => {
                        if let Err(e) = stream.send(Message::Text(text)).await {
                            tracing::warn!("ws send failed: {e}");
                            break;
                        }
                    }
                    else => break,
                }
            }

            let mut p = pending_for_task.lock().expect("pending");
            for (_, sender) in p.drain() {
                let _ = sender.send(Err(GatewayError::new("ws disconnected")));
            }
        });

        Ok((
            Self {
                inner: Arc::new(Mutex::new(Inner {
                    connected: true,
                    server_url: url.to_owned(),
                    device_id,
                    pending, // shared Arc<Mutex<HashMap>> with pump task
                    events_rx: Some(events_rx),
                    outbound_tx: Some(outbound_tx),
                    pump_task: Some(pump_task),
                    outbound_closed: false,
                })),
            },
            hello,
        ))
    }

    pub async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, GatewayError> {
        let id = format!("req-{}", uuid::Uuid::new_v4());
        let frame = build_request(&id, method, params);
        let text = match serde_json::to_string(&frame) {
            Ok(t) => t,
            Err(e) => return Err(GatewayError::new(format!("serialize request: {e}"))),
        };

        let (tx, rx) = oneshot::channel();
        let pending = {
            let inner = self.inner.lock().expect("inner");
            if !inner.connected {
                return Err(GatewayError::new("not connected"));
            }
            inner.pending.clone()
        };
        pending.lock().expect("pending").insert(id.clone(), tx);

        let outbound_tx = {
            let inner = self.inner.lock().expect("inner");
            inner.outbound_tx.clone()
        };
        let outbound_tx = match outbound_tx {
            Some(tx) => tx,
            None => return Err(GatewayError::new("outbound channel closed")),
        };
        if let Err(e) = outbound_tx.send(text.clone()) {
            pending.lock().expect("pending").remove(&id);
            return Err(GatewayError::new(format!("outbound send: {e}")));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_canceled)) => Err(GatewayError::new("response channel closed")),
            Err(_timeout) => {
                pending.lock().expect("pending").remove(&id);
                Err(GatewayError::new("request timeout"))
            }
        }
    }

    pub async fn next_event(&self) -> Option<GatewayEventFrame> {
        let mut rx = {
            let mut inner = self.inner.lock().expect("inner");
            inner.events_rx.take()?
        };
        let event = rx.recv().await;
        self.inner.lock().expect("inner").events_rx = Some(rx);
        event
    }
}

fn route_text(
    text: &str,
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, GatewayError>>>>>,
    events_tx: &mpsc::UnboundedSender<GatewayEventFrame>,
) -> Result<(), String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("parse frame: {e}"))?;
    let frame_type = value
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing frame type".to_owned())?;
    match frame_type {
        "res" => {
            let response: GatewayResponseFrame =
                serde_json::from_value(value).map_err(|e| format!("parse response: {e}"))?;
            let id = response.id.clone();
            eprintln!("[DBG route_text] response id={id} ok={}", response.ok);
            let mut p = pending.lock().expect("pending");
            eprintln!(
                "[DBG route_text] pending keys: {:?}",
                p.keys().collect::<Vec<_>>()
            );
            if let Some(sender) = p.remove(&id) {
                let result = if response.ok {
                    Ok(response.payload.unwrap_or(Value::Null))
                } else {
                    let msg = response
                        .error
                        .as_ref()
                        .and_then(|e| e.message.as_ref())
                        .and_then(|v| v.as_str())
                        .unwrap_or("request failed")
                        .to_owned();
                    let code = response
                        .error
                        .as_ref()
                        .and_then(|e| e.code.as_ref())
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    Err(match code {
                        Some(c) => GatewayError::new(msg).with_code(c),
                        None => GatewayError::new(msg),
                    })
                };
                let _ = sender.send(result);
            } else {
            }
            Ok(())
        }
        "event" => {
            let event: GatewayEventFrame =
                serde_json::from_value(value).map_err(|e| format!("parse event: {e}"))?;
            events_tx
                .send(event)
                .map_err(|_| "event channel closed".to_owned())?;
            Ok(())
        }
        other => Err(format!("unknown frame type: {other}")),
    }
}

#[async_trait::async_trait]
impl GatewayWireClient for TungsteniteWireClient {
    async fn connect(&self, _opts: &ConnectOptions) -> Result<GatewayHello, GatewayError> {
        let inner = self.inner.lock().expect("inner");
        Ok(GatewayHello {
            device_id: inner.device_id.clone(),
            server_id: inner.server_url.clone(),
            scopes: vec![],
            expires_at_unix: None,
        })
    }

    async fn disconnect(&self) -> Result<(), GatewayError> {
        let pump_task = {
            let mut inner = self.inner.lock().expect("inner");
            inner.connected = false;
            inner.outbound_tx.take();
            inner.events_rx.take();
            inner.pump_task.take()
        };
        if let Some(task) = pump_task {
            task.abort();
            let _ = task.await;
        }
        Ok(())
    }

    async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, GatewayError> {
        self.send_request(method, params, Duration::from_secs(30))
            .await
    }

    async fn next_event(&self, _timeout_ms: u64) -> Option<GatewayEventFrame> {
        self.next_event().await
    }

    async fn is_connected(&self) -> bool {
        self.inner.lock().expect("inner").connected
    }
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn route_text_dispatches_response_to_pending() {
        let (tx, rx) = oneshot::channel();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        pending.lock().unwrap().insert("req-1".to_owned(), tx);
        let (events_tx, _events_rx) = mpsc::unbounded_channel();

        let resp = json!({
            "type": "res",
            "id": "req-1",
            "ok": true,
            "payload": {"answer": 42}
        });
        let text = serde_json::to_string(&resp).unwrap();
        route_text(&text, &pending, &events_tx).unwrap();

        let result = rx.await.unwrap().unwrap();
        assert_eq!(result["answer"], 42);
    }

    #[tokio::test]
    async fn route_text_dispatches_event_to_queue() {
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, GatewayError>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        let evt = json!({
            "type": "event",
            "event": "stream.chunk",
            "payload": {"text": "hi"}
        });
        let text = serde_json::to_string(&evt).unwrap();
        route_text(&text, &pending, &events_tx).unwrap();

        let event = events_rx.recv().await.unwrap();
        assert_eq!(event.event, "stream.chunk");
        assert_eq!(event.payload.unwrap()["text"], "hi");
    }

    #[tokio::test]
    async fn route_text_response_with_error_maps_to_gateway_error() {
        let (tx, rx) = oneshot::channel();
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, GatewayError>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        pending.lock().unwrap().insert("req-2".to_owned(), tx);
        let (events_tx, _events_rx) = mpsc::unbounded_channel();

        let resp = json!({
            "type": "res",
            "id": "req-2",
            "ok": false,
            "error": {"code": "FORBIDDEN", "message": "denied"}
        });
        let text = serde_json::to_string(&resp).unwrap();
        route_text(&text, &pending, &events_tx).unwrap();

        let err = rx.await.unwrap().unwrap_err();
        assert_eq!(err.message, "denied");
        assert_eq!(err.gateway_code.as_deref(), Some("FORBIDDEN"));
    }

    #[tokio::test]
    async fn route_text_response_with_unknown_id_silently_dropped() {
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, GatewayError>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let resp = json!({
            "type": "res",
            "id": "req-unknown",
            "ok": true,
            "payload": {}
        });
        let text = serde_json::to_string(&resp).unwrap();
        route_text(&text, &pending, &events_tx).unwrap();
    }

    #[tokio::test]
    async fn route_text_rejects_unknown_frame_type() {
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, GatewayError>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let resp = json!({
            "type": "bogus",
            "id": "req-1"
        });
        let text = serde_json::to_string(&resp).unwrap();
        let err = route_text(&text, &pending, &events_tx).unwrap_err();
        assert!(err.contains("unknown frame type"));
    }
}
