//! Hermes gateway Dashboard REST 客户端。
//!
//! 协议（对齐 Node `pollStatus` in `packages/adapters/hermes/src/gateway/server/execute.ts`）：
//! - `POST /v1/runs` —— 创建 run，返回 `{run_id, status, ...}`
//! - `GET /v1/runs/{id}` —— 轮询 run 状态（每 `pollIntervalMs` 调一次）
//! - 终态 status：`finished` / `error` / `cancelled`
//!
//! 设计：
//! - **`DashboardClient` struct** —— reqwest + 3 REST endpoints
//! - **`RunStatus` enum** —— typed run 状态
//! - **`HermesRun` struct** —— run handle（id / status / summary / error）
//! - **retry/backoff** —— 用 `retry_policy::backoff_with_jitter`
//! - **header 注入** —— `Authorization: Bearer <api_key>` + `X-Hermes-Session-Key`

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::retry_policy::backoff_with_jitter;

/// Run 状态枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Finished,
    Error,
    Cancelled,
}

impl RunStatus {
    /// 是否为终态。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RunStatus::Finished | RunStatus::Error | RunStatus::Cancelled
        )
    }
}

/// Hermes run handle。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesRun {
    pub run_id: String,
    pub status: RunStatus,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
    pub model: Option<String>,
    /// 完整 raw response（debug / forward compat）
    pub raw: Value,
}

/// 创建 run 的请求 body。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRunRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub session_key: Option<String>,
    pub workspace: Option<String>,
    pub metadata: Option<Value>,
}

/// Dashboard REST 客户端。
#[derive(Clone)]
pub struct DashboardClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    session_key: Option<String>,
}

impl DashboardClient {
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

    fn auth_headers(&self, accept: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&self.inner.api_key) {
            headers.insert(HeaderName::from_static("authorization"), v);
        }
        if let Some(sk) = &self.inner.session_key {
            if let Ok(v) = HeaderValue::from_str(sk) {
                headers.insert(HeaderName::from_static("x-hermes-session-key"), v);
            }
        }
        if let Ok(v) = HeaderValue::from_str(accept) {
            headers.insert(HeaderName::from_static("accept"), v);
        }
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );
        headers
    }

    /// POST /v1/runs —— 创建 run。
    pub async fn create_run(&self, req: &CreateRunRequest) -> Result<HermesRun, String> {
        let url = format!("{}/v1/runs", self.inner.base_url);
        let headers = self.auth_headers("application/json");
        let resp = self
            .inner
            .http
            .post(&url)
            .headers(headers)
            .json(req)
            .send()
            .await
            .map_err(|e| format!("create_run: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(format!("create_run returned {status}"));
        }

        let value: Value = resp
            .json()
            .await
            .map_err(|e| format!("create_run parse: {e}"))?;
        parse_hermes_run(value)
    }

    /// GET /v1/runs/{id} —— 单次轮询。
    pub async fn get_run(&self, run_id: &str) -> Result<HermesRun, String> {
        let url = format!("{}/v1/runs/{run_id}", self.inner.base_url);
        let headers = self.auth_headers("application/json");
        let resp = self
            .inner
            .http
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("get_run: {e}"))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(format!("run {run_id} not found"));
        }
        if !status.is_success() {
            return Err(format!("get_run returned {status}"));
        }

        let value: Value = resp
            .json()
            .await
            .map_err(|e| format!("get_run parse: {e}"))?;
        parse_hermes_run(value)
    }

    /// Poll GET /v1/runs/{id} until terminal or timeout.
    pub async fn poll_until_terminal(
        &self,
        run_id: &str,
        interval_ms: u64,
        timeout_ms: u64,
    ) -> Result<HermesRun, String> {
        let start = std::time::Instant::now();
        let mut attempt: u32 = 0;
        loop {
            if start.elapsed().as_millis() as u64 > timeout_ms {
                return Err(format!(
                    "poll_until_terminal: timed out after {timeout_ms}ms"
                ));
            }
            attempt += 1;
            match self.get_run(run_id).await {
                Ok(run) if run.status.is_terminal() => return Ok(run),
                Ok(run) => {
                    let _ = run; // not terminal yet, keep polling
                }
                Err(e) => {
                    tracing::warn!("poll attempt {attempt} failed: {e}");
                }
            }
            // Backoff with jitter, but cap at interval_ms
            let backoff = backoff_with_jitter(attempt, interval_ms, interval_ms * 4);
            tokio::time::sleep(Duration::from_millis(backoff.min(interval_ms * 4))).await;
        }
    }
}

fn parse_hermes_run(value: Value) -> Result<HermesRun, String> {
    let run_id = value
        .get("run_id")
        .or_else(|| value.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing run_id".to_owned())?
        .to_owned();
    let status: RunStatus = value
        .get("status")
        .and_then(|v| v.as_str())
        .and_then(|s| match s {
            "queued" => Some(RunStatus::Queued),
            "running" => Some(RunStatus::Running),
            "finished" => Some(RunStatus::Finished),
            "error" | "failed" => Some(RunStatus::Error),
            "cancelled" => Some(RunStatus::Cancelled),
            _ => None,
        })
        .unwrap_or(RunStatus::Running);
    Ok(HermesRun {
        run_id,
        status,
        summary: value
            .get("summary")
            .and_then(|v| v.as_str())
            .map(String::from),
        error: value
            .get("error")
            .and_then(|v| v.as_str())
            .map(String::from),
        duration_ms: value.get("duration_ms").and_then(|v| v.as_u64()),
        model: value
            .get("model")
            .and_then(|v| v.as_str())
            .map(String::from),
        raw: value,
    })
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_status_is_terminal_for_finished_error_cancelled() {
        assert!(RunStatus::Finished.is_terminal());
        assert!(RunStatus::Error.is_terminal());
        assert!(RunStatus::Cancelled.is_terminal());
        assert!(!RunStatus::Running.is_terminal());
        assert!(!RunStatus::Queued.is_terminal());
    }

    #[test]
    fn parse_hermes_run_extracts_required_fields() {
        let v = serde_json::json!({
            "run_id": "r-1",
            "status": "finished",
            "summary": "done",
            "duration_ms": 1234,
            "model": "claude-3-5-sonnet",
        });
        let run = parse_hermes_run(v).expect("parse");
        assert_eq!(run.run_id, "r-1");
        assert_eq!(run.status, RunStatus::Finished);
        assert_eq!(run.summary.as_deref(), Some("done"));
        assert_eq!(run.duration_ms, Some(1234));
        assert_eq!(run.model.as_deref(), Some("claude-3-5-sonnet"));
    }

    #[test]
    fn parse_hermes_run_accepts_id_alias() {
        let v = serde_json::json!({"id": "r-2", "status": "running"});
        let run = parse_hermes_run(v).expect("parse");
        assert_eq!(run.run_id, "r-2");
        assert_eq!(run.status, RunStatus::Running);
    }

    #[test]
    fn parse_hermes_run_extracts_error() {
        let v = serde_json::json!({
            "run_id": "r-3",
            "status": "error",
            "error": "rate limited"
        });
        let run = parse_hermes_run(v).expect("parse");
        assert_eq!(run.status, RunStatus::Error);
        assert_eq!(run.error.as_deref(), Some("rate limited"));
    }

    #[test]
    fn parse_hermes_run_fails_without_run_id() {
        let v = serde_json::json!({"status": "running"});
        assert!(parse_hermes_run(v).is_err());
    }

    #[test]
    fn parse_hermes_run_defaults_unknown_status_to_running() {
        let v = serde_json::json!({"run_id": "r-x", "status": "weird"});
        let run = parse_hermes_run(v).expect("parse");
        assert_eq!(run.status, RunStatus::Running);
    }
}
