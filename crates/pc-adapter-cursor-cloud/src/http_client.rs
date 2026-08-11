//! Cursor Cloud real HTTP client —— 用 `reqwest` 实现 `CursorCloudClient` trait。
//!
//! 与 `FakeCursorCloudClient` 的关系：
//! - 同 `CursorCloudClient` trait —— 可直接注入 `execute_with_client`
//! - 真实环境用 `ReqwestCursorCloudClient::new(base_url)` 创建
//! - 测试环境用 `FakeCursorCloudClient::with_script(...)` 创建
//!
//! 设计要点：
//! - **Placeholder endpoints**：5 个 REST endpoint 路径（`/agents`、`/agents/{id}/runs`、`...`），
//!   由于 Cursor Cloud REST API 未公开文档，这些 endpoint shape 是 **interface contract**
//!   —— 一旦 SDK docs 释出，可批量替换。
//! - **Auth header**：`X-API-Key: <api_key>`（与 Node `@cursor/sdk` 一致）
//! - **Error mapping**：HTTP 4xx → CloudError with `gateway_code`；5xx → CloudError("server error")
//! - **Timeout**：每个请求 30s 默认（可通过 builder 配置）
//!
//! 不假设 Cursor Cloud 实际 REST shape —— 这是 transport-layer 骨架，
//! 等 SDK docs 释出后只需替换 5 个 `endpoint_url` 调用即可。

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{json, Value};

use crate::cloud_client::{
    AgentOptions, CloudAgent, CloudError, CloudRun, CursorCloudClient, RunFetchOptions,
    SdkTransportMessage, SendOptions,
};

/// 真实 HTTP 客户端。
///
/// Clone 是 cheap 的（内部 `reqwest::Client` 已经是 `Arc`）。
#[derive(Clone)]
pub struct ReqwestCursorCloudClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl ReqwestCursorCloudClient {
    /// Create a new client with default 30s request timeout.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client build");
        Self {
            inner: Arc::new(Inner {
                http,
                base_url: base_url.into(),
                api_key: api_key.into(),
            }),
        }
    }

    /// Builder for custom timeout.
    pub fn with_timeout(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client build");
        Self {
            inner: Arc::new(Inner {
                http,
                base_url: base_url.into(),
                api_key: api_key.into(),
            }),
        }
    }

    /// Build standard auth headers.
    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&self.inner.api_key) {
            headers.insert(HeaderName::from_static("x-api-key"), v);
        }
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );
        headers
    }

    /// Execute a POST request and parse JSON response or return CloudError.
    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, CloudError> {
        let url = format!("{}{}", self.inner.base_url, path);
        let headers = self.auth_headers();
        let resp = self
            .inner
            .http
            .post(&url)
            .headers(headers)
            .json(body)
            .send()
            .await
            .map_err(|e| CloudError::new(format!("http request failed: {e}")))?;
        parse_response(resp).await
    }

    /// Execute a GET request and parse JSON response.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, CloudError> {
        let url = format!("{}{}", self.inner.base_url, path);
        let headers = self.auth_headers();
        let resp = self
            .inner
            .http
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| CloudError::new(format!("http request failed: {e}")))?;
        parse_response(resp).await
    }

    /// Build the JSON body for create_agent from AgentOptions.
    fn build_create_body(&self, opts: &AgentOptions) -> Value {
        json!({
            "name": opts.name,
            "model": opts.model,
            "envType": opts.env_type,
            "envName": opts.env_name,
            "repos": opts.repos,
            "workOnCurrentBranch": opts.work_on_current_branch,
            "autoCreatePr": opts.auto_create_pr,
            "skipReviewerRequest": opts.skip_reviewer_request,
            "envVars": opts.env_vars,
        })
    }
}

/// Map reqwest Response to Result<T, CloudError> based on status code.
async fn parse_response<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<T, CloudError> {
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| CloudError::new(format!("http body read failed: {e}")))?;

    if status.is_success() {
        serde_json::from_slice(&bytes)
            .map_err(|e| CloudError::new(format!("response parse failed: {e}")))
    } else {
        // Try to parse error body
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let message = body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("http error")
            .to_owned();
        let code = body
            .get("code")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| status.to_string());
        Err(CloudError::new(message).with_code(code).with_details(body))
    }
}

#[async_trait::async_trait]
impl CursorCloudClient for ReqwestCursorCloudClient {
    async fn create_agent(&self, opts: &AgentOptions) -> Result<CloudAgent, CloudError> {
        let body = self.build_create_body(opts);
        let resp: CloudAgent = self.post_json("/agents", &body).await?;
        Ok(resp)
    }

    async fn resume_agent(
        &self,
        agent_id: &str,
        _opts: &AgentOptions,
    ) -> Result<CloudAgent, CloudError> {
        // Resume: GET /agents/{id}
        let path = format!("/agents/{agent_id}");
        let resp: CloudAgent = self.get_json(&path).await?;
        Ok(resp)
    }

    async fn get_run(
        &self,
        run_id: &str,
        _opts: &RunFetchOptions,
    ) -> Result<Option<CloudRun>, CloudError> {
        let path = format!("/runs/{run_id}");
        // 404 → None
        let url = format!("{}{}", self.inner.base_url, path);
        let headers = self.auth_headers();
        let resp = self
            .inner
            .http
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| CloudError::new(format!("http request failed: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let run: CloudRun = parse_response(resp).await?;
        Ok(Some(run))
    }

    async fn send_prompt(
        &self,
        agent: &CloudAgent,
        prompt: &str,
        opts: &SendOptions,
    ) -> Result<CloudRun, CloudError> {
        let path = format!("/agents/{}/runs", agent.agent_id);
        let body = json!({
            "prompt": prompt,
            "model": opts.model,
        });
        let resp: CloudRun = self.post_json(&path, &body).await?;
        Ok(resp)
    }

    async fn stream_messages(
        &self,
        run: &CloudRun,
        sink: &mut (dyn FnMut(SdkTransportMessage) + Send),
    ) -> Result<(), CloudError> {
        // Stream messages via SSE from /runs/{id}/messages
        let url = format!("{}/runs/{}/messages", self.inner.base_url, run.id);
        let headers = self.auth_headers();
        let resp = self
            .inner
            .http
            .get(&url)
            .headers(headers)
            .header("accept", "text/event-stream")
            .send()
            .await
            .map_err(|e| CloudError::new(format!("http stream failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let _ = resp.bytes().await;
            return Err(CloudError::new(format!(
                "stream returned non-success status: {status}"
            )));
        }

        // Read SSE stream by consuming the response body
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| CloudError::new(format!("stream read: {e}")))?;
        let mut buffer = String::from_utf8_lossy(&bytes).to_string();

        // Parse SSE: blocks separated by "\n\n", each starts with "data: "
        while let Some(idx) = buffer.find("\n\n") {
            let event = buffer[..idx].to_owned();
            buffer.drain(..idx + 2);
            for line in event.lines() {
                if let Some(rest) = line.strip_prefix("data: ") {
                    if let Ok(value) = serde_json::from_str::<Value>(rest.trim()) {
                        if let Some(msg) = parse_sse_message(&value) {
                            sink(msg);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn wait_for_run(&self, run: &CloudRun) -> Result<CloudRun, CloudError> {
        // Poll GET /runs/{id} until status != Running
        let path = format!("/runs/{}", run.id);
        for _attempt in 0..120 {
            let url = format!("{}{}", self.inner.base_url, path);
            let headers = self.auth_headers();
            let resp = self
                .inner
                .http
                .get(&url)
                .headers(headers)
                .send()
                .await
                .map_err(|e| CloudError::new(format!("poll failed: {e}")))?;
            let status = resp.status();
            if !status.is_success() {
                return Err(CloudError::new(format!(
                    "poll returned non-success status: {status}"
                )));
            }
            if !status.is_success() {
                return Err(CloudError::new(format!(
                    "poll returned non-success: {status}"
                )));
            }
            let polled: CloudRun = resp
                .json()
                .await
                .map_err(|e| CloudError::new(format!("poll parse: {e}")))?;
            if !matches!(polled.status, crate::cloud_client::CloudRunStatus::Running) {
                return Ok(polled);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(CloudError::new("wait_for_run: timed out after 60s"))
    }
}

/// Map SSE JSON value to SdkTransportMessage.
fn parse_sse_message(value: &Value) -> Option<SdkTransportMessage> {
    let kind = value.get("type").and_then(|v| v.as_str())?;
    match kind {
        "assistant" => value
            .get("text")
            .and_then(|v| v.as_str())
            .map(|t| SdkTransportMessage::Assistant { text: t.to_owned() }),
        "user" => value
            .get("text")
            .and_then(|v| v.as_str())
            .map(|t| SdkTransportMessage::User { text: t.to_owned() }),
        "thinking" => value
            .get("text")
            .and_then(|v| v.as_str())
            .map(|t| SdkTransportMessage::Thinking { text: t.to_owned() }),
        "tool_call" => Some(SdkTransportMessage::ToolCall {
            name: value
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            status: value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            args: value.get("args").cloned(),
            result: value.get("result").cloned(),
        }),
        "tool_result" => Some(SdkTransportMessage::ToolResult {
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
        "status" => Some(SdkTransportMessage::Status {
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
        "task" => value
            .get("text")
            .and_then(|v| v.as_str())
            .map(|t| SdkTransportMessage::Task { text: t.to_owned() }),
        _ => None,
    }
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sse_message_extracts_assistant_text() {
        let v = json!({"type": "assistant", "text": "hello"});
        let m = parse_sse_message(&v).expect("should parse");
        match m {
            SdkTransportMessage::Assistant { text } => assert_eq!(text, "hello"),
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_message_extracts_tool_call() {
        let v = json!({
            "type": "tool_call",
            "name": "bash",
            "status": "running",
            "args": {"cmd": "ls"}
        });
        let m = parse_sse_message(&v).expect("should parse");
        match m {
            SdkTransportMessage::ToolCall {
                name, status, args, ..
            } => {
                assert_eq!(name, "bash");
                assert_eq!(status, "running");
                assert!(args.is_some());
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_message_returns_none_for_unknown_kind() {
        let v = json!({"type": "unknown_thing", "data": "x"});
        assert!(parse_sse_message(&v).is_none());
    }

    #[test]
    fn build_create_body_includes_all_fields() {
        let client = ReqwestCursorCloudClient::new("https://api.example.com", "test-key");
        let opts = AgentOptions {
            api_key: "k".to_owned(),
            name: "TestAgent".to_owned(),
            model: Some("gpt-4".to_owned()),
            env_type: crate::session_codec::RuntimeEnvType::Cloud,
            env_name: None,
            repos: vec![],
            work_on_current_branch: true,
            auto_create_pr: false,
            skip_reviewer_request: false,
            env_vars: HashMap::new(),
        };
        let body = client.build_create_body(&opts);
        assert_eq!(body["name"], "TestAgent");
        assert_eq!(body["model"], "gpt-4");
        assert_eq!(body["workOnCurrentBranch"], true);
        assert_eq!(body["autoCreatePr"], false);
    }
}

use std::collections::HashMap;
