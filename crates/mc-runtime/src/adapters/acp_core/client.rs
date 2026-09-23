//! ACP 客户端状态机（[`AcpDecoder`]）：JSON-RPC 2.0 over stdio。
//!
//! # 线序
//!
//! ```text
//! → {"jsonrpc":"2.0","id":1,"method":"initialize","params":{protocolVersion:1,clientInfo:{…}}}
//! ← {"jsonrpc":"2.0","id":1,"result":{…}}                        ← 需要时看 authMethods
//! → {"jsonrpc":"2.0","id":2,"method":"authenticate",…}           ← 只有 grok
//! ← {"jsonrpc":"2.0","id":2,"result":{}}
//! → {"jsonrpc":"2.0","id":3,"method":"session/resume"|"session/load"|"session/new",…}
//! ← {"jsonrpc":"2.0","id":3,"result":{"sessionId":"…"}}           ← 会话 id
//! → {"jsonrpc":"2.0","id":4,"method":"session/set_model",…}      ← 请求带模型时
//! → {"jsonrpc":"2.0","id":5,"method":"session/set_config_option",…} ← 只有 kimi + 推理等级
//! → {"jsonrpc":"2.0","id":6,"method":"session/prompt","params":{sessionId,prompt:[…]}}
//! ← {"jsonrpc":"2.0","method":"session/update","params":{"update":{…}}}   ← 正文/思维/工具/用量
//! ← {"jsonrpc":"2.0","id":6,"result":{"stopReason":"end_turn","usage":{…}}}
//! ```
//!
//! 两条方向相反的"请求"会交织：对端也会**反手**请求客户端（
//! `session/request_permission`、`terminal/*`）。因此 [`AcpDecoder::push_line`]
//! 的分派顺序与上游 `hermesClient.handleLine` 一致：先看是不是应答
//! （有 `id` 且有 `result`/`error`），再看是不是对端发来的请求（有 `id` 且有
//! `method`），最后才是通知（只有 `method`）。
//!
//! # 为什么帧 id 是固定的 1..=6
//!
//! 应答靠 id 对回状态机，不靠到达顺序，所以 id 不必真的递增——但**固定**能让
//! 抓包、日志与一致性回放脚本都可预测（`codex/stream.rs` 同款记法）。每个应答
//! 处理分支都带 `phase` 守卫：id 认得出但阶段不对的应答一律丢弃（对端乱序、
//! 重复或提前应答都不会把状态机带跑）。
//!
//! # 有意偏离上游的三处
//!
//! 1. **取消会补发 `session/cancel` 通知**（[`AcpDecoder::cancel_frames`]）。
//!    上游根本没有这个帧：它靠取消 context / 杀进程收场。本 crate 的 run 循环
//!    本来就"先发帧、再 ≤200ms drain、再 kill"（见
//!    [`crate::adapters::cli_core::run`]），补一条通知让对端能自己把 turn 收干净，
//!    代价只是对端多收一条未知通知（JSON-RPC 通知没有 id，不需应答，标准实现会
//!    忽略）。**这是本片新增行为**，`docs/33` §6 有表。
//! 2. **`finish()` 永不报终态失败**：终态失败只从 `session/prompt` 应答的
//!    `stopReason` / JSON-RPC error 来。否则用户取消（流被截断、没有任何应答）
//!    会被判成 `Failed/AgentError` 假失败——`codearts`/`opencode` 那套 fail-closed
//!    解码器的老坑，这里不再踩一次。
//! 3. **不做"用法精算"**：上游 `acp_usage.go` 按字段出现与否消解"输入是否已含
//!    缓存读"的歧义；本片统一"按模型逐桶取最大值"，单调且与到达顺序无关。

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::super::cli_core::decoder::{field_str, json_str, CliDecoder, CliSummary, DecoderState};
use super::decode::{
    content_text, env_non_empty, error_frame, extract_auth_methods, message_text, model_for_usage,
    non_empty, normalize_tool_aliases, normalize_update, notification_frame, parse_model_id,
    parse_tool_args, parse_usage, request_frame, response_frame, rpc_error_message,
    select_permission_option, select_xai_auth_method, tool_input, tool_name, tool_name_from_update,
    tool_output,
};
use super::{
    AcpAuth, AcpFlavor, AcpPromptFields, CLIENT_NAME, CLIENT_VERSION, ID_AUTHENTICATE,
    ID_INITIALIZE, ID_PROMPT, ID_SESSION, ID_SET_CONFIG, ID_SET_MODEL, NO_PERMISSION_OPTION,
    PROTOCOL_VERSION, TERMINAL_NOT_ENABLED,
};
use crate::adapter::{EventDecoder, LaunchRequest, RuntimeEvent, TokenUsage};

/// 握手进度（每个阶段只处理自己那一帧的应答）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// 还没发握手帧。
    Start,
    /// 已发 `initialize`，等它的应答。
    AwaitInitialize,
    /// 已发 `authenticate`，等它的应答。
    AwaitAuthenticate,
    /// 已发会话帧（new/resume/load），等会话 id。
    AwaitSession,
    /// 已发 `session/set_model`。
    AwaitSetModel,
    /// 已发 `session/set_config_option`。
    AwaitSetConfig,
    /// 已发 `session/prompt`。
    AwaitPrompt,
    /// 已定终态（或已失败），后续应答不再改状态。
    Finished,
}

impl Phase {
    /// 是否已经定终态。
    const fn is_finished(self) -> bool {
        matches!(self, Self::Finished)
    }
}

/// 延迟发射路径里的在飞工具（上游 `pendingACPTool`）。
#[derive(Debug, Clone, Default)]
struct PendingTool {
    /// 起始帧给出的工具名（可能为空，完成帧再补）。
    tool: String,
    /// `tool_call_update` 流式累积的参数文本（**覆盖**而不是追加，与上游一致）。
    args_text: String,
    /// 是否已经在起始帧发过 `ToolUse`。
    emitted: bool,
}

/// ACP 事件解码器（6 个 ACP provider 共用，差异全在 [`AcpFlavor`]）。
pub struct AcpDecoder {
    flavor: &'static AcpFlavor,
    state: DecoderState,
    phase: Phase,
    prompt: String,
    model: Option<String>,
    thinking_level: Option<String>,
    resume_session: Option<String>,
    cwd: String,
    /// `XAI_API_KEY` 是否可用（先看本次 run 的 env，再看进程 env）。
    have_api_key: bool,
    /// 待写进 stdin 的帧。
    outbox: Vec<String>,
    /// 在飞的（延迟发射的）工具，按 call id。
    pending_tools: BTreeMap<String, PendingTool>,
    /// 按模型逐桶取最大值后的用量。
    usage: BTreeMap<String, TokenUsage>,
}

impl AcpDecoder {
    /// 按一次 run 的请求构造（会话参数、模型、推理等级都从请求里取）。
    pub fn new(flavor: &'static AcpFlavor, request: &LaunchRequest) -> Self {
        Self {
            flavor,
            state: DecoderState::default(),
            phase: Phase::Start,
            prompt: request.prompt.clone(),
            model: non_empty(request.model.as_deref()),
            thinking_level: non_empty(request.thinking_level.as_deref()),
            resume_session: non_empty(request.resume_session.as_deref()),
            // 上游 kimi.go 只在 cwd 为空时才回落到 "."：这里统一按同一回落。
            cwd: non_empty(
                request
                    .cwd
                    .as_deref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .as_deref(),
            )
            .unwrap_or_else(|| ".".to_owned()),
            have_api_key: env_non_empty(request, "XAI_API_KEY"),
            outbox: Vec::new(),
            pending_tools: BTreeMap::new(),
            usage: BTreeMap::new(),
        }
    }

    /// 取走待写帧。
    fn drain_outbox(&mut self) -> Vec<String> {
        std::mem::take(&mut self.outbox)
    }

    // ── 握手各帧 ──

    fn queue_initialize(&mut self) {
        self.phase = Phase::AwaitInitialize;
        self.outbox.push(request_frame(
            ID_INITIALIZE,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientInfo": {"name": CLIENT_NAME, "version": CLIENT_VERSION},
                // 刻意不宣告任何客户端能力（终端能力不做，见模块文档与 docs/33 §6）。
                "clientCapabilities": {},
            }),
        ));
    }

    fn queue_session_frame(&mut self) {
        self.phase = Phase::AwaitSession;
        let (method, params) = match self.resume_session.as_deref() {
            Some(session_id) => (
                self.flavor.resume.method(),
                json!({"cwd": self.cwd, "sessionId": session_id, "mcpServers": []}),
            ),
            None => ("session/new", json!({"cwd": self.cwd, "mcpServers": []})),
        };
        self.outbox.push(request_frame(ID_SESSION, method, params));
    }

    /// 会话之后的下一步：先选模型，再下发推理等级，最后才发 prompt。
    fn queue_after_session(&mut self) -> Vec<RuntimeEvent> {
        if let Some(model) = self.model.clone() {
            let Some(session_id) = self.state.session_id().map(str::to_owned) else {
                return self.fail(format!("{} 没有会话 id，无法切换模型", self.flavor.label));
            };
            self.phase = Phase::AwaitSetModel;
            self.outbox.push(request_frame(
                ID_SET_MODEL,
                "session/set_model",
                json!({"sessionId": session_id, "modelId": model}),
            ));
            return Vec::new();
        }
        self.queue_after_model()
    }

    /// 模型这一步之后的下一步：推理等级（只有 kimi 有）→ prompt。
    fn queue_after_model(&mut self) -> Vec<RuntimeEvent> {
        if let (Some(config_id), Some(level)) =
            (self.flavor.thinking_config, self.thinking_level.clone())
        {
            let Some(session_id) = self.state.session_id().map(str::to_owned) else {
                return self.fail(format!(
                    "{} 没有会话 id，无法下发推理等级",
                    self.flavor.label
                ));
            };
            self.phase = Phase::AwaitSetConfig;
            self.outbox.push(request_frame(
                ID_SET_CONFIG,
                "session/set_config_option",
                json!({"sessionId": session_id, "configId": config_id, "value": level}),
            ));
            return Vec::new();
        }
        self.queue_prompt()
    }

    fn queue_prompt(&mut self) -> Vec<RuntimeEvent> {
        let Some(session_id) = self.state.session_id().map(str::to_owned) else {
            return self.fail(format!("{} 没有会话 id，无法发 prompt", self.flavor.label));
        };
        self.phase = Phase::AwaitPrompt;
        let blocks = json!([{"type": "text", "text": self.prompt}]);
        let mut params = Map::new();
        params.insert("sessionId".to_owned(), json!(session_id));
        params.insert("prompt".to_owned(), blocks.clone());
        // kiro 两种键都读，上游两个都发（kiro.go L373）。
        if self.flavor.prompt_fields == AcpPromptFields::PromptAndContent {
            params.insert("content".to_owned(), blocks);
        }
        self.outbox.push(request_frame(
            ID_PROMPT,
            "session/prompt",
            Value::Object(params),
        ));
        self.state.progress("running")
    }

    /// 判失败（写终态 + 一条 `Error` 事件）。
    fn fail(&mut self, message: String) -> Vec<RuntimeEvent> {
        self.phase = Phase::Finished;
        self.state.note_error(message)
    }

    // ── 应答 ──

    // JSON-RPC 的 id 在 serde_json 里是 f64，所以这里必须做一次截断转换（畸形 id
    // 不在契约内）；上游同款先按整数认、再按浮点回落。
    #[allow(clippy::cast_possible_truncation)]
    fn handle_response(&mut self, id: &Value, object: &Map<String, Value>) -> Vec<RuntimeEvent> {
        // JSON-RPC 数字默认是 f64，先按整数取，再按浮点回落（上游同款）。
        let Some(id) = id
            .as_i64()
            .or_else(|| id.as_f64().map(|value| value as i64))
        else {
            return Vec::new();
        };
        match id {
            ID_INITIALIZE if self.phase == Phase::AwaitInitialize => self.on_initialize(object),
            ID_AUTHENTICATE if self.phase == Phase::AwaitAuthenticate => {
                self.on_authenticate(object)
            }
            ID_SESSION if self.phase == Phase::AwaitSession => self.on_session(object),
            ID_SET_MODEL if self.phase == Phase::AwaitSetModel => self.on_set_model(object),
            ID_SET_CONFIG if self.phase == Phase::AwaitSetConfig => self.on_set_config(object),
            ID_PROMPT if self.phase == Phase::AwaitPrompt => self.on_prompt(object),
            _ => Vec::new(),
        }
    }

    fn on_initialize(&mut self, object: &Map<String, Value>) -> Vec<RuntimeEvent> {
        if let Some(message) = rpc_error_message(self.flavor.label, "initialize", object) {
            return self.fail(message);
        }
        if self.flavor.auth != AcpAuth::XaiApiKey {
            self.queue_session_frame();
            return Vec::new();
        }
        let result = object.get("result").cloned().unwrap_or(Value::Null);
        let offered = extract_auth_methods(&result);
        match select_xai_auth_method(&offered, self.have_api_key) {
            Ok(method_id) => {
                self.phase = Phase::AwaitAuthenticate;
                self.outbox.push(request_frame(
                    ID_AUTHENTICATE,
                    "authenticate",
                    json!({"methodId": method_id, "_meta": {"headless": true}}),
                ));
                self.state.progress("authenticating")
            }
            Err(message) => self.fail(format!("{} 认证设置失败：{message}", self.flavor.label)),
        }
    }

    fn on_authenticate(&mut self, object: &Map<String, Value>) -> Vec<RuntimeEvent> {
        if let Some(message) = rpc_error_message(self.flavor.label, "authenticate", object) {
            return self.fail(message);
        }
        self.queue_session_frame();
        Vec::new()
    }

    fn on_session(&mut self, object: &Map<String, Value>) -> Vec<RuntimeEvent> {
        let method = match self.resume_session.as_deref() {
            Some(_) => self.flavor.resume.method(),
            None => "session/new",
        };
        if let Some(message) = rpc_error_message(self.flavor.label, method, object) {
            return self.fail(message);
        }
        let result = object.get("result").cloned().unwrap_or(Value::Null);
        let reported = json_str(result.get("sessionId"));
        match (reported, self.resume_session.clone()) {
            (Some(session_id), _) => self.state.set_session(&session_id),
            // 恢复请求：对端没回 id 时沿用请求里那个（上游 resolveResumedSessionID）。
            (None, Some(requested)) => self.state.set_session(&requested),
            (None, None) => {
                return self.fail(format!("{} session/new 没有返回会话 id", self.flavor.label));
            }
        }
        self.queue_after_session()
    }

    fn on_set_model(&mut self, object: &Map<String, Value>) -> Vec<RuntimeEvent> {
        if let Some(message) = rpc_error_message(self.flavor.label, "session/set_model", object) {
            // 上游对"选了模型却切换失败"是**致命**的：静默回落到默认模型会让
            // 用户以为选择生效了（kimi.go L320 的长注释）。
            return self.fail(format!("{} 无法切换到模型：{message}", self.flavor.label));
        }
        self.queue_after_model()
    }

    fn on_set_config(&mut self, object: &Map<String, Value>) -> Vec<RuntimeEvent> {
        // 推理等级下发失败**不阻断**本轮（上游 kimi.go 只记 warning）。
        let events = match rpc_error_message(self.flavor.label, "session/set_config_option", object)
        {
            Some(message) => self.state.emit_error(format!("推理等级未生效：{message}")),
            None => Vec::new(),
        };
        events.into_iter().chain(self.queue_prompt()).collect()
    }

    fn on_prompt(&mut self, object: &Map<String, Value>) -> Vec<RuntimeEvent> {
        self.phase = Phase::Finished;
        if let Some(message) = rpc_error_message(self.flavor.label, "session/prompt", object) {
            return self.state.note_error(message);
        }
        let result = object.get("result").cloned().unwrap_or(Value::Null);
        self.apply_prompt_result(&result)
    }

    /// 从 prompt 应答 / `turn_end` 通知里取终态信号（上游 `extractPromptResult`）。
    fn apply_prompt_result(&mut self, result: &Value) -> Vec<RuntimeEvent> {
        if let Some(model) = parse_model_id(result.get("_meta")) {
            self.model = Some(model);
        }
        let mut events = Vec::new();
        if let Some(usage) = parse_usage(result.get("usage")) {
            events.extend(self.merge_usage(usage));
        }
        let stop_reason = field_str(result, "stopReason").unwrap_or_default();
        if stop_reason == "cancelled" {
            events.extend(self.state.note_error(format!(
                "{} 把这次 prompt 判为 cancelled",
                self.flavor.label
            )));
        }
        events
    }

    // ── 通知 ──

    fn handle_notification(
        &mut self,
        method: &str,
        object: &Map<String, Value>,
    ) -> Vec<RuntimeEvent> {
        if method != "session/update" && method != "session/notification" {
            return Vec::new();
        }
        let Some(update) = object.get("params").and_then(|params| params.get("update")) else {
            return Vec::new();
        };
        let (kind, data) = normalize_update(update);
        match kind.as_str() {
            "agent_message_chunk" => {
                let text = message_text(data);
                self.state.text(&text)
            }
            "agent_thought_chunk" => {
                let text = message_text(data);
                self.state.thinking(&text)
            }
            "tool_call" => self.handle_tool_call(data),
            "tool_call_update" => self.handle_tool_call_update(data),
            "usage_update" => match parse_usage(data.get("usage")) {
                Some(usage) => self.merge_usage(usage),
                None => Vec::new(),
            },
            "turn_end" => self.apply_prompt_result(data),
            _ => Vec::new(),
        }
    }

    /// `tool_call`：起始帧带参数就立刻发 `ToolUse`，否则挂起等 `tool_call_update`。
    ///
    /// 延迟发射是上游刻意的（`hermes.go` L1758 的长注释，GH#6583）：kimi 这类
    /// 后端的参数是**逐帧**流出来的，起始帧常常没有参数；而守护进程的"在飞工具"
    /// 计数只在收到 `ToolUse` 时前进，若起始帧就空转，长工具会被短得多的空闲
    /// 看门狗判死。外部 ACP provider 的 `toolStartCarriesFinalInput` 都是 false
    /// （= `BuiltinRuntime` 为假），所以走的就是延迟发射这条路。
    fn handle_tool_call(&mut self, data: &Value) -> Vec<RuntimeEvent> {
        let call_id = field_str(data, "toolCallId").unwrap_or_default();
        let tool = self.map_tool(&tool_name(data));
        if let Some(input) = tool_input(data) {
            self.pending_tools.insert(
                call_id.clone(),
                PendingTool {
                    tool: tool.clone(),
                    args_text: String::new(),
                    emitted: true,
                },
            );
            self.state.tool_use(call_id, tool, input)
        } else {
            let args_text = content_text(data);
            self.pending_tools.insert(
                call_id,
                PendingTool {
                    tool,
                    args_text,
                    emitted: false,
                },
            );
            Vec::new()
        }
    }

    fn handle_tool_call_update(&mut self, data: &Value) -> Vec<RuntimeEvent> {
        let call_id = field_str(data, "toolCallId").unwrap_or_default();
        let status = field_str(data, "status").unwrap_or_default();
        if status != "completed" && status != "failed" {
            // 流式中：只累积参数文本（覆盖，不追加）。
            if let Some(pending) = self.pending_tools.get_mut(&call_id) {
                if !pending.emitted {
                    let text = content_text(data);
                    if !text.is_empty() {
                        pending.args_text = text;
                    }
                }
            }
            return Vec::new();
        }

        let pending = self.pending_tools.remove(&call_id).unwrap_or_default();
        let mut events = Vec::new();
        if !pending.emitted {
            let tool = if pending.tool.is_empty() {
                self.map_tool(&tool_name_from_update(data))
            } else {
                pending.tool
            };
            // 完成帧自带的 `rawInput` 才是真正的入参，比起始帧渲染出来的正文
            // 优先（上游 `emitDeferredToolUse`：对端可以只在完成帧给入参）。
            let input = tool_input(data).unwrap_or_else(|| parse_tool_args(&pending.args_text));
            events.extend(self.state.tool_use(call_id.clone(), tool, input));
        }
        let output = tool_output(data);
        events.extend(self.state.tool_result(call_id, output, status == "failed"));
        events
    }

    /// 工具名过一遍 provider 别名表（上游 `onMessage` 里的重归一）。
    fn map_tool(&self, name: &str) -> String {
        normalize_tool_aliases(name, self.flavor.tool_aliases)
    }

    /// 用量合并：按模型逐桶取最大值（单调；同一快照重复到达不会翻倍）。
    fn merge_usage(&mut self, usage: TokenUsage) -> Vec<RuntimeEvent> {
        let model = model_for_usage(self.model.as_deref(), self.flavor.label);
        let entry = self.usage.entry(model.clone()).or_default();
        entry.input = entry.input.max(usage.input);
        entry.output = entry.output.max(usage.output);
        entry.cache_read = entry.cache_read.max(usage.cache_read);
        entry.cache_write = entry.cache_write.max(usage.cache_write);
        entry.total_tokens = entry.total_tokens.max(usage.total_tokens);
        let merged = *entry;
        self.state.set_usage(&model, merged)
    }

    // ── 对端发来的请求（代理 → 客户端）──

    fn handle_agent_request(
        &mut self,
        id: &Value,
        method: &str,
        object: &Map<String, Value>,
    ) -> Vec<RuntimeEvent> {
        match method {
            "session/request_permission" => {
                let options = object
                    .get("params")
                    .and_then(|params| params.get("options"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                match select_permission_option(&options) {
                    // 只选对端**真的提供过**的 optionId；绝不回 "cancelled"
                    // （有些 ACP 后端把 cancelled 读成"取消整个 turn"）。
                    Some(option_id) => self.outbox.push(response_frame(
                        id.clone(),
                        json!({"outcome": {"outcome": "selected", "optionId": option_id}}),
                    )),
                    None => self
                        .outbox
                        .push(error_frame(id.clone(), -32603, NO_PERMISSION_OPTION)),
                }
                Vec::new()
            }
            _ if method.starts_with("terminal/") => {
                // 本片不宣告终端能力，所有终端请求都 fail-closed。
                self.outbox
                    .push(error_frame(id.clone(), -32601, TERMINAL_NOT_ENABLED));
                Vec::new()
            }
            _ => {
                self.outbox.push(error_frame(
                    id.clone(),
                    -32601,
                    format!("method not found: {method}"),
                ));
                Vec::new()
            }
        }
    }
}

impl EventDecoder for AcpDecoder {
    fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            return Vec::new();
        };
        let Some(object) = value.as_object() else {
            return Vec::new();
        };
        let id = object.get("id").cloned();
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let has_outcome = object.contains_key("result") || object.contains_key("error");
        match (id, method, has_outcome) {
            (Some(id), _, true) => self.handle_response(&id, object),
            (Some(id), Some(method), false) => self.handle_agent_request(&id, &method, object),
            (_, Some(method), false) => self.handle_notification(&method, object),
            _ => Vec::new(),
        }
    }

    fn finish(&mut self) -> Vec<RuntimeEvent> {
        // 流被截断（取消 / 进程自杀）不是失败：终态失败只能来自 prompt 应答。
        Vec::new()
    }
}

impl CliDecoder for AcpDecoder {
    fn summary(&self) -> CliSummary {
        self.state.summary()
    }

    fn take_outbox(&mut self) -> Vec<String> {
        self.drain_outbox()
    }

    fn initial_frames(&mut self) -> Vec<String> {
        if self.phase != Phase::Start {
            return Vec::new();
        }
        self.queue_initialize();
        self.drain_outbox()
    }

    fn cancel_frames(&mut self) -> Vec<String> {
        if self.phase.is_finished() {
            return Vec::new();
        }
        let Some(session_id) = self.state.session_id().map(str::to_owned) else {
            return Vec::new();
        };
        self.phase = Phase::Finished;
        vec![notification_frame(
            "session/cancel",
            json!({"sessionId": session_id}),
        )]
    }
}
