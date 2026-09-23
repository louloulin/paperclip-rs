//! CLI 类 adapter 共用的解码器骨架（[`CliDecoder`] + [`DecoderState`]）。
//!
//! # 为什么要有 [`DecoderState`]
//!
//! 「文本增量怎么拼成终态正文」「用量按模型累加」「会话 id 从哪来」这三件事，
//! 7 个新 adapter 里一模一样。写成一份，[`crate::adapter::RunOutcome`] 的语义才
//! 只需在一处守住：
//!
//! - `output` = **所有 `Text` 事件拼接**（`adapter.rs` 对 `RunOutcome::output` 的
//!   定义就是"各 turn 的文本增量拼接"）。一致性套件会断言
//!   `文本事件拼接 == outcome.output`，所以任何"用终态结果覆盖正文"的写法都会红；
//! - `usage` = 按模型累加（[`DecoderState::add_usage`]）或覆盖
//!   （[`DecoderState::set_usage`]，给"上报累计值"的协议用，如 codex）；
//! - `session_id` = 协议里报的会话 id（空串按"没报"处理）。
//!
//! # 与上游的取舍
//!
//! 上游（`server/pkg/agent/*.go`）里 `output` 的口径并不统一：claude 用 `result`
//! 事件的最终文本、copilot 用"最后一个完整 turn"。本 crate 统一成"拼接"，
//! 逐 provider 的差异记在 `docs/33`。

use std::collections::BTreeMap;

use crate::adapter::{EventDecoder, ModelUsage, RuntimeEvent, TokenUsage};

/// 解码器在 stdout 结束后交给 run 循环的汇总。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliSummary {
    /// 正文（= 所有 `Text` 事件的拼接）。
    pub output: String,
    /// 协议里报出来的会话 id。
    pub session_id: Option<String>,
    /// 按模型的用量（顺序稳定）。
    pub usage: Vec<ModelUsage>,
    /// 协议层报出的失败（非零退出之外的失败信号）。
    pub terminal_error: Option<String>,
    /// 事件计数（日志用）。
    pub text_events: usize,
    /// `Thinking` 事件数。
    pub thinking_events: usize,
    /// `ToolUse` 事件数。
    pub tool_events: usize,
}

/// 带汇总能力的 [`EventDecoder`]。
///
/// 后三个方法有默认实现（返回空）：只有请求/应答式协议（codex 的 app-server）
/// 才需要"往 stdin 写帧"。
pub trait CliDecoder: EventDecoder + Send {
    /// 汇总（run 循环在 `finish()` 之后取一次）。
    fn summary(&self) -> CliSummary;

    /// 一次性取走"读到某些事件后要写回 stdin 的帧"。
    fn take_outbox(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// spawn 之后立刻要写的帧（握手第一步）。
    fn initial_frames(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// 取消时补发的帧（如 codex 的 `turn/interrupt`），尽力而为。
    fn cancel_frames(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// 文本 / 用量 / 会话 / 错误状态的累加器。
#[derive(Debug, Default)]
pub struct DecoderState {
    output: String,
    session_id: Option<String>,
    usage: BTreeMap<String, TokenUsage>,
    terminal_error: Option<String>,
    text_events: usize,
    thinking_events: usize,
    tool_events: usize,
}

impl DecoderState {
    /// 记录会话 id（空/空白串忽略）。
    pub fn set_session(&mut self, raw: &str) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            self.session_id = Some(trimmed.to_owned());
        }
    }

    /// 记录会话 id（`None` 忽略）。
    pub fn set_session_opt(&mut self, raw: Option<&str>) {
        if let Some(raw) = raw {
            self.set_session(raw);
        }
    }

    /// 会话 id。
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// 正文（供 decoder 内部判断"是否已经吐过文本"）。
    pub fn output(&self) -> &str {
        &self.output
    }

    /// 正文是否为空。
    pub fn has_text(&self) -> bool {
        !self.output.is_empty()
    }

    /// 文本增量：累加进正文 + 出一条 `Text`。
    pub fn text(&mut self, delta: &str) -> Vec<RuntimeEvent> {
        if delta.is_empty() {
            return Vec::new();
        }
        self.output.push_str(delta);
        self.text_events += 1;
        vec![RuntimeEvent::Text {
            delta: delta.to_owned(),
        }]
    }

    /// 推理增量。
    pub fn thinking(&mut self, delta: &str) -> Vec<RuntimeEvent> {
        if delta.is_empty() {
            return Vec::new();
        }
        self.thinking_events += 1;
        vec![RuntimeEvent::Thinking {
            delta: delta.to_owned(),
        }]
    }

    /// 工具调用。
    pub fn tool_use(
        &mut self,
        call_id: impl Into<String>,
        tool: impl Into<String>,
        input: serde_json::Value,
    ) -> Vec<RuntimeEvent> {
        self.tool_events += 1;
        vec![RuntimeEvent::ToolUse {
            call_id: call_id.into(),
            tool: tool.into(),
            input,
        }]
    }

    /// 工具结果。
    pub fn tool_result(
        &mut self,
        call_id: impl Into<String>,
        output: impl Into<String>,
        is_error: bool,
    ) -> Vec<RuntimeEvent> {
        vec![RuntimeEvent::ToolResult {
            call_id: call_id.into(),
            output: output.into(),
            is_error,
        }]
    }

    /// 进度。
    pub fn progress(&self, status: &str) -> Vec<RuntimeEvent> {
        vec![RuntimeEvent::Progress {
            status: status.to_owned(),
        }]
    }

    /// **只**发一条 `Error` 事件（不改变终态）。
    pub fn emit_error(&mut self, message: impl Into<String>) -> Vec<RuntimeEvent> {
        vec![RuntimeEvent::Error {
            message: message.into(),
        }]
    }

    /// 发 `Error` 事件 **并**把 run 判为失败（协议层失败信号）。
    ///
    /// 第一个失败信号胜出：后续信号只发事件不覆盖错误串（上游同款：`finalError`
    /// 一旦写上就不再改）。
    pub fn note_error(&mut self, message: impl Into<String>) -> Vec<RuntimeEvent> {
        let message = message.into();
        if self.terminal_error.is_none() {
            self.terminal_error = Some(message.clone());
        }
        vec![RuntimeEvent::Error { message }]
    }

    /// 协议层失败串（没有就是 `None`）。
    pub fn terminal_error(&self) -> Option<&str> {
        self.terminal_error.as_deref()
    }

    /// 按模型**累加**用量并出一条 `Usage` 事件（增量式上报的协议）。
    pub fn add_usage(&mut self, model: &str, usage: TokenUsage) -> Vec<RuntimeEvent> {
        let entry = self.usage.entry(model.to_owned()).or_default();
        *entry += usage;
        vec![RuntimeEvent::Usage {
            model: model.to_owned(),
            usage,
        }]
    }

    /// 按模型**覆盖**用量并出一条 `Usage` 事件（累计值上报的协议）。
    pub fn set_usage(&mut self, model: &str, usage: TokenUsage) -> Vec<RuntimeEvent> {
        self.usage.insert(model.to_owned(), usage);
        vec![RuntimeEvent::Usage {
            model: model.to_owned(),
            usage,
        }]
    }

    /// **整体**覆盖用量表（多来源择一上报的协议，如 copilot 的
    /// `session.shutdown` / `assistant.usage` / `assistant.message` 三者只能取一）：
    /// 已消失的模型条目会被删掉，不会和旧来源叠加。
    pub fn set_usage_map(&mut self, usage: Vec<ModelUsage>) -> Vec<RuntimeEvent> {
        self.usage = usage
            .iter()
            .map(|entry| (entry.model.clone(), entry.usage))
            .collect();
        usage
            .into_iter()
            .map(|entry| RuntimeEvent::Usage {
                model: entry.model,
                usage: entry.usage,
            })
            .collect()
    }

    /// 取汇总（克隆一份，`self` 留给 `finish()` 继续用）。
    pub fn summary(&self) -> CliSummary {
        CliSummary {
            output: self.output.clone(),
            session_id: self.session_id.clone(),
            usage: self
                .usage
                .iter()
                .map(|(model, usage)| ModelUsage {
                    model: model.clone(),
                    usage: *usage,
                })
                .collect(),
            terminal_error: self.terminal_error.clone(),
            text_events: self.text_events,
            thinking_events: self.thinking_events,
            tool_events: self.tool_events,
        }
    }
}

/// `usage` 的常用构造（`total` 由四段相加，避免各 provider 算错）。
pub fn tokens(input: u64, output: u64, cache_read: u64, cache_write: u64) -> TokenUsage {
    TokenUsage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + output + cache_read + cache_write,
    }
}

/// 从 JSON 里取 `u64`（容忍字符串 / 浮点 / 负数，取不到算 0）。
pub fn json_u64(value: Option<&serde_json::Value>) -> u64 {
    match value {
        Some(serde_json::Value::Number(n)) => u64::try_from(n.as_i64().unwrap_or(0)).unwrap_or(0),
        Some(serde_json::Value::String(s)) => {
            u64::try_from(s.trim().parse::<i64>().unwrap_or(0)).unwrap_or(0)
        }
        _ => 0,
    }
}

/// 从 JSON 对象里按 key 取 `u64`。
pub fn field_u64(value: &serde_json::Value, key: &str) -> u64 {
    json_u64(value.get(key))
}

/// 从 JSON 里取字符串（非字符串 / 空串 → `None`）。
pub fn json_str(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// 从 JSON 对象里按 key 取字符串。
pub fn field_str(value: &serde_json::Value, key: &str) -> Option<String> {
    json_str(value.get(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_deltas_accumulate_into_output() {
        let mut state = DecoderState::default();
        assert!(state.text("").is_empty());
        assert_eq!(
            state.text("a"),
            vec![RuntimeEvent::Text { delta: "a".into() }]
        );
        state.text("b");
        assert_eq!(state.output(), "ab");
        assert_eq!(state.summary().output, "ab");
        assert_eq!(state.summary().text_events, 2);
    }

    #[test]
    fn first_error_wins() {
        let mut state = DecoderState::default();
        state.note_error("boom");
        state.note_error("later");
        assert_eq!(state.terminal_error(), Some("boom"));
        assert_eq!(state.summary().terminal_error.as_deref(), Some("boom"));
    }

    #[test]
    fn add_usage_accumulates_and_set_usage_replaces() {
        let mut state = DecoderState::default();
        state.add_usage("m", tokens(1, 2, 0, 0));
        state.add_usage("m", tokens(3, 0, 0, 0));
        assert_eq!(state.summary().usage[0].usage.total_tokens, 6);
        state.set_usage("m", tokens(1, 2, 0, 0));
        assert_eq!(state.summary().usage[0].usage.total_tokens, 3);
    }

    #[test]
    fn blank_session_id_is_ignored() {
        let mut state = DecoderState::default();
        state.set_session("   ");
        assert_eq!(state.session_id(), None);
        state.set_session(" s1 ");
        assert_eq!(state.session_id(), Some("s1"));
    }

    #[test]
    fn json_helpers_are_lossy_but_safe() {
        let value: serde_json::Value =
            serde_json::json!({"a": 7, "b": "9", "c": -3, "d": "x", "e": null});
        assert_eq!(field_u64(&value, "a"), 7);
        assert_eq!(field_u64(&value, "b"), 9);
        assert_eq!(field_u64(&value, "c"), 0);
        assert_eq!(field_u64(&value, "d"), 0);
        assert_eq!(field_u64(&value, "missing"), 0);
        assert_eq!(field_str(&value, "b"), Some("9".to_owned()));
        assert_eq!(field_str(&value, "e"), None);
    }
}
