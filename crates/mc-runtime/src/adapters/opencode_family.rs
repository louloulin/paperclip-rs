//! `OpenCode` 引擎系的**共用** NDJSON 解码器（`opencode` / `codearts` / `deveco`）。
//!
//! 三个 provider 的 stdout 是同一套事件结构（上游 `opencode.go` /
//! `codearts.go` / `deveco.go` 三份 `processEvents` 的字段名逐一对齐：
//! `type` / `sessionID` / `part.{text,tool,callID,state,tokens,cache,reason}` /
//! `error.{name,data.message}`），因此这里只写一份。
//!
//! # 事件 → 事件
//!
//! | NDJSON 事件 | 本实现 |
//! |---|---|
//! | 信封上的 `sessionID`（任何事件） | 记会话 id |
//! | `text`（`part.text`） | `Text` |
//! | `tool_use`（`part.tool` / `part.callID` / `part.state`） | `ToolUse`；`state.status` 是 `completed`/`error` 时再补一条 `ToolResult` |
//! | `error`（`error.data.message` → `error.name` → 兜底串） | `Error` **并**把 run 判死（OpenCode 系会带 0 退出码报错，退出码不可信） |
//! | `step_start` | `Progress { status: "running" }` |
//! | `step_finish`（`part.tokens`） | 按模型**累加**用量 + `Usage` 事件 |
//!
//! # 与上游的差异（记在 `docs/33`）
//!
//! 1. **fail-closed 判据只做了两条**：上游 opencode / codearts 还按
//!    `step_finish.reason == "tool-calls"` 与"空 step"（无文本、无工具、无用量）
//!    两条判据拒绝假绿终态。那两条要按 step 记账 `reason` 与用量非零性，
//!    本片没做，留给 M4。
//! 2. **`reasoning` 不折算进 `output`**：上游在"1.x ≥ 1.3.16"的版本区间里把
//!    `tokens.reasoning` 加进 `output`。解码器手上没有 CLI 版本（版本探测在
//!    `probe_version` 里，与事件流是两条路），因此不做这个版本相关折算。
//! 3. **deveco 不开 fail-closed**：上游 `deveco.go` 的 `processEvents` 没有 step
//!    记账，干净 EOF 就算完成。为保持行为一致，`strict = false` 只给 deveco。

use serde::Deserialize;
use serde_json::Value;

use super::cli_core::decoder::{field_str, tokens};
use super::cli_core::{CliDecoder, CliSummary, DecoderState};
use crate::adapter::{EventDecoder, RuntimeEvent};

/// `OpenCode` 引擎 NDJSON 的一行。
#[derive(Debug, Deserialize)]
struct OpenCodeEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "sessionID")]
    session_id: Option<String>,
    part: Option<Value>,
    error: Option<Value>,
}

/// 非 JSON 行的留档上限（上游 `unparsedOutput` 同值）。
const UNPARSED_LIMIT: usize = 4096;

/// 共用解码器。
#[derive(Debug)]
pub(crate) struct OpenCodeFamilyDecoder {
    state: DecoderState,
    label: &'static str,
    fallback_model: String,
    /// 是否启用 fail-closed 判据（opencode / codearts 开，deveco 关）。
    strict: bool,
    open_step: bool,
    parsed_events: usize,
    unparsed: String,
}

impl OpenCodeFamilyDecoder {
    /// `label` 用于失败串前缀，`fallback_model` = 请求里的模型名。
    pub(crate) fn new(
        label: &'static str,
        fallback_model: impl Into<String>,
        strict: bool,
    ) -> Self {
        Self {
            state: DecoderState::default(),
            label,
            fallback_model: fallback_model.into(),
            strict,
            open_step: false,
            parsed_events: 0,
            unparsed: String::new(),
        }
    }

    fn handle(&mut self, event: &OpenCodeEvent) -> Vec<RuntimeEvent> {
        self.parsed_events += 1;
        if let Some(session_id) = event.session_id.as_deref() {
            self.state.set_session(session_id);
        }
        match event.kind.as_str() {
            "text" => {
                let text = event
                    .part
                    .as_ref()
                    .and_then(|part| field_str(part, "text"))
                    .unwrap_or_default();
                self.state.text(&text)
            }
            "tool_use" => self.handle_tool_use(event.part.as_ref()),
            "error" => {
                let message = error_message(event.error.as_ref());
                self.state.note_error(message)
            }
            "step_start" => {
                self.open_step = true;
                self.state.progress("running")
            }
            "step_finish" => {
                self.open_step = false;
                self.add_step_usage(event.part.as_ref())
            }
            // 上游只认这五种；未知类型（协议演进）静默忽略。
            _ => Vec::new(),
        }
    }

    fn handle_tool_use(&mut self, part: Option<&Value>) -> Vec<RuntimeEvent> {
        let Some(part) = part else {
            return Vec::new();
        };
        let call_id = field_str(part, "callID").unwrap_or_default();
        let tool = field_str(part, "tool").unwrap_or_default();
        let state = part.get("state");
        let input = state
            .and_then(|state| state.get("input"))
            .cloned()
            .unwrap_or(Value::Null);
        let mut out = self.state.tool_use(call_id.clone(), tool, input);
        let (status, output, error) = match state {
            Some(state) => (
                field_str(state, "status"),
                state.get("output").cloned(),
                field_str(state, "error"),
            ),
            None => (None, None, None),
        };
        if matches!(status.as_deref(), Some("completed" | "error")) {
            let is_error = status.as_deref() == Some("error");
            let text = match (is_error, error) {
                (true, Some(error)) if !error.is_empty() => error,
                _ => match output {
                    Some(Value::String(text)) => text,
                    Some(other) => other.to_string(),
                    None => String::new(),
                },
            };
            out.extend(self.state.tool_result(call_id, text, is_error));
        }
        out
    }

    fn add_step_usage(&mut self, part: Option<&Value>) -> Vec<RuntimeEvent> {
        let Some(part) = part else {
            return Vec::new();
        };
        let Some(tokens_value) = part.get("tokens").filter(|value| !value.is_null()) else {
            return Vec::new();
        };
        let cache = tokens_value.get("cache");
        let usage = tokens(
            super::cli_core::decoder::field_u64(tokens_value, "input"),
            super::cli_core::decoder::field_u64(tokens_value, "output"),
            cache.map_or(0, |cache| {
                super::cli_core::decoder::field_u64(cache, "read")
            }),
            cache.map_or(0, |cache| {
                super::cli_core::decoder::field_u64(cache, "write")
            }),
        );
        if usage.total_tokens == 0 {
            return Vec::new();
        }
        let model = self.fallback_model.clone();
        self.state.add_usage(&model, usage)
    }

    /// 非 JSON 行留档（截断到 [`UNPARSED_LIMIT`]），供 fail-closed 失败串引用。
    fn record_unparsed(&mut self, line: &str) {
        if self.unparsed.len() >= UNPARSED_LIMIT {
            return;
        }
        if !self.unparsed.is_empty() {
            self.unparsed.push('\n');
        }
        let remaining = UNPARSED_LIMIT - self.unparsed.len();
        let line: String = line.chars().take(remaining).collect();
        self.unparsed.push_str(&line);
    }
}

impl EventDecoder for OpenCodeFamilyDecoder {
    fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent> {
        if let Ok(event) = serde_json::from_str::<OpenCodeEvent>(line) {
            self.handle(&event)
        } else {
            self.record_unparsed(line);
            Vec::new()
        }
    }

    fn finish(&mut self) -> Vec<RuntimeEvent> {
        if !self.strict {
            return Vec::new();
        }
        let label = self.label;
        // OpenCode 系没有终态结果事件，"没看到 error" 不等于"跑完了"：干净 EOF
        // 但 step 还开着 ⇒ 流是被掐断的，按失败收尾（上游同款 fail-closed）。
        if self.parsed_events == 0 {
            let detail = self.unparsed.clone();
            let message = if detail.trim().is_empty() {
                format!("{label} returned no parseable JSON events")
            } else {
                format!(
                    "{label} returned no parseable JSON events: {}",
                    detail.trim()
                )
            };
            return self.state.note_error(message);
        }
        if self.open_step {
            return self.state.note_error(format!(
                "{label} stream ended without a terminal signal (step still open at EOF)"
            ));
        }
        Vec::new()
    }
}

impl CliDecoder for OpenCodeFamilyDecoder {
    fn summary(&self) -> CliSummary {
        self.state.summary()
    }
}

/// 错误串：`error.data.message` → `error.name` → `error.message` → 兜底串。
fn error_message(error: Option<&Value>) -> String {
    let Some(error) = error else {
        return "unknown opencode error".to_owned();
    };
    if let Some(message) = error
        .get("data")
        .and_then(|data| field_str(data, "message"))
        .filter(|message| !message.is_empty())
    {
        return message;
    }
    for key in ["name", "message", "error"] {
        if let Some(text) = field_str(error, key) {
            return text;
        }
    }
    "unknown opencode error".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &[&str], strict: bool) -> (Vec<RuntimeEvent>, CliSummary) {
        let mut decoder = OpenCodeFamilyDecoder::new("opencode", "m", strict);
        let mut events = Vec::new();
        for line in lines {
            events.extend(decoder.push_line(line));
        }
        events.extend(decoder.finish());
        (events, decoder.summary())
    }

    const STEP_START: &str = r#"{"type":"step_start","sessionID":"ses_1","part":{}}"#;
    const TEXT: &str = r#"{"type":"text","sessionID":"ses_1","part":{"text":"ok"}}"#;
    const STEP_FINISH: &str = r#"{"type":"step_finish","sessionID":"ses_1","part":{"reason":"stop","tokens":{"input":10,"output":5,"cache":{"read":1,"write":0}}}}"#;

    #[test]
    fn text_session_and_usage_are_accumulated() {
        let (events, summary) = feed(&[STEP_START, TEXT, STEP_FINISH], true);
        let text: String = events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::Text { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "ok");
        assert_eq!(summary.session_id.as_deref(), Some("ses_1"));
        assert_eq!(summary.usage[0].usage.total_tokens, 16);
        assert_eq!(summary.terminal_error, None);
    }

    #[test]
    fn tool_use_pairs_with_a_result_on_terminal_state() {
        let (events, summary) = feed(
            &[
                r#"{"type":"tool_use","sessionID":"ses","part":{"tool":"bash","callID":"c1","state":{"status":"completed","input":{"cmd":"ls"},"output":"a\nb"}}}"#,
            ],
            true,
        );
        assert_eq!(summary.tool_events, 1);
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::ToolUse { call_id, tool, .. } if call_id == "c1" && tool == "bash"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::ToolResult { output, is_error, .. } if output == "a\nb" && !is_error
        )));
    }

    #[test]
    fn error_event_fails_the_run_even_without_a_nonzero_exit() {
        let (_, summary) = feed(
            &[
                r#"{"type":"error","error":{"name":"UnknownError","data":{"message":"invalid model"}}}"#,
            ],
            true,
        );
        assert_eq!(summary.terminal_error.as_deref(), Some("invalid model"));
    }

    #[test]
    fn strict_mode_fails_closed_on_open_step_or_no_events() {
        let (_, summary) = feed(&[STEP_START, TEXT], true);
        assert!(summary
            .terminal_error
            .as_deref()
            .unwrap_or_default()
            .contains("step still open at EOF"));
        let (_, summary) = feed(&["\x1b[31mError: AK 未配置\x1b[0m", "more junk"], true);
        let error = summary.terminal_error.unwrap_or_default();
        assert!(error.contains("no parseable JSON events"), "{error}");
        assert!(error.contains("AK 未配置"), "{error}");
        // 宽松模式（deveco）同一条流不发失败信号。
        let (_, summary) = feed(&[STEP_START, TEXT], false);
        assert_eq!(summary.terminal_error, None);
    }

    #[test]
    fn balanced_step_in_strict_mode_stays_completed_and_junk_lines_are_ignored() {
        let (_, summary) = feed(&["junk", STEP_START, TEXT, STEP_FINISH, "tail junk"], true);
        assert_eq!(summary.terminal_error, None);
        assert_eq!(summary.output, "ok");
    }
}
