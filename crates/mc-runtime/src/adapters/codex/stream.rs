//! codex `app-server` 的 JSON-RPC 2.0 解码器（上游 `codex.go` 的协议面）。
//!
//! # 协议
//!
//! `codex app-server --listen stdio://` 是**请求/应答**协议，不是事件流：
//!
//! ```text
//! → {"jsonrpc":"2.0","id":1,"method":"initialize",…}
//! → {"jsonrpc":"2.0","method":"initialized"}
//! → {"jsonrpc":"2.0","id":2,"method":"thread/start",…}       ← 或 thread/resume
//! ← {"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"…"}}}   ← thread id = 会话 id
//! → {"jsonrpc":"2.0","id":3,"method":"turn/start","params":{"threadId":…,"input":[…]}}
//! ← {"jsonrpc":"2.0","method":"turn/started","params":{"turn":{"id":…}}}
//! ← {"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"itemId":…,"delta":…}}
//! ← {"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"status":"completed"}}}
//! ```
//!
//! 关键约束是 `turn/start` 必须带 `threadId`，而它只在 `thread/start` 的**响应**里。
//! 因此 prompt 不是启动时写下去的：解码器在认出 thread id 的那一刻把 `turn/start`
//! 帧塞进 outbox，run 循环逐行读 stdout 时顺手泵出去（见
//! [`crate::adapters::cli_core::run`]）。
//!
//! # 与上游的两处有意差异
//!
//! 1. **不做逐请求的超时/重试**：上游对 `initialize` / `thread/start` 有各自的
//!    handshake 超时与"resume 失败回退到 thread/start"的重试，还会在语义静默
//!    （`semanticInactivityTimeout`）时判定卡死。这些属于 M3-3 的租约/看门狗职责，
//!    本片只用 `LaunchRequest::timeout` 兜住整轮。
//! 2. **token 用量按 `total` 覆盖**：上游按"同一 turn 内 `last` + `total` 增量"累加，
//!    以区分续跑回放的历史快照。我们没有 active-turn 归属信息，因此直接取
//!    `tokenUsage.total`（它本身就是该 thread 的累计值）。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use super::super::cli_core::decoder::{
    field_str, field_u64, tokens, CliDecoder, CliSummary, DecoderState,
};
use crate::adapter::{EventDecoder, RuntimeEvent, TokenUsage};

/// 客户端自称（上游 `initialize` 的 `clientInfo` 逐字段一致）。
const CLIENT_NAME: &str = "multica-agent-sdk";
const CLIENT_TITLE: &str = "Multica Agent SDK";
const CLIENT_VERSION: &str = "0.2.0";

/// 握手帧的固定 id（便于对照上游日志与抓包）。
const INIT_ID: u64 = 1;
const THREAD_ID: u64 = 2;
const TURN_ID: u64 = 3;
/// `turn/interrupt` 的 id（上游同款）。
const INTERRUPT_ID: u64 = 98;

/// codex app-server 的事件解码器。
pub(crate) struct CodexDecoder {
    state: DecoderState,
    model: String,
    prompt: String,
    cwd: Option<PathBuf>,
    thinking_level: Option<String>,
    resume: Option<String>,
    /// 已认出的 thread id（= 会话 id）。
    thread: String,
    /// 已认出的 turn id（`turn/interrupt` 要用）。
    turn: String,
    /// 每个 agentMessage item 已经交付出去的文本：`item/completed` 的快照是权威的，
    /// 但要减掉前面已经流出去的增量，否则正文会重复一遍。
    delivered: BTreeMap<String, String>,
    /// 待写进 stdin 的帧（`turn/start` / `turn/interrupt`）。
    outbox: Vec<String>,
    /// `turn/start` 是否已经排过（thread id 只应触发一次）。
    turn_queued: bool,
    /// 握手帧是否已经取走。
    handshaken: bool,
}

impl CodexDecoder {
    /// `model` 只用于用量事件的归属名。
    pub(crate) fn new(
        model: String,
        prompt: String,
        cwd: Option<PathBuf>,
        thinking_level: Option<String>,
        resume: Option<String>,
    ) -> Self {
        Self {
            state: DecoderState::default(),
            model,
            prompt,
            cwd,
            thinking_level,
            resume,
            thread: String::new(),
            turn: String::new(),
            delivered: BTreeMap::new(),
            outbox: Vec::new(),
            turn_queued: false,
            handshaken: false,
        }
    }

    /// `initialize` → `initialized` → `thread/start`（或 `thread/resume`）。
    pub(crate) fn initial_frames(&mut self) -> Vec<String> {
        if self.handshaken {
            return Vec::new();
        }
        self.handshaken = true;
        let params = json!({
            "clientInfo": {
                "name": CLIENT_NAME,
                "title": CLIENT_TITLE,
                "version": CLIENT_VERSION,
            },
            "capabilities": { "experimentalApi": true },
        });
        let mut frames = vec![
            frame(INIT_ID, "initialize", &params),
            notification("initialized"),
        ];
        // `developerInstructions` 留 null：runtime brief 已经在线程的上下文里，
        // 每个 turn 再内联一遍就是重复（上游在 codex-cli 0.144.6 上验过）。
        let mut setup = json!({
            "model": non_empty(&self.model),
            "modelProvider": Value::Null,
            "profile": Value::Null,
            "cwd": self.cwd.as_ref().map_or(Value::Null, |cwd| json!(cwd.display().to_string())),
            "approvalPolicy": Value::Null,
            "sandbox": Value::Null,
            "config": Value::Null,
            "baseInstructions": Value::Null,
            "developerInstructions": Value::Null,
            "compactPrompt": Value::Null,
            "includeApplyPatchTool": Value::Null,
            "experimentalRawEvents": false,
            "persistExtendedHistory": true,
        });
        // 续跑：`thread/resume` 复用线程持久化的模型与推理等级，只在显式给了
        // thinking_level 时才覆盖。
        let method = match self.resume.as_deref().filter(|id| !id.is_empty()) {
            Some(thread) => {
                setup["threadId"] = json!(thread);
                "thread/resume"
            }
            None => "thread/start",
        };
        if let Some(level) = self.thinking_level.as_deref().filter(|l| !l.is_empty()) {
            setup["config"] = json!({ "model_reasoning_effort": level });
        }
        frames.push(frame(THREAD_ID, method, &setup));
        frames
    }

    /// `turn/start`：thread id 一到就能发了。
    fn queue_turn_start(&mut self) {
        if self.turn_queued || self.thread.is_empty() {
            return;
        }
        self.turn_queued = true;
        let mut params = json!({
            "threadId": self.thread,
            "input": [{ "type": "text", "text": self.prompt }],
        });
        // turn/start 的推理等级是顶层的 `effort`（thread 请求才嵌在 config 里）。
        if let Some(level) = self.thinking_level.as_deref().filter(|l| !l.is_empty()) {
            params["effort"] = json!(level);
        }
        self.outbox.push(frame(TURN_ID, "turn/start", &params));
    }

    fn handle(&mut self, raw: &Value) -> Vec<RuntimeEvent> {
        if let Some(method) = raw.get("method").and_then(Value::as_str) {
            let params = raw.get("params").cloned().unwrap_or(Value::Null);
            // 服务端发来的**请求**（审批提示）需要回包；本解码器没有应答通道，
            // 因此只能忽略（见 `docs/33` 的已知缺口）。
            if raw.get("id").is_some() {
                return Vec::new();
            }
            return self.handle_notification(method, &params);
        }
        let Some(id) = raw.get("id").and_then(Value::as_u64) else {
            return Vec::new();
        };
        let error = raw.get("error").map(error_message);
        let result = raw.get("result").cloned().unwrap_or(Value::Null);
        match id {
            INIT_ID => {
                if let Some(message) = error {
                    self.state
                        .note_error(format!("codex initialize failed: {message}"))
                } else {
                    Vec::new()
                }
            }
            THREAD_ID => {
                if let Some(message) = error {
                    self.state
                        .note_error(format!("codex thread/start failed: {message}"))
                } else {
                    let thread = extract_thread_id(&result);
                    if !thread.is_empty() {
                        self.thread = thread;
                        self.state.set_session(&self.thread);
                        self.queue_turn_start();
                    }
                    Vec::new()
                }
            }
            TURN_ID => {
                if let Some(message) = error {
                    self.state
                        .note_error(format!("codex turn/start failed: {message}"))
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn handle_notification(&mut self, method: &str, params: &Value) -> Vec<RuntimeEvent> {
        match method {
            "turn/started" => {
                if let Some(turn) = nested(params, &["turn", "id"]).and_then(Value::as_str) {
                    turn.clone_into(&mut self.turn);
                }
                self.state.progress("running")
            }
            "turn/completed" => {
                let status = nested(params, &["turn", "status"])
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(turn) = nested(params, &["turn", "id"]).and_then(Value::as_str) {
                    turn.clone_into(&mut self.turn);
                }
                let mut events = self.flush_pending_text();
                match status {
                    "failed" => {
                        let message = nested(params, &["turn", "error", "message"])
                            .and_then(Value::as_str)
                            .unwrap_or("codex turn failed");
                        events.extend(self.state.note_error(message.to_owned()));
                    }
                    "cancelled" | "canceled" | "aborted" | "interrupted" => {
                        events.extend(self.state.note_error("turn was aborted".to_owned()));
                    }
                    _ => {}
                }
                events
            }
            "thread/tokenUsage/updated" => {
                // 只有当前 turn 的快照才算（续跑会回放历史快照）；缺 `turnId` 时不拦，
                // 免得新版 app-server 省掉这个字段就把用量整条丢掉。
                let turn_id = field_str(params, "turnId").unwrap_or_default();
                if !turn_id.is_empty() && !self.turn.is_empty() && turn_id != self.turn {
                    return Vec::new();
                }
                let Some(total) = nested(params, &["tokenUsage", "total"]) else {
                    return Vec::new();
                };
                let usage = codex_usage(total);
                if usage.is_empty() {
                    return Vec::new();
                }
                self.state.set_usage(&self.model, usage)
            }
            "error" => {
                let will_retry = params
                    .get("willRetry")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if will_retry {
                    return Vec::new();
                }
                let message = nested(params, &["error", "message"])
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| field_str(params, "message"));
                match message {
                    Some(message) => self.state.note_error(message),
                    None => Vec::new(),
                }
            }
            // 纯信息性（只说明线程状态变了），终态只看 turn/completed。
            "thread/status/changed" => Vec::new(),
            other if other.starts_with("item/") => self.handle_item(other, params),
            _ => Vec::new(),
        }
    }

    fn handle_item(&mut self, method: &str, params: &Value) -> Vec<RuntimeEvent> {
        let item = params.get("item");
        match method {
            // 增量通知的 schema 是平的：`{threadId, turnId, itemId, delta}`。
            "item/agentMessage/delta" => {
                let item_id = field_str(params, "itemId").unwrap_or_default();
                let delta = field_str(params, "delta").unwrap_or_default();
                if item_id.is_empty() || delta.is_empty() {
                    return Vec::new();
                }
                self.delivered.entry(item_id).or_default().push_str(&delta);
                self.state.text(&delta)
            }
            "item/started" | "item/completed" => {
                let Some(item) = item else { return Vec::new() };
                let item_type = field_str(item, "type").unwrap_or_default();
                let item_id = field_str(item, "id").unwrap_or_default();
                if item_id.is_empty() {
                    return Vec::new();
                }
                match (method, item_type.as_str()) {
                    ("item/started", "commandExecution") => {
                        let command = field_str(item, "command").unwrap_or_default();
                        self.state
                            .tool_use(item_id, "exec_command", json!({ "command": command }))
                    }
                    ("item/completed", "commandExecution") => {
                        let output = field_str(item, "aggregatedOutput").unwrap_or_default();
                        self.state.tool_result(item_id, output, false)
                    }
                    ("item/started", "fileChange") => {
                        let changes = item.get("changes").cloned().unwrap_or(Value::Null);
                        self.state.tool_use(item_id, "patch_apply", changes)
                    }
                    ("item/completed", "fileChange") => {
                        let status = field_str(item, "status").unwrap_or_default();
                        self.state.tool_result(item_id, status, false)
                    }
                    ("item/completed", "agentMessage") => {
                        let text = field_str(item, "text").unwrap_or_default();
                        self.complete_agent_message(&item_id, &text)
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    /// `item/completed` 的文本是权威快照：只补发增量还没交付出去的后缀。
    fn complete_agent_message(&mut self, item_id: &str, text: &str) -> Vec<RuntimeEvent> {
        let delivered = self.delivered.remove(item_id).unwrap_or_default();
        if text.is_empty() {
            return Vec::new();
        }
        if delivered.is_empty() {
            return self.state.text(text);
        }
        match text.strip_prefix(&delivered) {
            // 前缀对不上：已经交付的正文无法撤回，也不能再追加一份（会重复）。
            Some("") | None => Vec::new(),
            Some(suffix) => self.state.text(suffix),
        }
    }

    /// 收尾前把"只收到增量、没有 item/completed"的正文留着（已经在增量时交付过，
    /// 这里只清理记账）。
    fn flush_pending_text(&mut self) -> Vec<RuntimeEvent> {
        self.delivered.clear();
        Vec::new()
    }

    /// `turn/interrupt` 帧（取消路径用）。
    fn cancel_frames(&mut self) -> Vec<String> {
        if self.thread.is_empty() {
            return Vec::new();
        }
        let mut params = json!({ "threadId": self.thread });
        if !self.turn.is_empty() {
            params["turnId"] = json!(self.turn);
        }
        vec![frame(INTERRUPT_ID, "turn/interrupt", &params)]
    }
}

impl EventDecoder for CodexDecoder {
    fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent> {
        if line.trim().is_empty() {
            return Vec::new();
        }
        match serde_json::from_str::<Value>(line) {
            Ok(raw) => self.handle(&raw),
            // 非 JSON 行（CLI 的告警、横幅）：容忍，不因此中断 run。
            Err(_) => Vec::new(),
        }
    }

    fn finish(&mut self) -> Vec<RuntimeEvent> {
        // JSON-RPC 的终态由 `turn/completed` 决定；stdout 关闭本身不是失败信号
        // （codex 在 turn 结束后会自己退，EOF 是正常现象）。
        Vec::new()
    }
}

impl CliDecoder for CodexDecoder {
    fn summary(&self) -> CliSummary {
        self.state.summary()
    }

    fn take_outbox(&mut self) -> Vec<String> {
        std::mem::take(&mut self.outbox)
    }

    fn initial_frames(&mut self) -> Vec<String> {
        CodexDecoder::initial_frames(self)
    }

    fn cancel_frames(&mut self) -> Vec<String> {
        CodexDecoder::cancel_frames(self)
    }
}

/// `{"jsonrpc":"2.0","id":…,"method":…,"params":…}` 一行。
/// 一帧 JSON-RPC 请求（上游 `request` / `notify` 写完都补 `'\n'`）。
///
/// 这个换行不能省：`app-server` 的 stdin 是 **newline-delimited** JSON-RPC，
/// 不补的话它永远拼不出一条完整的报文（也就永远不应答）。
fn frame(id: u64, method: &str, params: &Value) -> String {
    let mut line =
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
    line.push('\n');
    line
}

/// 无 `id` 的通知帧（`initialized`）。
fn notification(method: &str) -> String {
    let mut line = json!({ "jsonrpc": "2.0", "method": method }).to_string();
    line.push('\n');
    line
}

/// 空串 → `null`（上游 `nilIfEmpty`：让 CLI 自己的配置说了算）。
fn non_empty(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

/// 逐层取嵌套字段。
fn nested<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

/// 上游 `extractThreadID`：`result.thread.id`；顺带兼容 `result.threadId`。
fn extract_thread_id(result: &Value) -> String {
    if let Some(id) = nested(result, &["thread", "id"]).and_then(Value::as_str) {
        return id.to_owned();
    }
    field_str(result, "threadId").unwrap_or_default()
}

/// JSON-RPC 错误对象 → 人类可读串（`data.message` 优先，其次 `message`）。
fn error_message(error: &Value) -> String {
    if let Some(message) = nested(error, &["data", "message"]).and_then(Value::as_str) {
        return message.to_owned();
    }
    field_str(error, "message").unwrap_or_else(|| "unknown error".to_owned())
}

/// `tokenUsage.total` → [`TokenUsage`]（字段名来自 v2 app-server schema）。
fn codex_usage(total: &Value) -> TokenUsage {
    tokens(
        field_u64(total, "inputTokens"),
        field_u64(total, "outputTokens"),
        field_u64(total, "cachedInputTokens"),
        field_u64(total, "cacheWriteInputTokens"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder() -> CodexDecoder {
        CodexDecoder::new(
            "gpt-5-codex".to_owned(),
            "干点活".to_owned(),
            None,
            None,
            None,
        )
    }

    fn text_of(events: &[RuntimeEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::Text { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn handshake_is_initialize_then_thread_start() {
        let mut decoder = decoder();
        let raw = decoder.initial_frames();
        // 每帧都必须以换行结束（app-server 的 stdin 是逐行解析的）。
        for frame in &raw {
            assert!(frame.ends_with('\n'), "帧必须以换行结束：{frame:?}");
        }
        let frames: Vec<Value> = raw
            .iter()
            .map(|frame| serde_json::from_str(frame).expect("合法 JSON"))
            .collect();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0]["method"], "initialize");
        assert_eq!(frames[0]["id"], 1);
        assert_eq!(frames[1]["method"], "initialized");
        assert_eq!(frames[2]["method"], "thread/start");
        assert_eq!(frames[2]["params"]["cwd"], Value::Null);
        // 握手帧只交付一次。
        assert!(decoder.initial_frames().is_empty());
    }

    #[test]
    fn resume_uses_thread_resume_and_reasoning_effort() {
        let mut decoder = CodexDecoder::new(
            "gpt-5-codex".to_owned(),
            "p".to_owned(),
            Some(PathBuf::from("/work")),
            Some("high".to_owned()),
            Some("th-old".to_owned()),
        );
        let frames = decoder.initial_frames();
        let setup: Value = serde_json::from_str(&frames[2]).expect("合法 JSON");
        assert_eq!(setup["method"], "thread/resume");
        assert_eq!(setup["params"]["threadId"], "th-old");
        assert_eq!(setup["params"]["config"]["model_reasoning_effort"], "high");
        assert_eq!(setup["params"]["cwd"], "/work");
    }

    #[test]
    fn thread_response_queues_turn_start_with_the_prompt() {
        let mut decoder = CodexDecoder::new(
            "gpt-5-codex".to_owned(),
            "SECRET".to_owned(),
            None,
            Some("low".to_owned()),
            None,
        );
        decoder.push_line(r#"{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"th_1"}}}"#);
        let frames = decoder.take_outbox();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].ends_with('\n'), "帧必须以换行结束");
        let turn: Value = serde_json::from_str(&frames[0]).expect("合法 JSON");
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["id"], 3);
        assert_eq!(turn["params"]["threadId"], "th_1");
        assert_eq!(turn["params"]["input"][0]["text"], "SECRET");
        assert_eq!(turn["params"]["effort"], "low");
        assert_eq!(decoder.summary().session_id.as_deref(), Some("th_1"));
    }

    #[test]
    fn failed_setup_sets_terminal_error_instead_of_queueing_a_turn() {
        let mut decoder = decoder();
        let events = decoder.push_line(
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"boom","data":{"message":"model unknown"}}}"#,
        );
        assert!(matches!(events.as_slice(), [RuntimeEvent::Error { .. }]));
        assert!(decoder.take_outbox().is_empty());
        assert_eq!(
            decoder.summary().terminal_error.as_deref(),
            Some("codex thread/start failed: model unknown")
        );
    }

    #[test]
    fn deltas_then_completed_snapshot_do_not_duplicate_text() {
        let mut decoder = decoder();
        let mut events = decoder.push_line(
            r#"{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"itemId":"i1","delta":"ok"}}"#,
        );
        events.extend(decoder.push_line(
            r#"{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"id":"i1","type":"agentMessage","text":"ok!"}}}"#,
        ));
        // 增量只交付了 "ok"，快照的权威后缀是 "!"。
        assert_eq!(text_of(&events), "ok!");
        assert_eq!(decoder.summary().output, "ok!");
    }

    #[test]
    fn tool_items_become_use_and_result_pairs() {
        let mut decoder = decoder();
        let events = decoder.push_line(
            r#"{"jsonrpc":"2.0","method":"item/started","params":{"item":{"id":"c1","type":"commandExecution","command":"ls"}}}"#,
        );
        assert!(matches!(
            events.as_slice(),
            [RuntimeEvent::ToolUse { call_id, tool, .. }] if call_id == "c1" && tool == "exec_command"
        ));
        let events = decoder.push_line(
            r#"{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"id":"c1","type":"commandExecution","aggregatedOutput":"a.rs"}}}"#,
        );
        assert!(matches!(
            events.as_slice(),
            [RuntimeEvent::ToolResult { call_id, output, is_error }]
                if call_id == "c1" && output == "a.rs" && !is_error
        ));
    }

    #[test]
    fn terminal_statuses_and_usage_are_decoded() {
        let mut decoder = decoder();
        decoder.push_line(
            r#"{"jsonrpc":"2.0","method":"turn/started","params":{"turn":{"id":"t1"}}}"#,
        );
        let events = decoder.push_line(
            r#"{"jsonrpc":"2.0","method":"thread/tokenUsage/updated","params":{"turnId":"t1","tokenUsage":{"total":{"inputTokens":10,"outputTokens":5,"cachedInputTokens":1,"cacheWriteInputTokens":0},"last":{"inputTokens":10,"outputTokens":5,"cachedInputTokens":1,"cacheWriteInputTokens":0}}}}"#,
        );
        assert!(matches!(
            events.as_slice(),
            [RuntimeEvent::Usage { usage, .. }] if usage.total_tokens == 16
        ));

        // 别的 turn 的用量快照（续跑回放）不算。
        let events = decoder.push_line(
            r#"{"jsonrpc":"2.0","method":"thread/tokenUsage/updated","params":{"turnId":"other","tokenUsage":{"total":{"inputTokens":999,"outputTokens":0,"cachedInputTokens":0,"cacheWriteInputTokens":0},"last":{}}}}"#,
        );
        assert!(events.is_empty());

        let events =
            decoder.push_line(r#"{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"t1","status":"failed","error":{"message":"turn exploded"}}}}"#);
        assert!(matches!(events.as_slice(), [RuntimeEvent::Error { .. }]));
        assert_eq!(
            decoder.summary().terminal_error.as_deref(),
            Some("turn exploded")
        );
    }

    #[test]
    fn cancel_frame_carries_thread_and_turn() {
        let mut decoder = decoder();
        decoder.push_line(r#"{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"th_1"}}}"#);
        decoder.push_line(
            r#"{"jsonrpc":"2.0","method":"turn/started","params":{"turn":{"id":"t1"}}}"#,
        );
        let frames = decoder.cancel_frames();
        let interrupt: Value = serde_json::from_str(&frames[0]).expect("合法 JSON");
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert_eq!(interrupt["id"], 98);
        assert_eq!(interrupt["params"]["threadId"], "th_1");
        assert_eq!(interrupt["params"]["turnId"], "t1");
    }

    #[test]
    fn junk_and_unknown_events_are_ignored() {
        let mut decoder = decoder();
        assert!(decoder.push_line("codex: warning banner").is_empty());
        assert!(decoder
            .push_line(r#"{"jsonrpc":"2.0","method":"thread/status/changed","params":{}}"#)
            .is_empty());
        assert!(decoder.push_line(r#"{"jsonrpc":"2.0","method":"item/started","params":{"item":{"id":"","type":"reasoning"}}}"#).is_empty());
        assert!(decoder.finish().is_empty());
        assert!(decoder.summary().terminal_error.is_none());
    }
}
