//! Hermes gateway 完整 execute path —— 集成 `dashboard` + `sse_client` 替代简单 CLI spawn。
//!
//! 流程（对齐 Node `execute.ts::execute`）：
//! 1. 校验 `apiBaseUrl`（loopback / escape hatch）
//! 2. 解析 config 字段（apiKey / model / pollIntervalMs / reconnectMs / sessionKeyStrategy）
//! 3. 构造 session key
//! 4. 创建 `DashboardClient` + `HermesSseClient`
//! 5. `POST /v1/runs` 创建 run
//! 6. 后台 spawn SSE consumer —— 把 `SseEvent::AgentMessage.text` → `AdapterEvent::stdout`
//! 7. `poll_until_terminal` 阻塞等待终态
//! 8. 构造 `AdapterExecutionResult` + 发出 `AdapterEvent::Session`
//!
//! 设计要点：
//! - **`HermesExecuteClient` trait** —— mockable transport (POST + SSE 合并接口)
//! - **`DefaultHermesExecuteClient`** —— 生产实现（用 DashboardClient + HermesSseClient）
//! - **`FakeHermesExecuteClient`** —— 单测用剧本驱动
//! - **`execute_with_client(client, ctx, events)`** —— 纯函数，可测试
//! - **`emit_event` helper** —— 把 `SseEvent` → `AdapterEvent::Output`

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEvent, AdapterEventSink,
    AdapterExecutionContext, AdapterExecutionResult, OutputStream,
};
use serde_json::{json, Value};

use crate::constants::{SessionKeyStrategy, ADAPTER_LABEL, ADAPTER_TYPE};
use crate::dashboard::{CreateRunRequest, DashboardClient, HermesRun, RunStatus};
use crate::sse_client::{HermesSseClient, InMemorySseSink, SseEvent, SseEventSink};

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecuteError {
    InvalidConfig(String),
    InvalidBaseUrl(String),
    Transport(String),
    Run(String),
}

impl std::fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecuteError::InvalidConfig(m) => write!(f, "invalid config: {m}"),
            ExecuteError::InvalidBaseUrl(m) => write!(f, "invalid base url: {m}"),
            ExecuteError::Transport(m) => write!(f, "transport: {m}"),
            ExecuteError::Run(m) => write!(f, "run: {m}"),
        }
    }
}

impl std::error::Error for ExecuteError {}

// ============================================================================
// Parsed execute config
// ============================================================================

/// 从 `adapter_config` 提取的 Hermes gateway 配置。
#[derive(Debug, Clone, PartialEq)]
pub struct ExecuteConfig {
    pub api_base_url: String,
    pub api_key: String,
    pub model: Option<String>,
    pub session_key_strategy: SessionKeyStrategy,
    pub poll_interval_ms: u64,
    pub reconnect_ms: u64,
    pub timeout_ms: u64,
    pub max_reconnects: u32,
    pub workspace: Option<String>,
}

fn read_string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(String::from)
}

fn read_u64(v: &Value, key: &str, default: u64) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(default)
}

fn read_u32(v: &Value, key: &str, default: u32) -> u32 {
    v.get(key)
        .and_then(|x| x.as_u64())
        .map(|n| n as u32)
        .unwrap_or(default)
}

/// 把 adapter_config JSON 解析为 ExecuteConfig。
pub fn parse_execute_config(
    config: &Value,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<ExecuteConfig, ExecuteError> {
    let api_base_url = read_string(config, "apiBaseUrl")
        .or_else(|| env.get("HERMES_GATEWAY_BASE_URL").cloned())
        .or_else(|| env.get("HERMES_BASE_URL").cloned())
        .ok_or_else(|| ExecuteError::InvalidConfig(
            "missing apiBaseUrl (config or HERMES_GATEWAY_BASE_URL env)".to_owned(),
        ))?;
    let api_key = read_string(config, "apiKey")
        .or_else(|| env.get("HERMES_API_KEY").cloned())
        .or_else(|| env.get("HERMES_GATEWAY_KEY").cloned())
        .or_else(|| env.get("HERMES_GATEWAY_TOKEN").cloned())
        .ok_or_else(|| ExecuteError::InvalidConfig("missing apiKey (config or env)".to_owned()))?;

    let strategy_str = read_string(config, "sessionKeyStrategy");
    let session_key_strategy = match strategy_str.as_deref() {
        Some("fixed") | Some("agent") => SessionKeyStrategy::Agent,
        Some("run") => SessionKeyStrategy::Run,
        Some("none") => SessionKeyStrategy::None,
        _ => SessionKeyStrategy::Issue,
    };

    Ok(ExecuteConfig {
        api_base_url,
        api_key,
        model: read_string(config, "model"),
        session_key_strategy,
        poll_interval_ms: read_u64(config, "pollIntervalMs", 1000),
        reconnect_ms: read_u64(config, "reconnectMs", 1000),
        timeout_ms: read_u64(config, "timeoutMs", 300_000),
        max_reconnects: read_u32(config, "maxReconnects", 5),
        workspace: read_string(config, "workspace"),
    })
}

/// 从 `runtime_config.paperclipWake` 提取 issue_id。
pub fn extract_issue_id(wake: Option<&Value>) -> Option<String> {
    let w = wake?;
    read_trimmed(w.get("issueId")).or_else(|| read_trimmed(w.get("taskId")))
}

fn read_trimmed(v: Option<&Value>) -> Option<String> {
    v.and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

// ============================================================================
// HermesExecuteClient trait (mockable transport)
// ============================================================================

/// Hermes gateway execute transport 抽象（POST run + SSE consume 合并）。
///
/// 真实实现用 `DashboardClient` + `HermesSseClient`；测试用 `FakeHermesExecuteClient`。
#[async_trait::async_trait]
pub trait HermesExecuteClient: Send + Sync {
    /// POST /v1/runs 创建 run
    async fn create_run(&self, req: &CreateRunRequest) -> Result<HermesRun, ExecuteError>;

    /// 消费 SSE 事件流直到 terminal。
    /// 收集到的 events 通过 sink 暴露给调用方。
    async fn consume_events(
        &self,
        path: &str,
        sink: &dyn SseEventSink,
        max_reconnects: u32,
    ) -> Result<Vec<SseEvent>, ExecuteError>;

    /// Poll /v1/runs/{id} 直到 terminal。
    async fn poll_until_terminal(
        &self,
        run_id: &str,
        interval_ms: u64,
        timeout_ms: u64,
    ) -> Result<HermesRun, ExecuteError>;
}

pub type DynExecuteClient = Arc<dyn HermesExecuteClient>;

// ============================================================================
// Default execute client (production)
// ============================================================================

/// 默认实现：组合 DashboardClient + HermesSseClient。
pub struct DefaultHermesExecuteClient {
    dashboard: DashboardClient,
    sse: HermesSseClient,
    base_url: String,
}

impl DefaultHermesExecuteClient {
    pub fn new(
        api_base_url: impl Into<String>,
        api_key: impl Into<String>,
        session_key: Option<String>,
    ) -> Self {
        let base_url: String = api_base_url.into();
        let api_key: String = api_key.into();
        Self {
            dashboard: DashboardClient::new(base_url.clone(), api_key.clone(), session_key.clone()),
            sse: HermesSseClient::new(base_url.clone(), api_key, session_key),
            base_url,
        }
    }
}

#[async_trait]
impl HermesExecuteClient for DefaultHermesExecuteClient {
    async fn create_run(&self, req: &CreateRunRequest) -> Result<HermesRun, ExecuteError> {
        self.dashboard
            .create_run(req)
            .await
            .map_err(ExecuteError::Transport)
    }

    async fn consume_events(
        &self,
        path: &str,
        sink: &dyn SseEventSink,
        max_reconnects: u32,
    ) -> Result<Vec<SseEvent>, ExecuteError> {
        let result = self
            .sse
            .consume_until_terminal(path, sink, max_reconnects)
            .await
            .map_err(ExecuteError::Transport)?;
        Ok(result.events)
    }

    async fn poll_until_terminal(
        &self,
        run_id: &str,
        interval_ms: u64,
        timeout_ms: u64,
    ) -> Result<HermesRun, ExecuteError> {
        self.dashboard
            .poll_until_terminal(run_id, interval_ms, timeout_ms)
            .await
            .map_err(ExecuteError::Transport)
    }
}

// ============================================================================
// Scripted Fake Client
// ============================================================================

/// 单次脚本步骤（in-memory 剧本驱动）。
#[derive(Debug, Clone)]
pub enum ScriptedExecuteStep {
    /// create_run 返回
    CreateRun(HermesRun),
    /// SSE event（多次）
    Event(SseEvent),
    /// poll 返回
    Poll(HermesRun),
    /// transport 错误
    Transport(String),
}

/// In-memory fake client（剧本驱动）。
#[derive(Default)]
pub struct FakeHermesExecuteClient {
    pub script: std::sync::Mutex<Vec<ScriptedExecuteStep>>,
    pub created_runs: std::sync::Mutex<Vec<CreateRunRequest>>,
    pub consumed_events: std::sync::Mutex<Vec<SseEvent>>,
    pub polled_runs: std::sync::Mutex<Vec<String>>,
}

impl FakeHermesExecuteClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_script(script: Vec<ScriptedExecuteStep>) -> Self {
        Self {
            script: std::sync::Mutex::new(script),
            created_runs: std::sync::Mutex::new(Vec::new()),
            consumed_events: std::sync::Mutex::new(Vec::new()),
            polled_runs: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl HermesExecuteClient for FakeHermesExecuteClient {
    async fn create_run(&self, req: &CreateRunRequest) -> Result<HermesRun, ExecuteError> {
        self.created_runs
            .lock()
            .expect("created_runs")
            .push(req.clone());
        let mut script = self.script.lock().expect("script");
        if script.is_empty() {
            return Err(ExecuteError::Transport("script empty".to_owned()));
        }
        match script.remove(0) {
            ScriptedExecuteStep::CreateRun(run) => Ok(run),
            ScriptedExecuteStep::Transport(m) => Err(ExecuteError::Transport(m)),
            other => Err(ExecuteError::Transport(format!(
                "unexpected step in create_run: {other:?}"
            ))),
        }
    }

    async fn consume_events(
        &self,
        _path: &str,
        sink: &dyn SseEventSink,
        _max_reconnects: u32,
    ) -> Result<Vec<SseEvent>, ExecuteError> {
        let mut collected = Vec::new();
        let mut script = self.script.lock().expect("script");
        while let Some(step) = script.first() {
            match step {
                ScriptedExecuteStep::Event(_) => {
                    let ScriptedExecuteStep::Event(ev) = script.remove(0) else {
                        unreachable!()
                    };
                    let _ = sink.emit(ev.clone());
                    collected.push(ev);
                }
                _ => break,
            }
        }
        self.consumed_events
            .lock()
            .expect("consumed_events")
            .extend(collected.clone());
        Ok(collected)
    }

    async fn poll_until_terminal(
        &self,
        run_id: &str,
        _interval_ms: u64,
        _timeout_ms: u64,
    ) -> Result<HermesRun, ExecuteError> {
        self.polled_runs
            .lock()
            .expect("polled_runs")
            .push(run_id.to_owned());
        let mut script = self.script.lock().expect("script");
        let index = script.iter().position(|step| {
            matches!(
                step,
                ScriptedExecuteStep::Poll(_) | ScriptedExecuteStep::Transport(_)
            )
        });
        let Some(index) = index else {
            return Err(ExecuteError::Transport(
                "script has no poll step".to_owned(),
            ));
        };
        match script.remove(index) {
            ScriptedExecuteStep::Poll(run) => Ok(run),
            ScriptedExecuteStep::Transport(m) => Err(ExecuteError::Transport(m)),
            other => Err(ExecuteError::Transport(format!(
                "unexpected step in poll: {other:?}"
            ))),
        }
    }
}

// ============================================================================
// Event emission helpers
// ============================================================================

/// Convert SseEvent → AdapterEvent (if event has user-facing text).
pub async fn emit_sse_event(sink: &AdapterEventSink, event: &SseEvent) {
    if let Some(text) = event.extract_text() {
        let _ = sink
            .emit(AdapterEvent::Output {
                stream: OutputStream::Stdout,
                text,
                at: chrono::Utc::now(),
            })
            .await;
    }
}

/// 从 runtime_config 提取 agent / company / issue IDs。
pub fn extract_runtime_ids(runtime_config: &Value) -> (Option<String>, Option<String>) {
    let agent = runtime_config
        .get("agent")
        .and_then(|a| a.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let company = runtime_config
        .get("company")
        .or_else(|| runtime_config.get("agent"))
        .and_then(|a| a.get("companyId"))
        .and_then(|v| v.as_str())
        .map(String::from);
    (agent, company)
}

/// 构造 session key（调用 `constants::build_session_key` 兼容逻辑）。
pub fn build_session_key(
    strategy: SessionKeyStrategy,
    company_id: Option<&str>,
    agent_id: Option<&str>,
    issue_id: Option<&str>,
    run_id: &str,
) -> Option<String> {
    super::build_session_key(strategy, company_id, agent_id, issue_id, run_id)
}

// ============================================================================
// Result builder
// ============================================================================

/// 从终态 run + SSE events 构造 AdapterExecutionResult。
pub fn build_result(
    run: &HermesRun,
    consumed_events: &[SseEvent],
    session_key: Option<&str>,
) -> AdapterExecutionResult {
    let exit_code = if run.status.is_terminal() {
        match run.status {
            RunStatus::Finished => Some(0),
            RunStatus::Error | RunStatus::Cancelled => Some(1),
            _ => Some(0),
        }
    } else {
        Some(0)
    };

    let error_message = match run.status {
        RunStatus::Error | RunStatus::Cancelled => run.error.clone(),
        _ => None,
    };

    let error_code = match run.status {
        RunStatus::Error => Some("hermes_gateway_run_error".to_owned()),
        RunStatus::Cancelled => Some("hermes_gateway_run_cancelled".to_owned()),
        _ => None,
    };

    // Collect text from AgentMessage events
    let accumulated_text: String = consumed_events
        .iter()
        .filter_map(|e| match e {
            SseEvent::AgentMessage { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();

    AdapterExecutionResult {
        exit_code,
        signal: None,
        timed_out: false,
        error_message,
        error_code,
        usage: None,
        session_id: Some(run.run_id.clone()),
        session_params: Some(json!({
            "sessionKey": session_key,
            "hermesRunId": run.run_id,
            "durationMs": run.duration_ms,
            "model": run.model,
        })),
        session_display_id: Some(run.run_id.clone()),
        provider: Some(ADAPTER_TYPE.to_owned()),
        model: run.model.clone(),
        billing_type: Some("hermes_gateway".to_owned()),
        cost_usd: None,
        result_json: Some(run.raw.clone()),
        summary: run.summary.clone().or_else(|| {
            if accumulated_text.is_empty() {
                None
            } else {
                Some(accumulated_text)
            }
        }),
        clear_session: false,
    }
}

// ============================================================================
// Main execute path
// ============================================================================

/// 完整 execute path —— 使用可注入的 HermesExecuteClient。
///
/// 适合 e2e + 单测：`FakeHermesExecuteClient::with_script(...)` 即可剧本驱动。
pub async fn execute_with_client(
    client: DynExecuteClient,
    context: AdapterExecutionContext,
    events: AdapterEventSink,
) -> Result<AdapterExecutionResult, AdapterError> {
    let cfg = parse_execute_config(&context.adapter_config, &context.env)
        .map_err(|e| AdapterError::InvalidConfiguration(e.to_string()))?;

    // Validate apiBaseUrl (loopback / escape hatch + http(s) only)
    let parsed_url = url::Url::parse(&cfg.api_base_url)
        .map_err(|e| AdapterError::InvalidConfiguration(format!("invalid apiBaseUrl: {e}")))?;
    match parsed_url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(AdapterError::InvalidConfiguration(format!(
                "apiBaseUrl scheme {scheme} is not allowed; expected http or https"
            )));
        }
    }
    crate::transport_security::validate_api_base_url(&context.adapter_config, &cfg.api_base_url)
        .map_err(|e| AdapterError::InvalidConfiguration(format!("{e}")))?;

    // Resolve session key
    let wake = context
        .runtime_config
        .get("paperclipWake")
        .cloned()
        .or_else(|| context.runtime_config.get("wake").cloned());
    let issue_id = extract_issue_id(wake.as_ref());
    let (agent_id, company_id) = extract_runtime_ids(&context.runtime_config);
    let run_id_str = context.run_id.to_string();

    let session_key = build_session_key(
        cfg.session_key_strategy,
        company_id.as_deref(),
        agent_id.as_deref(),
        issue_id.as_deref(),
        &run_id_str,
    );

    // Create run
    let create_req = CreateRunRequest {
        prompt: context.prompt.clone(),
        model: cfg.model.clone(),
        session_key: session_key.clone(),
        workspace: cfg.workspace.clone(),
        metadata: Some(json!({
            "runId": run_id_str,
            "agentId": agent_id,
            "companyId": company_id,
        })),
    };

    let run = client
        .create_run(&create_req)
        .await
        .map_err(|e| AdapterError::Process(format!("create_run: {e}")))?;

    // Spawn SSE consumer (background) — drains events, emits AdapterEvent
    let sse_sink = Arc::new(InMemorySseSink::new());
    let sse_sink_clone: Arc<dyn SseEventSink> = sse_sink.clone();

    // Emit text from SSE events
    let events_clone = events.clone();
    let consume_handle = {
        let client = client.clone();
        let run_id = run.run_id.clone();
        tokio::spawn(async move {
            let path = format!("/v1/runs/{run_id}/events");
            let collected = client
                .consume_events(&path, sse_sink_clone.as_ref(), cfg.max_reconnects)
                .await
                .unwrap_or_default();
            for ev in &collected {
                emit_sse_event(&events_clone, ev).await;
            }
        })
    };

    let poll_result = client
        .poll_until_terminal(&run.run_id, cfg.poll_interval_ms, cfg.timeout_ms)
        .await;
    let final_run = match poll_result {
        Ok(run) => run,
        Err(error) => {
            consume_handle.abort();
            return Err(AdapterError::Process(format!("poll: {error}")));
        }
    };

    let _ = tokio::time::timeout(
        Duration::from_millis(
            cfg.reconnect_ms
                .saturating_mul(u64::from(cfg.max_reconnects) + 1)
                .max(1_000),
        ),
        consume_handle,
    )
    .await;

    let collected_events: Vec<SseEvent> = sse_sink.snapshot();

    // Emit session event
    let _ = events
        .emit(AdapterEvent::Session {
            session_id: Some(final_run.run_id.clone()),
            session_params: Some(json!({
                "sessionKey": session_key,
                "hermesRunId": final_run.run_id,
                "durationMs": final_run.duration_ms,
                "model": final_run.model,
            })),
            display_id: Some(final_run.run_id.clone()),
            at: chrono::Utc::now(),
        })
        .await;

    Ok(build_result(
        &final_run,
        &collected_events,
        session_key.as_deref(),
    ))
}

// ============================================================================
// Adapter implementation
// ============================================================================

pub struct HermesGatewayAdapterV2;

impl HermesGatewayAdapterV2 {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HermesGatewayAdapterV2 {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for HermesGatewayAdapterV2 {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin(ADAPTER_TYPE, ADAPTER_LABEL)
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        // Production path: build Default client from context
        let cfg = match parse_execute_config(&context.adapter_config, &context.env) {
            Ok(c) => c,
            Err(e) => return Err(AdapterError::InvalidConfiguration(e.to_string())),
        };
        let wake = context
            .runtime_config
            .get("paperclipWake")
            .cloned()
            .or_else(|| context.runtime_config.get("wake").cloned());
        let (agent_id, company_id) = extract_runtime_ids(&context.runtime_config);
        let issue_id = extract_issue_id(wake.as_ref());
        let session_key = build_session_key(
            cfg.session_key_strategy,
            company_id.as_deref(),
            agent_id.as_deref(),
            issue_id.as_deref(),
            &context.run_id.to_string(),
        );
        let client: DynExecuteClient = Arc::new(DefaultHermesExecuteClient::new(
            cfg.api_base_url,
            cfg.api_key,
            session_key,
        ));
        execute_with_client(client, context, events).await
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::HermesRun;
    use serde_json::json;
    use uuid::Uuid;

    fn sample_run(id: &str, status: RunStatus) -> HermesRun {
        HermesRun {
            run_id: id.to_owned(),
            status,
            summary: None,
            error: None,
            duration_ms: Some(100),
            model: Some("claude-3-5-sonnet".to_owned()),
            raw: json!({"run_id": id, "status": "running"}),
        }
    }

    fn sample_config() -> Value {
        json!({
            "apiBaseUrl": "http://127.0.0.1:8080",
            "apiKey": "test-key",
            "sessionKeyStrategy": "issue",
            "pollIntervalMs": 500,
            "timeoutMs": 5000,
        })
    }

    // ─── parse_execute_config ────────────────────────────────────────────

    #[test]
    fn parse_config_requires_api_base_url() {
        let cfg = json!({});
        let env = std::collections::BTreeMap::new();
        let err = parse_execute_config(&cfg, &env).unwrap_err();
        assert!(matches!(err, ExecuteError::InvalidConfig(_)));
    }

    #[test]
    fn parse_config_requires_api_key() {
        let cfg = json!({"apiBaseUrl": "http://127.0.0.1:8080"});
        let env = std::collections::BTreeMap::new();
        let err = parse_execute_config(&cfg, &env).unwrap_err();
        assert!(matches!(err, ExecuteError::InvalidConfig(_)));
        assert!(err.to_string().contains("apiKey"));
    }

    #[test]
    fn parse_config_picks_up_api_key_from_env() {
        let cfg = json!({"apiBaseUrl": "http://127.0.0.1:8080"});
        let mut env = std::collections::BTreeMap::new();
        env.insert("HERMES_API_KEY".to_owned(), "env-key".to_owned());
        let parsed = parse_execute_config(&cfg, &env).unwrap();
        assert_eq!(parsed.api_key, "env-key");
    }

    #[test]
    fn parse_config_overrides_strategy_and_timeouts() {
        let cfg = json!({
            "apiBaseUrl": "http://x",
            "apiKey": "k",
            "sessionKeyStrategy": "fixed",
            "pollIntervalMs": 2000,
            "reconnectMs": 500,
            "timeoutMs": 60000,
            "maxReconnects": 10,
        });
        let env = std::collections::BTreeMap::new();
        let parsed = parse_execute_config(&cfg, &env).unwrap();
        assert_eq!(parsed.session_key_strategy, SessionKeyStrategy::Agent);
        assert_eq!(parsed.poll_interval_ms, 2000);
        assert_eq!(parsed.max_reconnects, 10);
    }

    #[test]
    fn parse_config_defaults_strategy_to_issue() {
        let cfg = json!({"apiBaseUrl": "http://x", "apiKey": "k"});
        let env = std::collections::BTreeMap::new();
        let parsed = parse_execute_config(&cfg, &env).unwrap();
        assert_eq!(parsed.session_key_strategy, SessionKeyStrategy::Issue);
    }

    // ─── extract_issue_id ───────────────────────────────────────────────

    #[test]
    fn extract_issue_id_picks_issue_then_task() {
        let v = json!({"issueId": "is-1", "taskId": "tk-2"});
        assert_eq!(extract_issue_id(Some(&v)).as_deref(), Some("is-1"));
    }

    #[test]
    fn extract_issue_id_falls_back_to_task() {
        let v = json!({"taskId": "tk-2"});
        assert_eq!(extract_issue_id(Some(&v)).as_deref(), Some("tk-2"));
    }

    #[test]
    fn extract_issue_id_none_when_missing() {
        assert!(extract_issue_id(None).is_none());
        let v = json!({"other": "x"});
        assert!(extract_issue_id(Some(&v)).is_none());
    }

    // ─── build_session_key ──────────────────────────────────────────────

    #[test]
    fn build_session_key_issue_with_all_fields() {
        let sk = build_session_key(
            SessionKeyStrategy::Issue,
            Some("co-1"),
            Some("agent-1"),
            Some("issue-1"),
            "run-1",
        );
        assert_eq!(
            sk.as_deref(),
            Some("paperclip:company:co-1:agent:agent-1:issue:issue-1")
        );
    }

    #[test]
    fn build_session_key_run_only() {
        let sk = build_session_key(SessionKeyStrategy::Run, None, None, None, "run-1");
        assert_eq!(sk.as_deref(), Some("paperclip:run:run-1"));
    }

    #[test]
    fn build_session_key_none_returns_none() {
        let sk = build_session_key(
            SessionKeyStrategy::None,
            Some("co-1"),
            Some("agent-1"),
            Some("issue-1"),
            "run-1",
        );
        assert!(sk.is_none());
    }

    // ─── extract_runtime_ids ────────────────────────────────────────────

    #[test]
    fn extract_runtime_ids_picks_agent_and_company() {
        let runtime = json!({
            "agent": {"id": "ag-1", "companyId": "co-1"},
        });
        let (agent, company) = extract_runtime_ids(&runtime);
        assert_eq!(agent.as_deref(), Some("ag-1"));
        assert_eq!(company.as_deref(), Some("co-1"));
    }

    #[test]
    fn extract_runtime_ids_handles_missing() {
        let runtime = json!({});
        let (agent, company) = extract_runtime_ids(&runtime);
        assert!(agent.is_none());
        assert!(company.is_none());
    }

    // ─── build_result ───────────────────────────────────────────────────

    #[test]
    fn build_result_exit_zero_for_finished() {
        let run = sample_run("r-1", RunStatus::Finished);
        let r = build_result(&run, &[], None);
        assert_eq!(r.exit_code, Some(0));
        assert!(r.error_message.is_none());
    }

    #[test]
    fn build_result_exit_one_for_error() {
        let run = HermesRun {
            run_id: "r-2".to_owned(),
            status: RunStatus::Error,
            summary: None,
            error: Some("boom".to_owned()),
            duration_ms: None,
            model: None,
            raw: json!({}),
        };
        let r = build_result(&run, &[], None);
        assert_eq!(r.exit_code, Some(1));
        assert_eq!(r.error_message.as_deref(), Some("boom"));
        assert!(r.error_code.is_some());
    }

    #[test]
    fn build_result_accumulates_text_from_events() {
        let run = sample_run("r-3", RunStatus::Finished);
        let events = vec![
            SseEvent::AgentMessage {
                text: "hello ".to_owned(),
                delta: true,
            },
            SseEvent::AgentMessage {
                text: "world".to_owned(),
                delta: true,
            },
        ];
        let r = build_result(&run, &events, None);
        assert_eq!(r.summary.as_deref(), Some("hello world"));
    }

    #[test]
    fn build_result_session_id_propagates() {
        let run = sample_run("r-4", RunStatus::Finished);
        let r = build_result(&run, &[], Some("paperclip:run:r-4"));
        assert_eq!(r.session_id.as_deref(), Some("r-4"));
        assert_eq!(r.provider.as_deref(), Some(ADAPTER_TYPE));
        assert_eq!(
            r.session_params.as_ref().unwrap()["sessionKey"],
            "paperclip:run:r-4"
        );
    }

    // ─── execute_with_client e2e ────────────────────────────────────────

    #[tokio::test]
    async fn full_execute_happy_path_emits_session_event() {
        let run = sample_run("r-1", RunStatus::Finished);
        let script = vec![
            ScriptedExecuteStep::CreateRun(run.clone()),
            ScriptedExecuteStep::Event(SseEvent::AgentMessage {
                text: "hello ".to_owned(),
                delta: true,
            }),
            ScriptedExecuteStep::Event(SseEvent::AgentMessage {
                text: "world".to_owned(),
                delta: true,
            }),
            ScriptedExecuteStep::Poll(sample_run("r-1", RunStatus::Finished)),
        ];
        let client: DynExecuteClient = Arc::new(FakeHermesExecuteClient::with_script(script));
        let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "hi");
        ctx.adapter_config = sample_config();
        ctx.runtime_config = json!({
            "agent": {"id": "ag-1", "companyId": "co-1"},
            "paperclipWake": {"issueId": "is-1"},
        });
        let (sink, mut rx) = AdapterEventSink::channel(16);
        let result = execute_with_client(client, ctx, sink)
            .await
            .expect("execute ok");

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.session_id.as_deref(), Some("r-1"));
        assert_eq!(result.summary.as_deref(), Some("hello world"));

        // Verify session event emitted
        let mut saw_session = false;
        let mut saw_stdout = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AdapterEvent::Session { session_id, .. } => {
                    saw_session = session_id.is_some();
                }
                AdapterEvent::Output {
                    stream: OutputStream::Stdout,
                    text,
                    ..
                } => {
                    saw_stdout = saw_stdout || text.contains("hello");
                }
                _ => {}
            }
        }
        assert!(saw_session);
        assert!(saw_stdout);
    }

    #[tokio::test]
    async fn full_execute_error_run_returns_exit_one() {
        let run = HermesRun {
            run_id: "r-err".to_owned(),
            status: RunStatus::Error,
            summary: None,
            error: Some("rate limited".to_owned()),
            duration_ms: Some(50),
            model: None,
            raw: json!({}),
        };
        let script = vec![
            ScriptedExecuteStep::CreateRun(run.clone()),
            ScriptedExecuteStep::Poll(run),
        ];
        let client: DynExecuteClient = Arc::new(FakeHermesExecuteClient::with_script(script));
        let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "hi");
        ctx.adapter_config = sample_config();
        ctx.runtime_config = json!({});
        let (sink, _rx) = AdapterEventSink::channel(8);
        let result = execute_with_client(client, ctx, sink)
            .await
            .expect("execute ok");
        assert_eq!(result.exit_code, Some(1));
        assert_eq!(result.error_message.as_deref(), Some("rate limited"));
    }

    #[tokio::test]
    async fn full_execute_create_run_transport_error() {
        let script = vec![ScriptedExecuteStep::Transport(
            "connection refused".to_owned(),
        )];
        let client: DynExecuteClient = Arc::new(FakeHermesExecuteClient::with_script(script));
        let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "hi");
        ctx.adapter_config = sample_config();
        ctx.runtime_config = json!({});
        let (sink, _rx) = AdapterEventSink::channel(8);
        let err = execute_with_client(client, ctx, sink).await.unwrap_err();
        match err {
            AdapterError::Process(m) => assert!(m.contains("create_run")),
            other => panic!("expected Process error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn full_execute_missing_api_key_returns_config_error() {
        let client: DynExecuteClient = Arc::new(FakeHermesExecuteClient::new());
        let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "hi");
        ctx.adapter_config = json!({"apiBaseUrl": "http://127.0.0.1:8080"});
        ctx.runtime_config = json!({});
        let (sink, _rx) = AdapterEventSink::channel(8);
        let err = execute_with_client(client, ctx, sink).await.unwrap_err();
        assert!(matches!(err, AdapterError::InvalidConfiguration(_)));
    }

    #[tokio::test]
    async fn full_execute_invalid_base_url_returns_config_error() {
        let client: DynExecuteClient = Arc::new(FakeHermesExecuteClient::new());
        let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "hi");
        // ftp:// not allowed
        ctx.adapter_config = json!({
            "apiBaseUrl": "ftp://example.com",
            "apiKey": "k",
        });
        ctx.runtime_config = json!({});
        let (sink, _rx) = AdapterEventSink::channel(8);
        let err = execute_with_client(client, ctx, sink).await.unwrap_err();
        assert!(matches!(err, AdapterError::InvalidConfiguration(_)));
    }

    #[tokio::test]
    async fn full_execute_poll_transport_error() {
        let run = sample_run("r-x", RunStatus::Running);
        let script = vec![
            ScriptedExecuteStep::CreateRun(run.clone()),
            ScriptedExecuteStep::Transport("server gone".to_owned()),
        ];
        let client: DynExecuteClient = Arc::new(FakeHermesExecuteClient::with_script(script));
        let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "hi");
        ctx.adapter_config = sample_config();
        ctx.runtime_config = json!({});
        let (sink, _rx) = AdapterEventSink::channel(8);
        let err = execute_with_client(client, ctx, sink).await.unwrap_err();
        match err {
            AdapterError::Process(m) => assert!(m.contains("poll")),
            other => panic!("expected Process error, got {other:?}"),
        }
    }

    // ─── HermesGatewayAdapterV2 ─────────────────────────────────────────

    #[test]
    fn adapter_v2_descriptor_uses_canonical_label() {
        let a = HermesGatewayAdapterV2::new();
        let d = a.descriptor();
        assert_eq!(d.adapter_type, ADAPTER_TYPE);
        assert_eq!(d.label, ADAPTER_LABEL);
    }
}
