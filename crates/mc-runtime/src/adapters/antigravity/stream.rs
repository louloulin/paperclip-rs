//! `agy --output-format stream-json` 的解码器（上游 `antigravity.go` 的解析部分）。
//!
//! # 事件 → 事件
//!
//! | stream-json 事件 | 本实现 |
//! |---|---|
//! | `init` | 记 `conversation_id`（= 会话 id）与 `init.model` |
//! | `step_update`（`step_type:"agent_response"`） | `text_delta` 非空 → `Text`；记下"最新一步是否 `state:"done"`"（尾随网络错误容忍要用） |
//! | `step_update`（`state:"done"` 且带用量） | 按 `step_index` **覆盖**该步用量（同一个步会随状态变化重复上报） |
//! | `result` | 记 `conversation_id` / `status` / `error` / `response` / `usage`；终态在 `finish()` 里算 |
//! | 其它 `event` 值 | 忽略 |
//! | **不是事件 JSON 的行** | 当**纯文本**回显（`agy` 老版本只吐纯文本；每行之间补 `\n`） |
//!
//! # 终态归因（`finish()`）
//!
//! 顺序逐条对齐上游 `Execute` 尾部的判定链（**去掉**了依赖 `--log-file` 的三条，
//! 见下）：
//!
//! 1. `result.status` 映射（`antigravityResultStatus`）：空 / `SUCCESS` / `COMPLETED`
//!    → 完成；`CANCELLED` / `CANCELED` → 取消；`ABORTED` → 中止；`TIMEOUT` /
//!    `TIMED_OUT` → 超时；其它 → 失败。**只有"不是完成"才判失败**。
//! 2. 尾随网络错误容忍：`result.error` 等于 `agy` 的连接错误串、且 `response` 非空、
//!    且最新一个 `agent_response` 步已经是 `done` ⇒ **不**判失败（上游
//!    `antigravityCompletedDespiteTrailingNetworkError`）。`agy` 会在答完一整轮之后
//!    才失败一次后续网络操作，把这种"尾随"错误当成整轮失败会丢掉已经拿到的回答。
//! 3. 用量：`step_update` 里 `done` 快照的**去重求和**优先，`result.usage` 只在一步
//!    都没报时兜底（`result.usage` 是被恢复的会话的**累计**值，直接采纳会把前几轮
//!    重复计一遍）。
//!
//! 超时 / 取消由运行骨架（[`super::super::cli_core::run`]）归因，非零退出同理；
//! 上游把 `status` 排在退出码**之前**，本实现由 `finalize` 保留这个优先级并附加
//! 退出码诊断。
//!
//! # 与上游的差异（都记在 `docs/33`）
//!
//! 1. **正文以流为准**：上游 `result.response` 非空时会**覆盖** `output`。本 crate
//!    的 `RunOutcome::output` 定义是"所有 `Text` 事件拼接"，覆盖会让"事件流 == 终态
//!    正文"这条不变量失效（一致性套件就断它）。因此这里只在**一个文本事件都没有**
//!    时用 `response` 兜底产出一条 `Text`（对事件流而言与上游等价），其余情况以
//!    增量拼接为准 —— 两者在单轮里逐字相同。
//! 2. **不写 `--log-file`，因此不做三件依赖日志的事**：(a) 从 glog 行里
//!    `conversation=<uuid>` 抢会话 id；(b) `printmode.go: timed out after N polls`
//!    的 `--print-timeout` 嗅探；(c) `agent executor error:` 的 provider 错误嗅探。
//!    会话 id 只从流里取；`--print-timeout` 超时只能靠 run 自己的墙钟。这三条是
//!    **已知缺口**（`result` 里没有会话 id 时本实现回报 `None`，而上游会救回来）。
//! 3. **不做空 stdout 的 transcript 抢修**（上游 `readAntigravityTranscriptOutput`）：
//!    "completed 但正文为空"在本实现里就是一个空答案。
//! 4. **不做 `agy models` 目录校验**：上游对非空的 `--model` 先查 `agy models`，
//!    不在目录里就拒绝启动（`agy` 遇到不认识的模型会静默空跑、退出 0）。本 crate
//!    的 `launch` 不做这种预检，`--model` 原样透传。

use std::collections::BTreeMap;

use serde_json::Value;

use super::super::cli_core::decoder::{field_str, tokens};
use super::super::cli_core::{CliDecoder, CliSummary, DecoderState};
use super::LABEL;
use crate::adapter::{EventDecoder, ModelUsage, RuntimeEvent, TokenUsage};

/// `agy` 在**答完整轮之后**才冒出来的网络错误串（上游同名字面量）。
const NETWORK_ISSUE_ERROR: &str =
    "There was a network issue connecting to the server, please try again.";

/// 一步 `agent_response` 的类型值。
const STEP_TYPE_AGENT_RESPONSE: &str = "agent_response";

/// `status` 的映射结果（上游 `antigravityResultStatus` 的取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultStatus {
    Completed,
    Cancelled,
    Aborted,
    Timeout,
    Failed,
}

impl ResultStatus {
    fn parse(status: Option<&str>) -> Self {
        match status
            .map(str::trim)
            .map(str::to_ascii_uppercase)
            .as_deref()
        {
            None | Some("" | "SUCCESS" | "COMPLETED") => Self::Completed,
            Some("CANCELLED" | "CANCELED") => Self::Cancelled,
            Some("ABORTED") => Self::Aborted,
            Some("TIMEOUT" | "TIMED_OUT") => Self::Timeout,
            Some(_) => Self::Failed,
        }
    }

    fn is_completed(self) -> bool {
        self == Self::Completed
    }
}

/// `agy` 的用量桶（`thinking_tokens` 已经包含在 `output_tokens` 里，不能重复计）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct StreamUsage {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
}

impl StreamUsage {
    fn parse(value: &Value) -> Self {
        Self {
            input: non_negative(value, "input_tokens"),
            output: non_negative(value, "output_tokens"),
            cache_read: non_negative(value, "cache_read_tokens"),
            cache_write: non_negative(value, "cache_write_tokens"),
        }
    }

    /// 上游 `hasTokens`：四个桶任一非零（`total_tokens` / `thinking_tokens` 不算）。
    fn has_tokens(&self) -> bool {
        [self.input, self.output, self.cache_read, self.cache_write]
            .into_iter()
            .flatten()
            .any(|tokens| tokens != 0)
    }

    fn token_usage(&self) -> TokenUsage {
        tokens(
            self.input.unwrap_or(0),
            self.output.unwrap_or(0),
            self.cache_read.unwrap_or(0),
            self.cache_write.unwrap_or(0),
        )
    }
}

/// 取一个非负整数（缺字段 / 负数 / 非数字 ⇒ `None`）。
fn non_negative(value: &Value, key: &str) -> Option<u64> {
    value.get(key)?.as_u64()
}

/// `agy` 的 stream-json 解码器。
#[derive(Debug)]
pub(crate) struct AntigravityStreamDecoder {
    state: DecoderState,
    /// 请求里的模型名：流里没报 `init.model` 时用它。
    fallback_model: String,
    /// 流里报的 `init.model`（用量归属用）。
    stream_model: Option<String>,
    /// 最新的 `agent_response` 步号与它是否 `done`（默认 -1 / false，上游同款）。
    latest_step: Option<i64>,
    latest_step_done: bool,
    /// `step_index` → 该步 `done` 时的用量快照。
    step_usage: BTreeMap<i64, TokenUsage>,
    result_usage: Option<TokenUsage>,
    result_status: Option<String>,
    result_error: Option<String>,
    result_response: Option<String>,
}

impl AntigravityStreamDecoder {
    pub(crate) fn new(fallback_model: impl Into<String>) -> Self {
        Self {
            state: DecoderState::default(),
            fallback_model: fallback_model.into(),
            stream_model: None,
            latest_step: None,
            latest_step_done: false,
            step_usage: BTreeMap::new(),
            result_usage: None,
            result_status: None,
            result_error: None,
            result_response: None,
        }
    }

    /// 一行事件（已经确认带 `event` 字段）。
    fn handle_event(&mut self, event: &Value) -> Vec<RuntimeEvent> {
        match field_str(event, "event").as_deref() {
            Some("init") => {
                self.state
                    .set_session_opt(field_str(event, "conversation_id").as_deref());
                if let Some(model) = event.get("init").and_then(|init| field_str(init, "model")) {
                    self.stream_model = Some(model);
                }
                Vec::new()
            }
            Some("step_update") => self.handle_step_update(event),
            Some("result") => self.handle_result(event),
            _ => Vec::new(),
        }
    }

    fn handle_step_update(&mut self, event: &Value) -> Vec<RuntimeEvent> {
        let Some(step) = event.get("step_update").filter(|step| step.is_object()) else {
            return Vec::new();
        };
        if let Some(conversation) = field_str(event, "conversation_id") {
            self.state.set_session(&conversation);
        }
        if let Some(conversation) = field_str(step, "conversation_id") {
            self.state.set_session(&conversation);
        }
        let step_type = field_str(step, "step_type");
        let step_index = step.get("step_index").and_then(Value::as_i64);
        let done = field_str(step, "state").is_some_and(|state| state.eq_ignore_ascii_case("done"));

        let mut out = Vec::new();
        if step_type.as_deref() == Some(STEP_TYPE_AGENT_RESPONSE) {
            if let (Some(index), Some(current)) = (step_index, self.latest_step) {
                // 只有**最新**那一步决定"回答完了没有"：先 DONE 的一步后面可能跟着
                // 一个被网络错误掐断的新 ACTIVE 步（上游注释同款）。
                if index >= current {
                    self.latest_step = Some(index);
                    self.latest_step_done = done;
                }
            } else if step_index.is_some() {
                // 首次见到（上游初值 -1 ⇒ 任何下标都算"更新"）。
                self.latest_step = step_index;
                self.latest_step_done = done;
            }
            if let Some(delta) = field_str(step, "text_delta") {
                out.extend(self.state.text(&delta));
            }
        }
        if done {
            if let (Some(index), Some(usage)) = (step_index, step.get("usage")) {
                let usage = StreamUsage::parse(usage);
                if usage.has_tokens() {
                    // 按步号**覆盖**：同一步会随状态变化重复上报。
                    self.step_usage.insert(index, usage.token_usage());
                }
            }
        }
        out
    }

    fn handle_result(&mut self, event: &Value) -> Vec<RuntimeEvent> {
        let Some(result) = event.get("result").filter(|result| result.is_object()) else {
            return Vec::new();
        };
        if let Some(conversation) = field_str(result, "conversation_id") {
            self.state.set_session(&conversation);
        }
        if let Some(conversation) = field_str(event, "conversation_id") {
            self.state.set_session(&conversation);
        }
        self.result_status = field_str(result, "status").or(self.result_status.take());
        self.result_error = field_str(result, "error").or(self.result_error.take());
        self.result_response = field_str(result, "response").or(self.result_response.take());
        if let Some(usage) = result.get("usage") {
            let usage = StreamUsage::parse(usage);
            if usage.has_tokens() {
                self.result_usage = Some(usage.token_usage());
            }
        }
        Vec::new()
    }

    /// 纯文本行：`agy` 老版本（或日志式输出）只吐文本，逐行回显并补 `\n`。
    ///
    /// 拼法与上游逐字对齐：第一行不带前缀，之后每行前缀一个 `\n`；**空行也发**
    /// （`chunk` 非空），只有"第一行就是空行"不发。
    fn plain_text(&mut self, line: &str) -> Vec<RuntimeEvent> {
        let chunk = if self.state.has_text() {
            format!("\n{line}")
        } else {
            line.to_owned()
        };
        if chunk.is_empty() {
            Vec::new()
        } else {
            self.state.text(&chunk)
        }
    }

    /// `result.response` 是否只是"答完之后才失败的尾随网络错误"（上游同名判定）。
    fn completed_despite_trailing_network_error(&self) -> bool {
        let provider_error = self.result_error.as_deref().unwrap_or_default();
        let response = self.result_response.as_deref().unwrap_or_default();
        provider_error
            .trim()
            .eq_ignore_ascii_case(NETWORK_ISSUE_ERROR)
            && !response.trim().is_empty()
            && self.latest_step_done
    }

    /// 用量归属的模型名：`init.model` 优先，其次请求里的模型。
    fn model_for_usage(&self) -> String {
        for candidate in [
            self.stream_model.as_deref(),
            Some(self.fallback_model.as_str()),
        ] {
            if let Some(model) = candidate.map(str::trim).filter(|model| !model.is_empty()) {
                return model.to_owned();
            }
        }
        LABEL.to_owned()
    }
}

impl EventDecoder for AntigravityStreamDecoder {
    fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent> {
        // `event` 字段缺失（含非 JSON 行）⇒ 纯文本路径（上游同款判定）。
        if let Ok(event) = serde_json::from_str::<Value>(line) {
            if field_str(&event, "event").is_some() {
                return self.handle_event(&event);
            }
        }
        self.plain_text(line)
    }

    fn finish(&mut self) -> Vec<RuntimeEvent> {
        let mut out = Vec::new();
        // 正文兜底：一个文本事件都没吐过时才用 `result.response`（差异第 1 条）。
        if !self.state.has_text() {
            if let Some(response) = self
                .result_response
                .as_deref()
                .filter(|response| !response.is_empty())
            {
                out.extend(self.state.text(response));
            }
        }
        // 用量：done 步的去重求和优先，`result.usage` 兜底（差异见模块文档第 3 条）。
        let mut step_total = TokenUsage::default();
        for usage in self.step_usage.values() {
            step_total += *usage;
        }
        let usage = if self.step_usage.is_empty() {
            self.result_usage
        } else {
            Some(step_total)
        };
        if let Some(usage) = usage {
            if usage.total_tokens != 0 {
                let model = self.model_for_usage();
                out.extend(self.state.set_usage_map(vec![ModelUsage { model, usage }]));
            }
        }
        // 终态：只有"不是完成"才判失败；尾随网络错误容忍放行。
        let status = ResultStatus::parse(self.result_status.as_deref());
        if !status.is_completed() && !self.completed_despite_trailing_network_error() {
            let message = self
                .result_error
                .clone()
                .filter(|error| !error.trim().is_empty())
                .unwrap_or_else(|| {
                    format!(
                        "{LABEL} returned status {}",
                        self.result_status.as_deref().unwrap_or("")
                    )
                });
            out.extend(self.state.note_error(message));
        }
        out
    }
}

impl CliDecoder for AntigravityStreamDecoder {
    fn summary(&self) -> CliSummary {
        self.state.summary()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder() -> AntigravityStreamDecoder {
        AntigravityStreamDecoder::new("gemini-3.6-flash-high")
    }

    fn texts(events: &[RuntimeEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::Text { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn result_status_mapping_matches_upstream() {
        for (raw, expected) in [
            (None, ResultStatus::Completed),
            (Some(""), ResultStatus::Completed),
            (Some(" success "), ResultStatus::Completed),
            (Some("COMPLETED"), ResultStatus::Completed),
            (Some("cancelled"), ResultStatus::Cancelled),
            (Some("CANCELED"), ResultStatus::Cancelled),
            (Some("aborted"), ResultStatus::Aborted),
            (Some("timed_out"), ResultStatus::Timeout),
            (Some("failed"), ResultStatus::Failed),
            (Some("还没见过"), ResultStatus::Failed),
        ] {
            assert_eq!(ResultStatus::parse(raw), expected, "{raw:?}");
        }
    }

    #[test]
    fn step_deltas_are_streamed_and_done_usage_replaces() {
        let mut decoder = decoder();
        decoder.push_line(
            r#"{"event":"init","conversation_id":"c1","init":{"model":"gemini-3.6-flash-high"}}"#,
        );
        let first = decoder.push_line(
            r#"{"event":"step_update","step_update":{"step_index":0,"state":"active","step_type":"agent_response","text_delta":"o"}}"#,
        );
        assert_eq!(texts(&first), "o");
        // 同一步 `done` 时重发一次快照：替换而不是叠加。
        decoder.push_line(
            r#"{"event":"step_update","step_update":{"step_index":0,"state":"active","step_type":"agent_response","text_delta":"k","usage":{"input_tokens":1,"output_tokens":1}}}"#,
        );
        decoder.push_line(
            r#"{"event":"step_update","step_update":{"step_index":0,"state":"done","step_type":"agent_response","text_delta":"","usage":{"input_tokens":4,"output_tokens":6}}}"#,
        );
        let summary = decoder.summary();
        assert_eq!(summary.output, "ok");
        assert_eq!(summary.session_id.as_deref(), Some("c1"));
        let events = decoder.finish();
        let usage: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::Usage { model, usage } => Some((model.clone(), *usage)),
                _ => None,
            })
            .collect();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].0, "gemini-3.6-flash-high");
        // 1+1 的 ACTIVE 快照被 4+6 的 DONE 快照替换 ⇒ 10。
        assert_eq!(usage[0].1.total_tokens, 10);
        assert_eq!(summary.terminal_error, None);
    }

    #[test]
    fn thinking_tokens_are_not_counted_twice() {
        let usage = StreamUsage::parse(&serde_json::json!({
            "input_tokens": 1,
            "output_tokens": 2,
            "thinking_tokens": 100,
            "total_tokens": 103,
        }));
        assert_eq!(usage.token_usage().total_tokens, 3);
    }

    #[test]
    fn only_the_latest_agent_step_decides_done() {
        let mut decoder = decoder();
        // 先 DONE 的一步，再跟一个被掐断的 ACTIVE 步 ⇒ 不算"答完了"。
        decoder.push_line(
            r#"{"event":"step_update","step_update":{"step_index":1,"state":"done","step_type":"agent_response","text_delta":"a"}}"#,
        );
        decoder.push_line(
            r#"{"event":"step_update","step_update":{"step_index":2,"state":"active","step_type":"agent_response","text_delta":"b"}}"#,
        );
        decoder.push_line(
            r#"{"event":"result","result":{"status":"FAILED","error":"There was a network issue connecting to the server, please try again.","response":"ab"}}"#,
        );
        // 最新一步没 done ⇒ 尾随网络错误不容忍，判失败。
        assert!(
            decoder.summary().terminal_error.is_none(),
            "终态在 finish() 里才定"
        );
        let events = decoder.finish();
        assert!(matches!(events.last(), Some(RuntimeEvent::Error { .. })));
        assert_eq!(
            decoder.summary().terminal_error.as_deref(),
            Some("There was a network issue connecting to the server, please try again.")
        );
    }

    #[test]
    fn a_trailing_network_error_after_a_done_answer_is_tolerated() {
        let mut decoder = decoder();
        decoder.push_line(
            r#"{"event":"step_update","step_update":{"step_index":0,"state":"done","step_type":"agent_response","text_delta":"ok"}}"#,
        );
        decoder.push_line(
            r#"{"event":"result","result":{"status":"FAILED","error":"There was a network issue connecting to the server, please try again.","response":"ok"}}"#,
        );
        // 先记住 status/error/response，`finish()` 里才判。
        assert!(decoder.summary().terminal_error.is_none());
        assert!(decoder.finish().is_empty());
        assert_eq!(decoder.summary().terminal_error, None);
    }

    #[test]
    fn a_failed_status_without_a_response_is_a_failure() {
        let mut decoder = decoder();
        decoder.push_line(r#"{"event":"result","result":{"status":"FAILED"}}"#);
        let events = decoder.finish();
        let Some(RuntimeEvent::Error { message }) = events.first() else {
            panic!("应出 Error，实际 {events:?}");
        };
        assert!(message.contains("returned status FAILED"), "{message}");
    }

    #[test]
    fn plain_text_lines_are_joined_with_newlines() {
        let mut decoder = decoder();
        assert_eq!(texts(&decoder.push_line("第一行")), "第一行");
        assert_eq!(texts(&decoder.push_line("第二行")), "\n第二行");
        // 空行也发（`chunk` = "\n"），否则 markdown 的段落会被挤成一行。
        assert_eq!(texts(&decoder.push_line("")), "\n");
        assert_eq!(decoder.summary().output, "第一行\n第二行\n");
    }

    #[test]
    fn a_json_line_without_event_is_plain_text() {
        let mut decoder = decoder();
        let events = decoder.push_line(r#"{"type":"unknown","payload":1}"#);
        assert_eq!(texts(&events), r#"{"type":"unknown","payload":1}"#);
    }

    #[test]
    fn result_response_is_only_a_fallback_for_the_body() {
        let mut decoder = decoder();
        decoder
            .push_line(r#"{"event":"result","result":{"status":"SUCCESS","response":"兜底正文"}}"#);
        let events = decoder.finish();
        assert_eq!(texts(&events), "兜底正文");
        assert_eq!(decoder.summary().output, "兜底正文");
    }

    #[test]
    fn result_usage_is_the_fallback_for_the_usage() {
        let mut decoder = decoder();
        decoder.push_line(
            r#"{"event":"result","result":{"status":"SUCCESS","response":"ok","usage":{"input_tokens":7,"output_tokens":8}}}"#,
        );
        let events = decoder.finish();
        let total: u64 = events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::Usage { usage, .. } => Some(usage.total_tokens),
                _ => None,
            })
            .sum();
        assert_eq!(total, 15);
    }
}
