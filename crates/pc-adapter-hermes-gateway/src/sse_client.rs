//! Hermes gateway SSE 事件流消费。
//!
//! 协议（对齐 Node `consumeEvents` in `packages/adapters/hermes/src/gateway/server/execute.ts`）：
//! - HTTP `GET /v1/events`（或 `/v1/runs/{id}/events`），`Accept: text/event-stream`
//! - 服务端推送 `data: <json>` 行（每行一个 SSE event）
//! - 事件 shape：`{type: "agent_message" | "tool_call" | "task_complete" | ..., payload: {...}}`
//!
//! 设计：
//! - **`SseEvent` 枚举** —— typed SSE 消息（与 cursor-cloud 的 `SdkTransportMessage` 类似）
//! - **`SseEventSink` trait** —— 可注入的 sink（生产用 emitter、测试用 in-memory 收集器）
//! - **`parse_sse_chunk` 纯函数** —— 把 raw bytes → `Vec<SseEvent>`
//! - **`HermesSseClient` struct** —— reqwest + 真实 SSE 流消费
//! - **reconnect backoff** —— 用 `retry_policy` 模块里的 `backoff_with_jitter`

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Mutex;

use crate::retry_policy::backoff_with_jitter;

/// SSE 事件类型（typed，与 cursor-cloud SdkTransportMessage 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SseEvent {
    /// agent 输出文本（partial 或 final）
    AgentMessage { text: String, delta: bool },
    /// 工具调用发起
    ToolCall { name: String, args: Option<Value> },
    /// 工具调用结果
    ToolResult {
        name: String,
        is_error: bool,
        content: Option<Value>,
    },
    /// 状态变化
    Status {
        status: String,
        message: Option<String>,
    },
    /// 任务完成（terminal）
    TaskComplete { summary: Option<String> },
    /// 任务失败（terminal）
    TaskFailed { error: String },
    /// 未知类型（保持 forward compat）
    Unknown { raw_type: String, payload: Value },
}

impl SseEvent {
    /// 是否为终态事件。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            SseEvent::TaskComplete { .. } | SseEvent::TaskFailed { .. }
        )
    }

    /// 提取 user-facing 文本（若事件携带 text 字段）。
    pub fn extract_text(&self) -> Option<String> {
        match self {
            SseEvent::AgentMessage { text, .. } => Some(text.clone()),
            SseEvent::Status {
                status,
                message: Some(msg),
            } if !msg.is_empty() => Some(format!("[{status}] {msg}")),
            _ => None,
        }
    }
}

/// SSE event sink（trait，可注入）。
pub trait SseEventSink: Send + Sync {
    fn emit(&self, event: SseEvent) -> Result<(), String>;
}

/// 解析 SSE chunk（多行 `data: ` 行，每行一个 JSON event）。
pub fn parse_sse_chunk(chunk: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut buffer = String::new();

    for line in chunk.lines() {
        if line.is_empty() {
            if !buffer.is_empty() {
                if let Some(event) = parse_sse_data(&buffer) {
                    events.push(event);
                }
                buffer.clear();
            }
        } else if let Some(rest) = line.strip_prefix("data: ") {
            if !buffer.is_empty() {
                buffer.push('\n');
            }
            buffer.push_str(rest);
        }
    }

    if !buffer.is_empty() {
        if let Some(event) = parse_sse_data(&buffer) {
            events.push(event);
        }
    }

    events
}

fn parse_sse_data(data: &str) -> Option<SseEvent> {
    let value: Value = serde_json::from_str(data.trim()).ok()?;
    let event_type = value.get("type").and_then(|v| v.as_str())?;
    match event_type {
        "agent_message" => {
            let text = value
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let delta = value
                .get("delta")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Some(SseEvent::AgentMessage { text, delta })
        }
        "tool_call" => Some(SseEvent::ToolCall {
            name: value
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            args: value.get("args").cloned(),
        }),
        "tool_result" => Some(SseEvent::ToolResult {
            name: value
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            is_error: value
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            content: value.get("content").cloned(),
        }),
        "status" => Some(SseEvent::Status {
            status: value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            message: value
                .get("message")
                .and_then(|v| v.as_str())
                .map(String::from),
        }),
        "task_complete" => Some(SseEvent::TaskComplete {
            summary: value
                .get("summary")
                .and_then(|v| v.as_str())
                .map(String::from),
        }),
        "task_failed" => Some(SseEvent::TaskFailed {
            error: value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("task failed")
                .to_owned(),
        }),
        _ => Some(SseEvent::Unknown {
            raw_type: event_type.to_owned(),
            payload: value,
        }),
    }
}

/// Hermes SSE HTTP 客户端。
#[derive(Clone)]
pub struct HermesSseClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    session_key: Option<String>,
}

impl HermesSseClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        session_key: Option<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client build");
        Self {
            inner: Arc::new(Inner {
                http,
                base_url: base_url.into(),
                api_key: api_key.into(),
                session_key,
            }),
        }
    }

    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&self.inner.api_key) {
            headers.insert(HeaderName::from_static("authorization"), v);
        }
        if let Some(sk) = &self.inner.session_key {
            if let Ok(v) = HeaderValue::from_str(sk) {
                headers.insert(HeaderName::from_static("x-hermes-session-key"), v);
            }
        }
        headers.insert(
            HeaderName::from_static("accept"),
            HeaderValue::from_static("text/event-stream"),
        );
        headers
    }

    /// Consume SSE stream from `/v1/events` until terminal event or error.
    pub async fn consume_until_terminal(
        &self,
        path: &str,
        sink: &dyn SseEventSink,
        max_reconnects: u32,
    ) -> Result<SseStreamResult, String> {
        let mut collected: Vec<SseEvent> = Vec::new();
        let mut terminal: Option<SseEvent> = None;
        let mut attempt: u32 = 0;
        let url = format!("{}{}", self.inner.base_url, path);

        loop {
            if attempt > max_reconnects {
                return Err(format!(
                    "sse stream disconnected after {max_reconnects} reconnect attempts"
                ));
            }
            let headers = self.auth_headers();
            let resp = self
                .inner
                .http
                .get(&url)
                .headers(headers)
                .send()
                .await
                .map_err(|e| format!("sse request: {e}"))?;

            if !resp.status().is_success() {
                return Err(format!("sse returned non-success: {}", resp.status()));
            }

            // Read full response body (simpler than chunked bytes_stream)
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| format!("sse body read: {e}"))?;
            let mut buffer = String::from_utf8_lossy(&bytes).to_string();

            // Parse all events from buffer
            while let Some(idx) = buffer.find("\n\n") {
                let event_str: String = buffer.drain(..idx + 2).collect();
                let events = parse_sse_chunk(&event_str);
                for event in events {
                    if event.is_terminal() {
                        terminal = Some(event.clone());
                    }
                    let _ = sink.emit(event.clone());
                    collected.push(event);
                }
            }

            if terminal.is_some() {
                return Ok(SseStreamResult {
                    events: collected,
                    terminal,
                    reconnects: attempt,
                });
            }

            attempt += 1;
            let backoff_ms = backoff_with_jitter(attempt, 250, 30_000);
            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        }
    }
}

/// SSE 流消费结果。
#[derive(Debug, Clone)]
pub struct SseStreamResult {
    pub events: Vec<SseEvent>,
    pub terminal: Option<SseEvent>,
    pub reconnects: u32,
}

/// In-memory SSE sink（用于测试）。
#[derive(Debug, Default, Clone)]
pub struct InMemorySseSink {
    pub events: Arc<Mutex<Vec<SseEvent>>>,
}

impl InMemorySseSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Vec<SseEvent> {
        self.events.lock().expect("events").clone()
    }
}

impl SseEventSink for InMemorySseSink {
    fn emit(&self, event: SseEvent) -> Result<(), String> {
        self.events.lock().expect("events").push(event);
        Ok(())
    }
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sse_chunk_single_event() {
        let chunk = "data: {\"type\":\"agent_message\",\"text\":\"hello\"}\n\n";
        let events = parse_sse_chunk(chunk);
        assert_eq!(events.len(), 1);
        match &events[0] {
            SseEvent::AgentMessage { text, delta } => {
                assert_eq!(text, "hello");
                assert!(!delta);
            }
            _ => panic!("expected AgentMessage"),
        }
    }

    #[test]
    fn parse_sse_chunk_multiple_events() {
        let chunk = "data: {\"type\":\"agent_message\",\"text\":\"a\"}\n\ndata: {\"type\":\"tool_call\",\"name\":\"bash\"}\n\n";
        let events = parse_sse_chunk(chunk);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], SseEvent::AgentMessage { .. }));
        assert!(matches!(events[1], SseEvent::ToolCall { .. }));
    }

    #[test]
    fn parse_sse_chunk_terminal_event() {
        let chunk = "data: {\"type\":\"task_complete\",\"summary\":\"done\"}\n\n";
        let events = parse_sse_chunk(chunk);
        assert_eq!(events.len(), 1);
        assert!(events[0].is_terminal());
        match &events[0] {
            SseEvent::TaskComplete { summary } => {
                assert_eq!(summary.as_deref(), Some("done"));
            }
            _ => panic!("expected TaskComplete"),
        }
    }

    #[test]
    fn parse_sse_chunk_unknown_type_returns_unknown_variant() {
        let chunk = "data: {\"type\":\"weird_thing\",\"x\":1}\n\n";
        let events = parse_sse_chunk(chunk);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SseEvent::Unknown { .. }));
    }

    #[test]
    fn parse_sse_chunk_ignores_comment_lines() {
        let chunk = ": this is a comment\ndata: {\"type\":\"status\",\"status\":\"running\"}\n\n: another comment\n";
        let events = parse_sse_chunk(chunk);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SseEvent::Status { .. }));
    }

    #[test]
    fn sse_event_extract_text_returns_agent_message_text() {
        let event = SseEvent::AgentMessage {
            text: "hello".to_owned(),
            delta: false,
        };
        assert_eq!(event.extract_text().as_deref(), Some("hello"));
    }

    #[test]
    fn sse_event_extract_text_returns_status_with_message() {
        let event = SseEvent::Status {
            status: "running".to_owned(),
            message: Some("computing".to_owned()),
        };
        assert_eq!(event.extract_text().as_deref(), Some("[running] computing"));
    }

    #[test]
    fn sse_event_extract_text_none_for_tool_call() {
        let event = SseEvent::ToolCall {
            name: "bash".to_owned(),
            args: None,
        };
        assert!(event.extract_text().is_none());
    }

    #[test]
    fn in_memory_sink_collects_events() {
        let sink = InMemorySseSink::new();
        sink.emit(SseEvent::AgentMessage {
            text: "a".into(),
            delta: false,
        })
        .unwrap();
        sink.emit(SseEvent::AgentMessage {
            text: "b".into(),
            delta: false,
        })
        .unwrap();
        let snapshot = sink.snapshot();
        assert_eq!(snapshot.len(), 2);
    }
}
