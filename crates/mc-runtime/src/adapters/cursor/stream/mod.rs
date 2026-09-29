//! `cursor-agent --output-format stream-json` 的解码器（上游 `cursor.go` 的解析部分）。
//!
//! # 事件 → 事件
//!
//! | stream-json 事件 | 本实现 |
//! |---|---|
//! | `system`（任何 subtype） | 记 `session_id`；`init` → `Progress { "running" }`；`error` → `Error`（**不**改终态） |
//! | `assistant`（`output_text` / `text` / `thinking` / `tool_use` 块） | `Text` / `Thinking` / `ToolUse`（**忽略** `message.usage`，见下） |
//! | `thinking`（`delta` / `completed`） | `delta` → `Thinking`；`completed` 只关块（块之间补空行）；其它 subtype 忽略 |
//! | `tool_call`（`started` / `completed`） | `started` → `ToolUse`；`completed` → `ToolResult` |
//! | `tool_use` / `tool_result`（旧版 CLI） | `ToolUse` / `ToolResult` |
//! | `result` | 记 `session_id`；`is_error` / `subtype:"error"` → **失败**；正文为空时用 `result` 文本兜底；用量以本事件为准 |
//! | `error` | `Error`（**不**改终态，只在流结束仍无 `result` 时升级为失败） |
//! | `text`（`part.text`） | `Text` |
//! | `step_finish`（`part.tokens`） | 只累加用量，不发事件（`result` 没报用量时用它兜底） |
//! | 其它 | 忽略 |
//!
//! # `result` 是协议边界
//!
//! 上游把 `result` 事件当"这一 turn 已经定了"的边界：看到它就 `cancel()` 掉 run
//! context（新版本 `cursor-agent` 会把 worker 进程留着不让退出）。本实现同款——
//! 看到 `result` 之后不再把"流里没终态"当成失败。
//!
//! # 与上游的差异（都记在 `docs/33`）
//!
//! 1. **正文口径**：上游 `output` = 所有文本块 + `result` 兜底；本 crate 统一成
//!    "所有 `Text` 事件拼接"（`adapter.rs` 对 `RunOutcome::output` 的定义）。
//!    `result` 文本只在**没吐过**任何文本时兜底，两个口径在单 turn 场景下等价。
//! 2. **不做后台工具跟踪**：上游把 `shell` 的 `isBackground` 调用挂到
//!    `cursorBackgroundTools` 上常驻监听、并把 `system/task_notification` 当作回收点。
//!    本实现没有这套后台台账（daemon 侧的 in-flight 计数由 `RuntimeEvent` 驱动），
//!    因此 `tool_call/completed` 一律发 `ToolResult`。
//! 3. **不做协议漂移计数**：上游统计未识别的事件 type/subtype 并在运行日志里告警
//!    （MUL-5231 / MUL-5434）。本 crate 的 decoder 没有 logger 通道，未知事件一律
//!    静默忽略 —— 这是**已知缺口**，不是"没有漂移"的证明。
//! 4. **`assistant.message.usage` 忽略**：与上游一致（token 只认 `result`，避免重复计），
//!    这一条写在这里是提醒："assistant 里有过用量"不代表本实现漏了它。
//!
//! # 文件拆分（门 ⑩）
//!
//! 本文件与 [`usage`]/`tests` 是同一份代码的**纯移动**（单文件 800 行上限，
//! `scripts/file_size_check.py`）：用量归集搬到 `stream/usage.rs`，单元用例搬到
//! `stream/tests.rs`。先例 = `docs/32` §30 的 **D10**。

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::super::cli_core::decoder::{field_str, tokens};
use super::super::cli_core::{CliDecoder, CliSummary, DecoderState};
use super::LABEL;
use crate::adapter::{EventDecoder, ModelUsage, RuntimeEvent, TokenUsage};

#[cfg(test)]
mod tests;
mod usage;

use usage::{field_u64_any, nested_usage, top_level_usage, usage_map};

/// `tool_call` 信封里工具名的键后缀（`readToolCall` ⇒ `read`）。
const TOOL_CALL_KEY_SUFFIX: &str = "ToolCall";

/// Cursor 的 `thinking` 事件块边界（上游 `cursorThinkingStream`）。
///
/// 推理是**顶层事件流**（`subtype:"delta"` 逐片、`subtype:"completed"` 收尾），
/// 不是 `assistant` 里的内容块。连续两个块之间补一个空行：上游 daemon 会把
/// `Thinking` 增量直接拼接，不补行的话两段推理会粘成一段。
#[derive(Debug, Default)]
struct ThinkingStream {
    block_open: bool,
    any_sent: bool,
}

impl ThinkingStream {
    /// 返回这一片要转发的文本（空片返回 `None`）。
    fn delta(&mut self, text: &str) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        let content = if !self.block_open && self.any_sent {
            format!("\n\n{text}")
        } else {
            text.to_owned()
        };
        self.block_open = true;
        self.any_sent = true;
        Some(content)
    }

    fn complete(&mut self) {
        self.block_open = false;
    }
}

/// stream-json 的一行。
///
/// 字段名逐条对齐上游 `cursorStreamEvent`（`json` tag 已在上游注释里标明
/// `snake_case` / camelCase 的差异，不能改用 `rename_all` 一把梭）。
#[derive(Debug, Deserialize)]
struct CursorEvent {
    #[serde(rename = "type")]
    kind: String,
    subtype: Option<String>,
    #[serde(rename = "session_id")]
    session_id: Option<String>,
    model: Option<String>,
    /// `assistant` 事件的消息体。
    message: Option<Value>,
    /// `thinking` 事件的推理片段。
    text: Option<String>,
    /// `tool_call` 事件的工具信封（键是工具名 + `ToolCall`）。
    #[serde(rename = "tool_call")]
    tool_call: Option<Value>,
    #[serde(rename = "call_id")]
    call_id: Option<String>,
    /// `tool_use` 事件的工具名 / id（旧版 CLI）。
    #[serde(rename = "tool_name")]
    tool_name: Option<String>,
    #[serde(rename = "tool_id")]
    tool_id: Option<String>,
    parameters: Option<Value>,
    /// `tool_result` 事件的输出（旧版 CLI）。
    output: Option<String>,
    /// `result` 事件的终态文本。
    #[serde(rename = "result")]
    result: Option<String>,
    #[serde(rename = "is_error")]
    is_error: Option<bool>,
    /// `result` 事件的顶层 camelCase 用量。
    #[serde(rename = "inputTokens")]
    input_tokens: Option<u64>,
    #[serde(rename = "outputTokens")]
    output_tokens: Option<u64>,
    #[serde(rename = "cacheReadTokens")]
    cache_read_tokens: Option<u64>,
    #[serde(rename = "cacheWriteTokens")]
    cache_write_tokens: Option<u64>,
    /// `result` 事件的嵌套用量对象（camelCase 与 legacy `snake_case` 两种都出现过）。
    usage: Option<Value>,
    /// `error` 事件的两个诊断字段。
    error: Option<String>,
    detail: Option<String>,
    /// `text` / `step_finish` 事件的复合载荷。
    part: Option<Value>,
}

impl CursorEvent {
    /// `error` 文本的取值优先级：`error` → `detail` → `result`（上游 `cursorErrorText`）。
    fn error_text(&self) -> String {
        for candidate in [
            self.error.as_deref(),
            self.detail.as_deref(),
            self.result.as_deref(),
        ] {
            if let Some(text) = candidate.filter(|text| !text.trim().is_empty()) {
                return text.to_owned();
            }
        }
        "cursor-agent returned an error result without details".to_owned()
    }

    /// 这个 `result` 事件报了吗用量（顶层四个字段任一非零，或带 `usage` 对象）。
    fn has_result_usage(&self) -> bool {
        self.usage.is_some()
            || self.input_tokens.unwrap_or(0) != 0
            || self.output_tokens.unwrap_or(0) != 0
            || self.cache_read_tokens.unwrap_or(0) != 0
            || self.cache_write_tokens.unwrap_or(0) != 0
    }
}

/// 一次 `tool_call` 事件解出来的东西。
#[derive(Debug, Default)]
struct ToolCall {
    call_id: String,
    name: String,
    input: Value,
    result: Option<Value>,
}

/// 解 `tool_call` 信封：`{"readToolCall":{"args":{…},"result":…},"toolCallId":"…"}`。
fn parse_tool_call(event: &CursorEvent) -> ToolCall {
    let mut call = ToolCall {
        call_id: cursor_call_id(event.call_id.as_deref().unwrap_or_default()),
        input: Value::Null,
        ..ToolCall::default()
    };
    let Some(envelope) = event.tool_call.as_ref().and_then(Value::as_object) else {
        return call;
    };
    if call.call_id.is_empty() {
        if let Some(nested) = envelope
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|nested| !nested.is_empty())
        {
            call.call_id = cursor_call_id(nested);
        }
    }
    // 信封里恰好一个 `<name>ToolCall` 键；真出现多个时取字典序第一个（上游同款，
    // 只为了让结果确定，不代表这种载荷存在）。
    let Some(key) = envelope
        .keys()
        .filter(|key| key.len() > TOOL_CALL_KEY_SUFFIX.len() && key.ends_with(TOOL_CALL_KEY_SUFFIX))
        .min()
        .cloned()
    else {
        return call;
    };
    key.trim_end_matches(TOOL_CALL_KEY_SUFFIX)
        .clone_into(&mut call.name);
    let payload = &envelope[&key];
    if let Some(args) = payload.get("args") {
        if !args.is_null() {
            call.input = args.clone();
        }
    }
    if let Some(result) = payload.get("result") {
        if !result.is_null() {
            call.result = Some(result.clone());
        }
    }
    call
}

/// 规范化 call id：Cursor 会把两个 id 打包成一个换行分隔的字符串
/// （`"call-…\nfc_…"`），首行已经唯一，且多行会破坏 daemon 的逐行日志。
fn cursor_call_id(raw: &str) -> String {
    let trimmed = raw.trim();
    match trimmed.split_once('\n') {
        Some((first, _)) => first.trim().to_owned(),
        None => trimmed.to_owned(),
    }
}

/// 一行 stdout 里取出来的一行 JSON（`stdout:` / `stderr:` 前缀由调用方剥，见
/// [`super::normalize_stream_line`]）。
#[derive(Debug)]
pub(crate) struct CursorStreamDecoder {
    state: DecoderState,
    /// 请求里的模型名：事件没带 `model` 时用它（上游 `cursorUsageModel`）。
    fallback_model: String,
    /// 见过 `result`（协议边界）。
    result_seen: bool,
    /// `system/error` / `error` 报出来的协议错误：只在流结束仍无 `result` 时升级为失败。
    protocol_error: Option<String>,
    /// `result` 报过权威用量（此时忽略 `step_finish` 的累加值）。
    has_result_usage: bool,
    result_usage: BTreeMap<String, TokenUsage>,
    step_usage: BTreeMap<String, TokenUsage>,
    thinking: ThinkingStream,
}

impl CursorStreamDecoder {
    /// `fallback_model` = 请求里的模型名。
    pub(crate) fn new(fallback_model: impl Into<String>) -> Self {
        Self {
            state: DecoderState::default(),
            fallback_model: fallback_model.into(),
            result_seen: false,
            protocol_error: None,
            has_result_usage: false,
            result_usage: BTreeMap::new(),
            step_usage: BTreeMap::new(),
            thinking: ThinkingStream::default(),
        }
    }

    /// 协议层错误：发事件、记诊断，但**不**改终态（`result` 还能推翻它）。
    fn protocol_error(&mut self, message: impl Into<String>) -> Vec<RuntimeEvent> {
        let message = message.into();
        if self.protocol_error.is_none() {
            self.protocol_error = Some(message.clone());
        }
        self.state.emit_error(message)
    }

    /// 用量归属的模型名：事件带的 `model` 优先，其次请求里的，最后 `LABEL`。
    fn usage_model(&self, event: &CursorEvent) -> String {
        for candidate in [event.model.as_deref(), Some(self.fallback_model.as_str())] {
            if let Some(model) = candidate.map(str::trim).filter(|model| !model.is_empty()) {
                return model.to_owned();
            }
        }
        LABEL.to_owned()
    }

    fn handle(&mut self, event: &CursorEvent) -> Vec<RuntimeEvent> {
        // 会话 id 每个事件都读一遍（上游同款：任一行报了 id 就更新）。
        self.state.set_session_opt(event.session_id.as_deref());

        match event.kind.as_str() {
            "system" => match event.subtype.as_deref() {
                Some("init") => self.state.progress("running"),
                Some("error") => {
                    let message = event.error_text();
                    self.protocol_error(message)
                }
                _ => Vec::new(),
            },
            "assistant" => self.handle_assistant(event.message.as_ref()),
            "thinking" => match event.subtype.as_deref() {
                // 只认 `delta`（携带推理片段）与 `completed`（收块）：未知 subtype
                // **不**并入推理（上游同款，避免把上游新增的东西猜成推理）。
                Some("delta") => {
                    let content = event.text.clone().unwrap_or_default();
                    match self.thinking.delta(&content) {
                        Some(content) => self.state.thinking(&content),
                        None => Vec::new(),
                    }
                }
                Some("completed") => {
                    self.thinking.complete();
                    Vec::new()
                }
                _ => Vec::new(),
            },
            "tool_call" => match event.subtype.as_deref() {
                // `started` / `completed` 定义一次调用；其它（未来的 `progress` 之类）
                // 不发结果，否则会提前把还在跑的长工具从 daemon 的台账里划掉。
                Some("started") => {
                    let call = parse_tool_call(event);
                    self.state.tool_use(call.call_id, call.name, call.input)
                }
                Some("completed") => {
                    let call = parse_tool_call(event);
                    let output = call
                        .result
                        .map(|result| result.to_string())
                        .unwrap_or_default();
                    self.state.tool_result(call.call_id, output, false)
                }
                _ => Vec::new(),
            },
            "tool_use" => {
                let call_id = event.tool_id.clone().unwrap_or_default();
                let name = event.tool_name.clone().unwrap_or_default();
                let input = event.parameters.clone().unwrap_or(Value::Null);
                self.state.tool_use(call_id, name, input)
            }
            "tool_result" => {
                let call_id = event.tool_id.clone().unwrap_or_default();
                let output = event.output.clone().unwrap_or_default();
                self.state.tool_result(call_id, output, false)
            }
            "result" => self.handle_result(event),
            "error" => {
                let message = event.error_text();
                self.protocol_error(message)
            }
            "text" => match event.part.as_ref().and_then(|part| field_str(part, "text")) {
                Some(text) => self.state.text(&text),
                None => Vec::new(),
            },
            "step_finish" => {
                if let Some(part) = event.part.as_ref() {
                    let model = self.usage_model(event);
                    let entry = self.step_usage.entry(model).or_default();
                    *entry += tokens(
                        field_u64_any(part, &["tokens", "input"]),
                        field_u64_any(part, &["tokens", "output"]),
                        field_u64_any(part, &["tokens", "cache", "read"]),
                        0,
                    );
                }
                Vec::new()
            }
            // 未知类型：静默忽略（差异第 3 条）。
            _ => Vec::new(),
        }
    }

    fn handle_assistant(&mut self, message: Option<&Value>) -> Vec<RuntimeEvent> {
        let Some(message) = message else {
            return Vec::new();
        };
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for block in blocks {
            match field_str(block, "type").as_deref() {
                Some("output_text" | "text") => {
                    if let Some(text) = field_str(block, "text") {
                        out.extend(self.state.text(&text));
                    }
                }
                Some("thinking") => {
                    if let Some(text) = field_str(block, "text") {
                        out.extend(self.state.thinking(&text));
                    }
                }
                Some("tool_use") => {
                    let call_id = field_str(block, "id").unwrap_or_default();
                    let name = field_str(block, "name").unwrap_or_default();
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    out.extend(self.state.tool_use(call_id, name, input));
                }
                _ => {}
            }
        }
        out
    }

    fn handle_result(&mut self, event: &CursorEvent) -> Vec<RuntimeEvent> {
        self.result_seen = true;
        let mut out = Vec::new();
        if event.is_error.unwrap_or(false) || event.subtype.as_deref() == Some("error") {
            // 失败由 `result` 决定（`protocol_error` 那套"等流结束再判"的兜底不再适用）。
            out.extend(self.state.note_error(event.error_text()));
        }
        // 正文兜底：只有**一个**文本事件都没吐过时才用 `result` 文本。
        if !self.state.has_text() {
            if let Some(result) = event.result.as_deref().filter(|text| !text.is_empty()) {
                out.extend(self.state.text(result));
            }
        }
        if event.has_result_usage() {
            let model = self.usage_model(event);
            let entry = self.result_usage.entry(model).or_default();
            // 顶层 camelCase 总量优先，其次嵌套对象（上游 `accumulateResultUsage`）。
            if let Some(usage) = top_level_usage(event) {
                *entry += usage;
            } else if let Some(usage) = event.usage.as_ref() {
                *entry += nested_usage(usage);
            }
            self.has_result_usage = true;
            out.extend(self.state.set_usage_map(usage_map(&self.result_usage)));
        }
        out
    }
}

impl EventDecoder for CursorStreamDecoder {
    fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent> {
        let line = normalize_stream_line(line);
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(event) = serde_json::from_str::<CursorEvent>(&line) else {
            // 非 JSON / 结构对不上的行：忽略（一致性套件的"脏数据"用例就靠这条）。
            return Vec::new();
        };
        self.handle(&event)
    }

    fn finish(&mut self) -> Vec<RuntimeEvent> {
        let mut out = Vec::new();
        // `result` 没报用量 ⇒ 用 `step_finish` 的累加值兜底（上游同款）。
        if !self.has_result_usage && !self.step_usage.is_empty() {
            out.extend(self.state.set_usage_map(usage_map(&self.step_usage)));
        }
        if !self.result_seen {
            // 流结束仍没见到 `result` = 上游的 "stream ended without terminal result"。
            let message = self
                .protocol_error
                .take()
                .unwrap_or_else(|| format!("{LABEL} stream ended without terminal result"));
            out.extend(self.state.note_error(message));
        }
        out
    }
}

impl CliDecoder for CursorStreamDecoder {
    fn summary(&self) -> CliSummary {
        self.state.summary()
    }
}

/// 剥掉 `cursor-agent` 可能打在 JSON 前面的 `stdout:` / `stderr:` 前缀
/// （上游 `normalizeCursorStreamLine`）。
pub(crate) fn normalize_stream_line(raw: &str) -> String {
    let trimmed = raw.trim();
    let lowered = trimmed.to_ascii_lowercase();
    for prefix in ["stdout", "stderr"] {
        if let Some(rest) = lowered.strip_prefix(prefix) {
            let rest = rest.trim_start_matches([' ', ':', '=']).trim_start();
            // 前缀后面必须紧跟着 `{`，否则这行本身就是一个以 "stdout" 开头的正文。
            if rest.starts_with('{') {
                return rest.to_owned();
            }
        }
    }
    trimmed.to_owned()
}
