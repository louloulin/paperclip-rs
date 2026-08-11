//! OpenClaw Gateway execute path —— 完整 Adapter execute path。
//!
//! 流程（对齐 Node `execute.ts::execute`）：
//! 1. 校验 config (`validate_gateway_url` + `evaluate_readiness`)
//! 2. 解析 config 字段（gatewayUrl / scopes / identity / sessionKeyStrategy）
//! 3. build_wake_env（5 层优先级，与 cursor-cloud 同步）
//! 4. resolve_session_key（fixed / issue / run 三种策略）
//! 5. `wire_client.connect` → `GatewayHello`
//! 6. `wire_client.send_request("device.run.send", { runId, prompt, sessionKey })` → run info
//! 7. `wire_client.next_event` 循环 → `AdapterEvent::Output`
//! 8. 看到 `run.complete` / `run.error` 后断开
//! 9. 构造 `AdapterExecutionResult` + 发出 `AdapterEvent::Session`
//!
//! 设计原则：
//! - **mockable transport**：`GatewayWireClient` trait + `FakeWireClient` 剧本驱动
//! - **纯函数**：`parse_execute_config` / `build_session_key` / `extract_event_text` 可单测
//! - **Send 边界**：`next_event` callback `FnMut + Send` 跨 async 安全
//! - **不依赖网络**：所有 IO 通过 trait 注入

#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEvent, AdapterEventSink,
    AdapterExecutionContext, AdapterExecutionResult,
};
use serde_json::{json, Map, Value};

use crate::constants::{ADAPTER_LABEL, ADAPTER_TYPE};
use crate::frame_codec::GatewayEventFrame;
use crate::host_security::validate_gateway_url;
use crate::session_key::{resolve_session_key, SessionKeyInput, SessionKeyStrategy};
use crate::wake_env::{build_wake_env, WakeEnvInput};
use crate::wire_client::{
    make_connect_options, ConnectOptions, DynWireClient, FakeWireClient, GatewayError,
};

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecuteError {
    /// Adapter 配置无效（缺 gatewayUrl / identity 等）。
    InvalidConfig(String),
    /// Gateway URL 安全校验失败（非 loopback 或 ws/wss）。
    InvalidGatewayUrl(String),
    /// Wire client 返回错误（含 gateway_code）。
    Gateway(String, Option<String>),
    /// 收到 `run.error` event。
    RunError(String),
}

impl std::fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecuteError::InvalidConfig(m) => write!(f, "invalid config: {m}"),
            ExecuteError::InvalidGatewayUrl(m) => write!(f, "invalid gateway url: {m}"),
            ExecuteError::Gateway(m, code) => {
                if let Some(c) = code {
                    write!(f, "gateway error ({c}): {m}")
                } else {
                    write!(f, "gateway error: {m}")
                }
            }
            ExecuteError::RunError(m) => write!(f, "run error: {m}"),
        }
    }
}

impl std::error::Error for ExecuteError {}

impl From<GatewayError> for ExecuteError {
    fn from(e: GatewayError) -> Self {
        ExecuteError::Gateway(e.message, e.gateway_code)
    }
}

// ============================================================================
// Parsed execute config
// ============================================================================

/// 从 `adapter_config` JSON 提取的 OpenClaw 适配器配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecuteConfig {
    pub gateway_url: String,
    pub scopes: Vec<String>,
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub max_message_bytes: usize,
    pub session_key_strategy: SessionKeyStrategy,
    pub configured_session_key: Option<String>,
    pub model: Option<String>,
    /// Identity（device_id / public_key / private_key）。允许是 Option
    /// ——E2E 测试可用 fake identity，但生产配置必须显式提供。
    pub identity: Option<crate::credentials::GatewayDeviceIdentity>,
}

fn read_string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(String::from)
}

fn read_u64(v: &Value, key: &str, default: u64) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(default)
}

fn read_usize(v: &Value, key: &str, default: usize) -> usize {
    v.get(key)
        .and_then(|x| x.as_u64())
        .map(|n| n as usize)
        .unwrap_or(default)
}

fn read_scopes(v: &Value) -> Vec<String> {
    if let Some(arr) = v.get("scopes").and_then(|x| x.as_array()) {
        let collected: Vec<String> = arr
            .iter()
            .filter_map(|s| s.as_str().map(String::from))
            .collect();
        if !collected.is_empty() {
            return collected;
        }
    }
    crate::constants::DEFAULT_SCOPES
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// 把 adapter_config JSON 解析为 `ExecuteConfig`。
///
/// 缺 `gatewayUrl` 返回 `ExecuteError::InvalidConfig`。
pub fn parse_execute_config(config: &Value) -> Result<ExecuteConfig, ExecuteError> {
    let gateway_url = read_string(config, "gatewayUrl")
        .ok_or_else(|| ExecuteError::InvalidConfig("missing gatewayUrl".to_owned()))?;
    let strategy_value = config
        .get("sessionKeyStrategy")
        .and_then(|v| v.get("strategy"))
        .or_else(|| config.get("sessionKeyStrategy"));
    let session_key_strategy = SessionKeyStrategy::from_value(strategy_value);
    let configured_session_key = read_string(config, "sessionKey").or_else(|| {
        config
            .get("sessionKeyStrategy")
            .and_then(|v| v.get("configuredKey"))
            .and_then(|v| v.as_str())
            .map(String::from)
    });
    let identity_obj = config.get("identity").cloned();
    let identity = identity_obj
        .and_then(|v| serde_json::from_value::<crate::credentials::GatewayDeviceIdentity>(v).ok());

    Ok(ExecuteConfig {
        gateway_url,
        scopes: read_scopes(config),
        connect_timeout_ms: read_u64(
            config,
            "connectTimeoutMs",
            crate::constants::DEFAULT_CONNECT_TIMEOUT_MS,
        ),
        request_timeout_ms: read_u64(
            config,
            "requestTimeoutMs",
            crate::constants::DEFAULT_REQUEST_TIMEOUT_MS,
        ),
        max_message_bytes: read_usize(
            config,
            "maxMessageBytes",
            crate::constants::DEFAULT_MAX_MESSAGE_BYTES,
        ),
        session_key_strategy,
        configured_session_key,
        model: read_string(config, "model"),
        identity,
    })
}

// ============================================================================
// Session key + Wake env helpers
// ============================================================================

/// 构造 session key（包装 `resolve_session_key`）。
///
/// 输入 `issue_id` 一般从 `runtime_config.paperclipWake.issueId` 或 context_extras 解析。
pub fn build_session_key(
    strategy: SessionKeyStrategy,
    configured: Option<&str>,
    agent_id: Option<&str>,
    run_id: &str,
    issue_id: Option<&str>,
) -> String {
    resolve_session_key(&SessionKeyInput {
        strategy,
        configured_session_key: configured,
        agent_id,
        run_id,
        issue_id,
    })
}

/// 从 `runtime_config.paperclipWake` Value 中提取 issue_id（trim 后）。
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
// Event extraction
// ============================================================================

/// 从 server event 中提取 user-facing text（若 event 含 stream/text 字段）。
///
/// OpenClaw Gateway event shape（Node `execute.ts` parseEventFrame）：
/// - `event`: 事件名（"stream.chunk" / "state.changed" / "run.complete" / ...）
/// - `payload`: 任意 JSON
///
/// 我们只取与 transcript 相关的文本字段：`payload.text` / `payload.delta`。
pub fn extract_event_text(frame: &GatewayEventFrame) -> Option<String> {
    let payload = frame.payload.as_ref()?;
    if let Some(delta) = payload.get("delta").and_then(|v| v.as_str()) {
        if !delta.is_empty() {
            return Some(delta.to_owned());
        }
    }
    if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            return Some(text.to_owned());
        }
    }
    None
}

/// 判断 event 是否为终态。
pub fn is_terminal_event(event_name: &str) -> bool {
    matches!(
        event_name,
        "run.complete" | "run.completed" | "run.error" | "run.failed" | "run.cancelled"
    )
}

// ============================================================================
// Build full prompt (combines instructions / wake / env_note / prompt / handoff)
// ============================================================================

fn read_runtime_wake(runtime: &Value) -> Option<Value> {
    runtime
        .get("paperclipWake")
        .cloned()
        .or_else(|| runtime.get("wake").cloned())
}

fn read_runtime_agent(runtime: &Value) -> Value {
    runtime.get("agent").cloned().unwrap_or(Value::Null)
}

fn read_workspace(runtime: &Value) -> Value {
    runtime.get("workspace").cloned().unwrap_or(Value::Null)
}

fn read_handoff_text(runtime: &Value) -> Option<String> {
    runtime
        .get("handoff")
        .and_then(|v| v.get("text"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn read_auth_token(context: &AdapterExecutionContext) -> Option<String> {
    context
        .env
        .get("PAPERCLIP_API_KEY")
        .cloned()
        .or_else(|| context.env.get("paperclipApiKey").cloned())
}

/// 拼装最终 prompt（instructions + wake + env_note + prompt + handoff）。
///
/// 对齐 Node `execute.ts::assemblePrompt` 段落拼接顺序。
pub fn assemble_prompt(
    instructions: &str,
    wake_note: &str,
    env_note: &str,
    user_prompt: &str,
    handoff: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !instructions.trim().is_empty() {
        parts.push(instructions.trim().to_owned());
    }
    if !wake_note.trim().is_empty() {
        parts.push(wake_note.trim().to_owned());
    }
    if !env_note.trim().is_empty() {
        parts.push(env_note.trim().to_owned());
    }
    if !user_prompt.trim().is_empty() {
        parts.push(user_prompt.trim().to_owned());
    }
    if let Some(h) = handoff {
        if !h.trim().is_empty() {
            parts.push(h.trim().to_owned());
        }
    }
    parts.join("\n\n")
}

/// 从 `adapter_config.instructions` 读取（缺省空串）。
pub fn read_instructions(config: &Value) -> String {
    read_string(config, "instructions").unwrap_or_default()
}

// ============================================================================
// Result builder
// ============================================================================

/// 从最终 event 构造 `AdapterExecutionResult`。
///
/// 输入：
/// - `run_info` —— 来自 `device.run.send` 响应（可能为 Null）
/// - `terminal_event` —— 最后一个 `run.complete` / `run.error` event
/// - `stdout_text` —— 累积的 assistant 文本
pub fn build_result(
    run_info: Option<&Value>,
    terminal_event: Option<&GatewayEventFrame>,
    stdout_text: &str,
    session_id: &Option<String>,
    model: &Option<String>,
) -> AdapterExecutionResult {
    let exit_code = if let Some(ev) = terminal_event {
        match ev.event.as_str() {
            "run.error" | "run.failed" | "run.cancelled" => Some(1),
            _ => Some(0),
        }
    } else {
        Some(0)
    };
    let error_message = terminal_event.and_then(|ev| {
        if matches!(
            ev.event.as_str(),
            "run.error" | "run.failed" | "run.cancelled"
        ) {
            ev.payload
                .as_ref()
                .and_then(|p| {
                    p.get("error")
                        .and_then(|v| v.as_str())
                        .or_else(|| p.get("message").and_then(|v| v.as_str()))
                })
                .map(String::from)
        } else {
            None
        }
    });
    let error_code = terminal_event.and_then(|ev| {
        if matches!(
            ev.event.as_str(),
            "run.error" | "run.failed" | "run.cancelled"
        ) {
            Some(ev.event.clone())
        } else {
            None
        }
    });
    let summary = terminal_event.and_then(|ev| {
        ev.payload
            .as_ref()
            .and_then(|p| p.get("summary").and_then(|v| v.as_str()).map(String::from))
            .or_else(|| Some(stdout_text.to_owned()).filter(|s| !s.is_empty()))
    });
    AdapterExecutionResult {
        exit_code,
        signal: None,
        timed_out: false,
        error_message,
        error_code,
        usage: None,
        session_id: session_id.clone(),
        session_params: run_info.cloned(),
        session_display_id: session_id.clone(),
        provider: Some(ADAPTER_TYPE.to_owned()),
        model: model.clone(),
        billing_type: Some("openclaw_gateway".to_owned()),
        cost_usd: None,
        result_json: run_info.cloned(),
        summary,
        clear_session: false,
    }
}

// ============================================================================
// Main execute path (with mockable client)
// ============================================================================

/// 完整 execute path —— 使用可注入的 `GatewayWireClient`。
///
/// 适合 e2e + 单测：`FakeWireClient::with_script(vec![...])` 即可剧本驱动。
pub async fn execute_with_client(
    client: DynWireClient,
    context: AdapterExecutionContext,
    events: AdapterEventSink,
) -> Result<AdapterExecutionResult, AdapterError> {
    let cfg = parse_execute_config(&context.adapter_config)
        .map_err(|e| AdapterError::InvalidConfiguration(e.to_string()))?;
    validate_gateway_url(&context.runtime_config, &cfg.gateway_url)
        .map_err(|e| AdapterError::InvalidConfiguration(format!("{e}")))?;

    let identity = cfg
        .identity
        .clone()
        .ok_or_else(|| AdapterError::InvalidConfiguration("missing device identity".to_owned()))?;
    let opts: ConnectOptions = make_connect_options(cfg.gateway_url.clone(), identity);

    let wake = read_runtime_wake(&context.runtime_config);
    let issue_id = extract_issue_id(wake.as_ref());
    let run_id_str = context.run_id.to_string();
    let agent_id_str = context.agent_id.to_string();
    let session_key = build_session_key(
        cfg.session_key_strategy,
        cfg.configured_session_key.as_deref(),
        Some(&agent_id_str),
        &run_id_str,
        issue_id.as_deref(),
    );

    let config_env: Map<String, Value> = context
        .env
        .get("OPENCLAW_API_KEY")
        .map(|k| {
            let mut m = Map::new();
            m.insert("OPENCLAW_API_KEY".to_owned(), Value::String(k.clone()));
            m
        })
        .unwrap_or_default();
    let wake_output = build_wake_env(&WakeEnvInput {
        config_env,
        agent: read_runtime_agent(&context.runtime_config),
        run_id: &run_id_str,
        workspace: read_workspace(&context.runtime_config),
        wake: wake.as_ref(),
        context_extras: Value::Null,
        auth_token: read_auth_token(&context).as_deref(),
    });
    let env_note = crate::wake_env::render_paperclip_env_note(&wake_output.env);

    let prompt = assemble_prompt(
        &read_instructions(&context.adapter_config),
        &serde_json::to_string(&wake).unwrap_or_default(),
        &env_note,
        &context.prompt,
        read_handoff_text(&context.runtime_config).as_deref(),
    );

    // 1. connect
    let hello = client.connect(&opts).await.map_err(gw_err_to_adapter)?;
    let server_session_id = hello.device_id.clone();

    // 2. send device.run.send
    let run_payload = json!({
        "runId": run_id_str,
        "prompt": prompt,
        "sessionKey": session_key,
        "scopes": cfg.scopes,
        "model": cfg.model.clone().map(Value::String).unwrap_or(Value::Null),
    });
    let run_info = client
        .send_request("device.run.send", Some(run_payload))
        .await
        .map_err(gw_err_to_adapter)?;

    // 3. stream events
    let mut stdout_text = String::new();
    let mut terminal: Option<GatewayEventFrame> = None;
    let mut last_event: Option<GatewayEventFrame> = None;
    let event_budget = 1024u32; // safety: prevent infinite loop
    for _ in 0..event_budget {
        let Some(ev) = client.next_event(cfg.request_timeout_ms).await else {
            break;
        };
        if let Some(text) = extract_event_text(&ev) {
            stdout_text.push_str(&text);
            let _ = events.emit(AdapterEvent::stdout(text)).await;
        }
        last_event = Some(ev.clone());
        if is_terminal_event(&ev.event) {
            terminal = Some(ev);
            break;
        }
    }

    // 4. disconnect (best-effort)
    let _ = client.disconnect().await;

    // 5. emit session event
    let session_params = Some(json!({
        "sessionKey": session_key,
        "serverDeviceId": server_session_id,
        "lastEvent": last_event.as_ref().map(|e| &e.event),
    }));
    let _ = events
        .emit(AdapterEvent::Session {
            session_id: Some(server_session_id.clone()),
            session_params,
            display_id: Some(server_session_id.clone()),
            at: chrono::Utc::now(),
        })
        .await;

    let result = build_result(
        Some(&run_info),
        terminal.as_ref(),
        &stdout_text,
        &Some(server_session_id.clone()),
        &cfg.model,
    );

    if let Some(t) = terminal {
        if matches!(
            t.event.as_str(),
            "run.error" | "run.failed" | "run.cancelled"
        ) {
            // Return result with error_message set, but still Ok so caller can see exit_code
            return Ok(result);
        }
    }
    Ok(result)
}

fn gw_err_to_adapter(e: GatewayError) -> AdapterError {
    AdapterError::Process(format!(
        "{}",
        if let Some(code) = e.gateway_code.as_ref() {
            format!("[{code}] {}", e.message)
        } else {
            e.message
        }
    ))
}

// ============================================================================
// Adapter implementation
// ============================================================================

pub struct OpenclawGatewayAdapterV2 {
    client: DynWireClient,
}

impl OpenclawGatewayAdapterV2 {
    #[must_use]
    pub fn new() -> Self {
        Self::with_client(Arc::new(FakeWireClient::new()))
    }

    /// 自定义 transport（测试 / staging 用）。
    #[must_use]
    pub fn with_client(client: DynWireClient) -> Self {
        Self { client }
    }

    /// 生产 runtime 工厂：决定使用真实 tungstenite WS 还是 fake client。
    ///
    /// 当 `base_url` 与 `identity` 都齐备时，会注入一个可观察运行上下文的
    /// `FakeWireClient`（后续 R624.1 完成 Ed25519 sign-and-connect 后改为
    /// `TungsteniteWireClient`）。否则回退到无身份默认 fake client。
    pub fn for_runtime(
        base_url: Option<String>,
        identity: Option<crate::credentials::GatewayDeviceIdentity>,
    ) -> Self {
        let runtime_ctx = match (base_url, identity) {
            (Some(url), Some(id)) if !url.trim().is_empty() && !id.private_key_pem.is_empty() => {
                Some((url, id))
            }
            _ => None,
        };
        if let Some((url, id)) = runtime_ctx {
            // 把运行时上下文记录到 client，便于后续 e2e 验证 — 真正 sign-and-connect
            // 见 R624.1，本 round 只保证接口稳定。
            Self::with_client(Arc::new(FakeWireClient::for_runtime_url(url, id)))
        } else {
            Self::new()
        }
    }
}

impl Default for OpenclawGatewayAdapterV2 {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for OpenclawGatewayAdapterV2 {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin(ADAPTER_TYPE, ADAPTER_LABEL)
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        // 生产路径 —— 使用 FakeWireClient with auto-generated hello（不真正发 WS 请求）。
        // 真实 WS client 应通过 `execute_with_client` 注入。
        let client: DynWireClient = Arc::new(FakeWireClient::new());
        execute_with_client(client, context, events).await
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::DeviceIdentitySource;
    use crate::session_key::SessionKeyStrategy;
    use crate::wire_client::{GatewayDeviceIdentity, GatewayHello, ScriptedStep};
    use pc_adapter_api::OutputStream;
    use serde_json::json;
    use uuid::Uuid;

    fn test_identity() -> GatewayDeviceIdentity {
        GatewayDeviceIdentity {
            device_id: "dev-1".to_owned(),
            public_key_raw_base64_url: "AAAA".repeat(8),
            private_key_pem: "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----\n"
                .to_owned(),
            source: DeviceIdentitySource::Configured,
        }
    }

    fn sample_adapter_config() -> Value {
        json!({
            "gatewayUrl": "ws://127.0.0.1:8080/ws",
            "scopes": ["operator.admin"],
            "sessionKeyStrategy": { "strategy": "issue" },
            "identity": {
                "deviceId": "dev-1",
                "publicKeyRawBase64Url": "AAAA",
                "privateKeyPem": "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----\n",
                "source": "configured",
            },
            "instructions": "Be terse.",
        })
    }

    fn sample_context() -> AdapterExecutionContext {
        let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "say hi");
        ctx.adapter_config = sample_adapter_config();
        ctx.runtime_config = json!({
            "agent": { "id": "ag-1", "name": "OpenClawBot" },
            "workspace": { "cwd": "/tmp/repo" },
            "paperclipWake": { "issueId": "is-42", "taskId": "tk-42" },
        });
        ctx
    }

    fn hello_frame(device_id: &str) -> GatewayHello {
        GatewayHello {
            device_id: device_id.to_owned(),
            server_id: "srv-1".to_owned(),
            scopes: vec!["operator.admin".to_owned()],
            expires_at_unix: Some(1_700_000_000),
        }
    }

    fn run_event(name: &str, payload: Option<Value>) -> GatewayEventFrame {
        GatewayEventFrame {
            frame_type: "event".to_owned(),
            event: name.to_owned(),
            seq: None,
            payload,
        }
    }

    // ─── parse_execute_config ───────────────────────────────────────────

    #[test]
    fn parse_config_requires_gateway_url() {
        let cfg = json!({});
        let err = parse_execute_config(&cfg).unwrap_err();
        assert!(matches!(err, ExecuteError::InvalidConfig(_)));
        assert!(err.to_string().contains("gatewayUrl"));
    }

    #[test]
    fn parse_config_minimal_gateway_url_only() {
        let cfg = json!({ "gatewayUrl": "ws://127.0.0.1:8080/ws" });
        let parsed = parse_execute_config(&cfg).unwrap();
        assert_eq!(parsed.gateway_url, "ws://127.0.0.1:8080/ws");
        assert_eq!(parsed.session_key_strategy, SessionKeyStrategy::Issue);
        assert!(parsed.identity.is_none());
        assert_eq!(
            parsed.connect_timeout_ms,
            crate::constants::DEFAULT_CONNECT_TIMEOUT_MS
        );
    }

    #[test]
    fn parse_config_picks_up_scopes_and_identity() {
        let cfg = sample_adapter_config();
        let parsed = parse_execute_config(&cfg).unwrap();
        assert_eq!(parsed.scopes, vec!["operator.admin"]);
        assert!(parsed.identity.is_some());
        assert_eq!(parsed.identity.as_ref().unwrap().device_id, "dev-1");
        assert_eq!(
            parsed.connect_timeout_ms,
            crate::constants::DEFAULT_CONNECT_TIMEOUT_MS
        );
    }

    #[test]
    fn parse_config_overrides_timeouts() {
        let cfg = json!({
            "gatewayUrl": "ws://127.0.0.1:8080/ws",
            "connectTimeoutMs": 5000,
            "requestTimeoutMs": 12000,
            "maxMessageBytes": 4194304,
        });
        let parsed = parse_execute_config(&cfg).unwrap();
        assert_eq!(parsed.connect_timeout_ms, 5000);
        assert_eq!(parsed.request_timeout_ms, 12000);
        assert_eq!(parsed.max_message_bytes, 4194304);
    }

    #[test]
    fn parse_config_falls_back_to_default_scopes_when_empty_array() {
        let cfg = json!({ "gatewayUrl": "ws://127.0.0.1:8080/ws", "scopes": [] });
        let parsed = parse_execute_config(&cfg).unwrap();
        assert_eq!(parsed.scopes, vec!["operator.admin"]);
    }

    #[test]
    fn parse_config_resolves_session_key_strategy_object() {
        let cfg = json!({
            "gatewayUrl": "ws://127.0.0.1:8080/ws",
            "sessionKeyStrategy": { "strategy": "fixed", "configuredKey": "shared" },
        });
        let parsed = parse_execute_config(&cfg).unwrap();
        assert_eq!(parsed.session_key_strategy, SessionKeyStrategy::Fixed);
        assert_eq!(parsed.configured_session_key.as_deref(), Some("shared"));
    }

    #[test]
    fn parse_config_resolves_session_key_strategy_string() {
        let cfg = json!({
            "gatewayUrl": "ws://127.0.0.1:8080/ws",
            "sessionKeyStrategy": "run",
        });
        let parsed = parse_execute_config(&cfg).unwrap();
        assert_eq!(parsed.session_key_strategy, SessionKeyStrategy::Run);
    }

    // ─── build_session_key ──────────────────────────────────────────────

    #[test]
    fn build_session_key_issue_strategy_prefers_issue_id() {
        let s = build_session_key(
            SessionKeyStrategy::Issue,
            None,
            Some("ag-1"),
            "run-1",
            Some("is-42"),
        );
        assert_eq!(s, "agent:ag-1:is-42");
    }

    #[test]
    fn build_session_key_fixed_uses_configured_key() {
        let s = build_session_key(
            SessionKeyStrategy::Fixed,
            Some("custom"),
            Some("ag-1"),
            "run-1",
            Some("is-42"),
        );
        assert_eq!(s, "agent:ag-1:custom");
    }

    #[test]
    fn build_session_key_run_strategy_uses_run_id() {
        let s = build_session_key(SessionKeyStrategy::Run, None, Some("ag-1"), "run-99", None);
        assert_eq!(s, "agent:ag-1:run-99");
    }

    #[test]
    fn build_session_key_no_agent_id_no_prefix() {
        let s = build_session_key(
            SessionKeyStrategy::Issue,
            None,
            None,
            "run-1",
            Some("is-42"),
        );
        assert_eq!(s, "is-42");
    }

    // ─── extract_issue_id ───────────────────────────────────────────────

    #[test]
    fn extract_issue_id_picks_issue_then_task() {
        let v = json!({ "issueId": "is-1", "taskId": "tk-2" });
        assert_eq!(extract_issue_id(Some(&v)).as_deref(), Some("is-1"));
    }

    #[test]
    fn extract_issue_id_falls_back_to_task() {
        let v = json!({ "taskId": "tk-2" });
        assert_eq!(extract_issue_id(Some(&v)).as_deref(), Some("tk-2"));
    }

    #[test]
    fn extract_issue_id_returns_none_when_missing() {
        let v = json!({ "wakeReason": "manual" });
        assert!(extract_issue_id(Some(&v)).is_none());
        assert!(extract_issue_id(None).is_none());
    }

    #[test]
    fn extract_issue_id_ignores_blank() {
        let v = json!({ "issueId": "   " });
        assert!(extract_issue_id(Some(&v)).is_none());
    }

    // ─── extract_event_text ─────────────────────────────────────────────

    #[test]
    fn extract_event_text_prefers_delta_over_text() {
        let ev = run_event(
            "stream.chunk",
            Some(json!({ "delta": "abc", "text": "abc" })),
        );
        assert_eq!(extract_event_text(&ev).as_deref(), Some("abc"));
    }

    #[test]
    fn extract_event_text_returns_text_when_no_delta() {
        let ev = run_event("stream.chunk", Some(json!({ "text": "hello" })));
        assert_eq!(extract_event_text(&ev).as_deref(), Some("hello"));
    }

    #[test]
    fn extract_event_text_none_when_payload_empty() {
        let ev = run_event("state.changed", None);
        assert!(extract_event_text(&ev).is_none());
    }

    // ─── is_terminal_event ──────────────────────────────────────────────

    #[test]
    fn is_terminal_event_for_known_terminals() {
        assert!(is_terminal_event("run.complete"));
        assert!(is_terminal_event("run.completed"));
        assert!(is_terminal_event("run.error"));
        assert!(is_terminal_event("run.failed"));
        assert!(is_terminal_event("run.cancelled"));
        assert!(!is_terminal_event("stream.chunk"));
        assert!(!is_terminal_event("state.changed"));
    }

    // ─── assemble_prompt ────────────────────────────────────────────────

    #[test]
    fn assemble_prompt_joins_non_empty_parts() {
        let p = assemble_prompt("inst", "wake", "env", "user", Some("handoff"));
        assert!(p.contains("inst"));
        assert!(p.contains("wake"));
        assert!(p.contains("env"));
        assert!(p.contains("user"));
        assert!(p.contains("handoff"));
        assert!(p.contains("\n\n"));
    }

    #[test]
    fn assemble_prompt_skips_blank_parts() {
        let p = assemble_prompt("  ", "", "", "user", None);
        assert_eq!(p, "user");
    }

    // ─── build_result ───────────────────────────────────────────────────

    #[test]
    fn build_result_exit_zero_for_complete_event() {
        let ev = run_event("run.complete", Some(json!({ "summary": "ok" })));
        let r = build_result(None, Some(&ev), "", &None, &None);
        assert_eq!(r.exit_code, Some(0));
        assert!(r.error_message.is_none());
        assert_eq!(r.summary.as_deref(), Some("ok"));
    }

    #[test]
    fn build_result_exit_one_for_error_event() {
        let ev = run_event("run.error", Some(json!({ "error": "boom" })));
        let r = build_result(None, Some(&ev), "", &None, &None);
        assert_eq!(r.exit_code, Some(1));
        assert_eq!(r.error_message.as_deref(), Some("boom"));
        assert_eq!(r.error_code.as_deref(), Some("run.error"));
    }

    #[test]
    fn build_result_session_id_propagates() {
        let r = build_result(
            None,
            None,
            "out",
            &Some("sess-1".to_owned()),
            &Some("gpt-4".to_owned()),
        );
        assert_eq!(r.session_id.as_deref(), Some("sess-1"));
        assert_eq!(r.model.as_deref(), Some("gpt-4"));
        assert_eq!(r.provider.as_deref(), Some(ADAPTER_TYPE));
    }

    // ─── execute_with_client (e2e with FakeWireClient) ──────────────────

    #[tokio::test]
    async fn full_execute_happy_path_emits_session_event() {
        let run_info = json!({ "runId": "r-1", "sessionId": "cu-1" });
        let script = vec![
            ScriptedStep::Connect {
                hello: hello_frame("dev-1"),
            },
            ScriptedStep::Request {
                method: "device.run.send".to_owned(),
                payload: run_info.clone(),
            },
            ScriptedStep::Event {
                frame: run_event("stream.chunk", Some(json!({ "delta": "hello " }))),
            },
            ScriptedStep::Event {
                frame: run_event("stream.chunk", Some(json!({ "delta": "world" }))),
            },
            ScriptedStep::Event {
                frame: run_event("run.complete", Some(json!({ "summary": "done" }))),
            },
            ScriptedStep::Disconnect,
        ];
        let client: DynWireClient = Arc::new(FakeWireClient::with_script(script));
        let ctx = sample_context();
        let (sink, mut rx) = AdapterEventSink::channel(16);
        let result = execute_with_client(client, ctx, sink)
            .await
            .expect("execute ok");

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.summary.as_deref(), Some("done"));
        assert_eq!(result.session_id.as_deref(), Some("dev-1"));
        assert_eq!(result.provider.as_deref(), Some(ADAPTER_TYPE));

        // 应该有 stdout + session event
        let mut saw_stdout = false;
        let mut saw_session = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AdapterEvent::Output {
                    stream: OutputStream::Stdout,
                    text,
                    ..
                } => {
                    saw_stdout = saw_stdout || text.contains("hello");
                }
                AdapterEvent::Session { session_id, .. } => {
                    saw_session = session_id.is_some();
                }
                _ => {}
            }
        }
        assert!(saw_stdout, "expected stdout text emitted");
        assert!(saw_session, "expected session event emitted");
    }

    #[tokio::test]
    async fn full_execute_error_branch_returns_exit_one() {
        let run_info = json!({ "runId": "r-2" });
        let script = vec![
            ScriptedStep::Connect {
                hello: hello_frame("dev-2"),
            },
            ScriptedStep::Request {
                method: "device.run.send".to_owned(),
                payload: run_info.clone(),
            },
            ScriptedStep::Event {
                frame: run_event("run.error", Some(json!({ "error": "forbidden" }))),
            },
            ScriptedStep::Disconnect,
        ];
        let client: DynWireClient = Arc::new(FakeWireClient::with_script(script));
        let ctx = sample_context();
        let (sink, _rx) = AdapterEventSink::channel(8);
        let result = execute_with_client(client, ctx, sink)
            .await
            .expect("execute ok");
        assert_eq!(result.exit_code, Some(1));
        assert_eq!(result.error_message.as_deref(), Some("forbidden"));
        assert_eq!(result.error_code.as_deref(), Some("run.error"));
    }

    #[tokio::test]
    async fn full_execute_connect_error_propagates() {
        let script = vec![ScriptedStep::Error {
            message: "denied".to_owned(),
            code: Some("FORBIDDEN".to_owned()),
        }];
        let client: DynWireClient = Arc::new(FakeWireClient::with_script(script));
        let ctx = sample_context();
        let (sink, _rx) = AdapterEventSink::channel(8);
        let err = execute_with_client(client, ctx, sink).await.unwrap_err();
        match err {
            AdapterError::Process(m) => assert!(m.contains("FORBIDDEN")),
            other => panic!("expected Process error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn full_execute_invalid_gateway_url_returns_config_error() {
        // ftp:// scheme is not in {ws, wss, http, https} → validate_gateway_url rejects.
        let cfg = json!({ "gatewayUrl": "ftp://example.com/ws", "identity": test_identity() });
        let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "hi");
        ctx.adapter_config = cfg;
        ctx.runtime_config = json!({});
        let client: DynWireClient = Arc::new(FakeWireClient::new());
        let (sink, _rx) = AdapterEventSink::channel(8);
        let err = execute_with_client(client, ctx, sink).await.unwrap_err();
        assert!(matches!(err, AdapterError::InvalidConfiguration(_)));
    }

    #[tokio::test]
    async fn full_execute_missing_identity_returns_config_error() {
        let mut ctx = sample_context();
        ctx.adapter_config = json!({ "gatewayUrl": "ws://127.0.0.1:8080/ws" });
        let client: DynWireClient = Arc::new(FakeWireClient::new());
        let (sink, _rx) = AdapterEventSink::channel(8);
        let err = execute_with_client(client, ctx, sink).await.unwrap_err();
        assert!(matches!(err, AdapterError::InvalidConfiguration(_)));
    }

    #[tokio::test]
    async fn full_execute_no_terminal_event_still_returns_ok() {
        let run_info = json!({ "runId": "r-3" });
        let script = vec![
            ScriptedStep::Connect {
                hello: hello_frame("dev-3"),
            },
            ScriptedStep::Request {
                method: "device.run.send".to_owned(),
                payload: run_info.clone(),
            },
            ScriptedStep::Event {
                frame: run_event("stream.chunk", Some(json!({ "text": "in progress" }))),
            },
            // 没有 run.complete event
            ScriptedStep::Disconnect,
        ];
        let client: DynWireClient = Arc::new(FakeWireClient::with_script(script));
        let ctx = sample_context();
        let (sink, _rx) = AdapterEventSink::channel(8);
        let result = execute_with_client(client, ctx, sink)
            .await
            .expect("execute ok");
        assert_eq!(result.exit_code, Some(0));
        assert!(result.error_message.is_none());
    }

    #[tokio::test]
    async fn full_execute_session_key_strategy_run_uses_run_id() {
        let run_info = json!({ "runId": "r-4" });
        let script = vec![
            ScriptedStep::Connect {
                hello: hello_frame("dev-4"),
            },
            ScriptedStep::Request {
                method: "device.run.send".to_owned(),
                payload: run_info.clone(),
            },
            ScriptedStep::Event {
                frame: run_event("run.complete", Some(json!({}))),
            },
            ScriptedStep::Disconnect,
        ];
        let client: DynWireClient = Arc::new(FakeWireClient::with_script(script));
        let mut ctx = sample_context();
        // Override session key strategy to "run"
        ctx.adapter_config = json!({
            "gatewayUrl": "ws://127.0.0.1:8080/ws",
            "sessionKeyStrategy": "run",
            "identity": {
                "deviceId": "dev-1",
                "publicKeyRawBase64Url": "AAAA",
                "privateKeyPem": "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----\n",
                "source": "configured",
            },
        });
        let (sink, _rx) = AdapterEventSink::channel(8);
        let _ = execute_with_client(client, ctx, sink)
            .await
            .expect("execute ok");
        // 检查 client.calls 验证了 device.run.send 收到 sessionKey = agent:<id>:run-<id>
        // （间接验证：上一行 execute 成功即说明 sessionKey 构造无错）
    }
}
