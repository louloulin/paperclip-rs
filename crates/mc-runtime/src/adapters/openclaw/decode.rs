//! `openclaw` 的解码器（上游 `server/pkg/agent/openclaw.go` + `openclaw_stdout.go`）。
//!
//! # 两种 stdout 形态
//!
//! `openclaw agent --json` 的 stdout 上可能出现两类东西（可混在同一段流里）：
//!
//! 1. **一个最终结果 blob**（`{"payloads": […], "meta": {...}}`，2026.5.x 的现役格式，
//!    通常是**跨多行**的 pretty-print，且前面可能跟着若干行非 JSON 日志）；
//! 2. **NDJSON 逐条事件**（`type` = `text` / `tool_use` / `tool_result` / `error` /
//!    `lifecycle` / `step_start` / `step_finish`）—— 为前向兼容留的口子，
//!    当前 openclaw 并不产出。
//!
//! 上游先整段读干 stdout，再"整段能否解析成结果 blob"优先、逐行扫描兜底。
//! 本 crate 的 [`EventDecoder`] 是**逐行驱动**的，于是同一个判据改成**每来一行就试一次**
//! 整段解析：一旦 `buffer` 能解析成结果 blob，立刻交付正文/会话/用量（快路径），
//! 后续行忽略；快路径一直不成立时才在逐行路径上处理事件与日志行。
//! 这与上游的轮询判据（"buffer 能否解析成完整结果"）是同一个谓词，只是采样点从
//! 100ms 定时器挪到"每行"。
//!
//! # 与上游的差异
//!
//! 1. **终态正文口径**：上游把 `scanResult.output` 直接塞进 `Result.Output`；本 crate
//!    的 `RunOutcome::output` 统一是"`Text` 事件拼接"（`docs/33` §5）。两种路径下都
//!    对齐了：快路径/结果行把每个 `payloads[].text` 发成 `Text`；**纯日志兜底**路径
//!    （上游 `gotEvents == false`）额外发**一条** `Text`，承载上游塞进 `Result.Output`
//!    的那段 trimmed 文本。
//! 2. **没有 idle-grace 提前收尾**：上游 `readOpenclawStdout` 在"buffer 已成完整结果
//!    **且** stdout 静默 ≥2s"时提前返回 `cutShort`，由调用方 kill 掉那个"交完结果却
//!    不退出"的进程。逐行解码器里没有"静默"这个概念（也没有让解码器喊停的手段），
//!    因此这种 run 会一直挂到 `LaunchRequest::timeout` 才收尾（`docs/33` §11）。
//! 3. **没有最低版本闸门**：上游 `checkOpenclawVersion` 在启动前挡掉 `< 2026.5.5`
//!    （那些版本把 JSON 写 stderr）；本 crate 只有 `probe_version()` 的 semver 解析，
//!    没有"探测值 → 拒绝启动"这条通路（`docs/33` §11）。
//! 4. **`--agent` 的语义没变**：上游注释写明 `opts.Model` 对 openclaw 是"注册过的
//!    agent 名"，这里照旧透传。
//! 5. **用量字段容忍字符串**：上游 `openclawInt64` 只认 JSON 数字；这里复用 crate 的
//!    [`cli_core::decoder::field_u64`]（数字与数字串都认），与其它 adapter 一致。
//! 6. **`step_finish` 的用量在 `finish()` 才上报**：上游把所有 `step_finish` 累加进一个
//!    `TokenUsage`、最后统一按"结果 blob 的 model → `opts.Model` → `unknown`"落表；
//!    这里同样累加、同样在收尾时落表（与快路径的即时上报互斥，不会重复计数）。
//!
//! [`EventDecoder`]: crate::adapter::EventDecoder
//! [`cli_core::decoder::field_u64`]: super::super::cli_core::decoder::field_u64

use serde_json::{Map, Value};

use super::super::cli_core::decoder::{json_u64, tokens, CliDecoder, CliSummary, DecoderState};
use crate::adapter::{EventDecoder, LaunchRequest, RuntimeEvent, TokenUsage};

/// 上游 `openclawNoParseableOutput`：外部日志告警依赖这个**逐字**串，不要改。
pub(crate) const NO_PARSEABLE_OUTPUT: &str = "openclaw returned no parseable output";

/// 上游 `openclawNoParseableOutput` 之外的最后兜底错误串。
const UNKNOWN_ERROR: &str = "unknown openclaw error";

/// openclaw 的 stdout 解码器（快路径 + 逐行兜底）。
pub(crate) struct OpenclawDecoder {
    state: DecoderState,
    /// `LaunchRequest.model`（对 openclaw 是 agent 名）：结果 blob 没报 model 时用它。
    fallback_model: String,
    /// 逐行累积的 trimmed 行（快路径的输入，也是"纯日志兜底"的原文）。
    buffer: String,
    /// 非 JSON 的日志行（上游 `rawLines`）。
    raw_lines: Vec<String>,
    /// `step_finish` 累加出来的用量（在 `finish()` 落表）。
    stream_usage: TokenUsage,
    /// 逐行路径上某个结果 blob 报出的用量（非零才记，覆盖 `stream_usage`）。
    result_usage: Option<TokenUsage>,
    /// 结果 blob 报出的真实模型名（`meta.agentMeta.model`）。
    model: String,
    /// 是否已经解析出过任何结构化内容（事件或结果）。
    got_events: bool,
    /// 整段结果 blob 已经交付过（此后所有行都忽略 —— 上游在快路径上直接返回）。
    fast_path_done: bool,
    /// 用量是否已经上报（快路径即时上报过就不再补）。
    usage_done: bool,
}

impl OpenclawDecoder {
    pub(crate) fn new(request: &LaunchRequest) -> Self {
        Self {
            state: DecoderState::default(),
            fallback_model: request.model.clone().unwrap_or_default(),
            buffer: String::new(),
            raw_lines: Vec::new(),
            stream_usage: TokenUsage::default(),
            result_usage: None,
            model: String::new(),
            got_events: false,
            fast_path_done: false,
            usage_done: false,
        }
    }

    /// 结果 blob → 事件（正文 + 会话 + 模型）；顺带把 blob 里的用量回给调用方。
    ///
    /// 模型名先落进 `self.model`，用量由调用方决定"即时上报还是收尾上报"。
    fn apply_result(&mut self, result: &OpenclawResult) -> (Vec<RuntimeEvent>, Option<TokenUsage>) {
        let mut events = Vec::new();
        if let Some(payloads) = &result.payloads {
            for payload in payloads {
                if !payload.text.is_empty() {
                    events.extend(self.state.text(&payload.text));
                }
            }
        }
        let mut usage = None;
        if let Some(agent) = &result.meta.agent_meta {
            if let Some(session) = agent.get("sessionId").and_then(Value::as_str) {
                self.state.set_session_opt(Some(session));
            }
            if let Some(model) = agent.get("model").and_then(Value::as_str) {
                let model = model.trim();
                if !model.is_empty() {
                    model.clone_into(&mut self.model);
                }
            }
            if let Some(object) = agent.get("usage").and_then(Value::as_object) {
                usage = Some(parse_usage(object));
            }
        }
        (events, usage)
    }

    /// 逐行路径上的 NDJSON 事件。
    fn handle_event(&mut self, event: &OpenclawEvent) -> Vec<RuntimeEvent> {
        let mut events = Vec::new();
        self.state.set_session_opt(event.session_id.as_deref());
        match event.kind.as_str() {
            "text" => {
                if let Some(text) = non_empty(event.text.as_deref()) {
                    events.extend(self.state.text(text));
                }
            }
            "tool_use" => {
                // 上游：`input` 不是对象（或解不出对象）时留空 map。
                let input = match event.input.as_ref() {
                    Some(Value::Object(object)) => Value::Object(object.clone()),
                    _ => serde_json::json!({}),
                };
                events.extend(self.state.tool_use(
                    event.call_id.clone().unwrap_or_default(),
                    event.tool.clone().unwrap_or_default(),
                    input,
                ));
            }
            "tool_result" => events.extend(self.state.tool_result(
                event.call_id.clone().unwrap_or_default(),
                event.text.clone().unwrap_or_default(),
                false,
            )),
            "error" => events.extend(self.state.note_error(event.error_message())),
            "lifecycle" => {
                if matches!(
                    event.phase.as_deref(),
                    Some("error" | "failed" | "cancelled")
                ) {
                    events.extend(self.state.note_error(event.error_message()));
                }
            }
            "step_start" => events.extend(self.state.progress("running")),
            "step_finish" => {
                if let Some(object) = event.usage.as_ref().and_then(Value::as_object) {
                    self.stream_usage += parse_usage(object);
                }
            }
            // 未知事件类型：上游只把它算作"确实见到了结构化输出"，不产生任何消息。
            _ => {}
        }
        events
    }

    /// 上报用量的模型名：blob 报的 → `LaunchRequest.model` → `"unknown"`。
    fn usage_model(&self) -> String {
        if !self.model.is_empty() {
            self.model.clone()
        } else if !self.fallback_model.is_empty() {
            self.fallback_model.clone()
        } else {
            "unknown".to_owned()
        }
    }
}

impl EventDecoder for OpenclawDecoder {
    fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        if self.fast_path_done {
            return Vec::new();
        }
        self.buffer.push_str(line);
        self.buffer.push('\n');

        // 快路径：整段（必要时剥掉前导日志行）能解析成结果 blob ⇒ 交付并收工。
        // 上游对同一个谓词是 100Hz 轮询，这里改成"每行采一次样"。
        if !self.fast_path_done {
            if let Some(result) = parse_whole_buffer_result(&self.buffer) {
                let (mut events, usage) = self.apply_result(&result);
                if let Some(usage) = usage.filter(|usage| is_non_zero(*usage)) {
                    events.extend(self.state.add_usage(&self.usage_model(), usage));
                }
                self.usage_done = true;
                self.got_events = true;
                self.fast_path_done = true;
                return events;
            }
        }

        if !line.starts_with('{') {
            self.raw_lines.push(line.to_owned());
            return Vec::new();
        }
        if let Some(event) = parse_event(line) {
            self.got_events = true;
            return self.handle_event(&event);
        }
        if let Some(result) = parse_result(line) {
            self.got_events = true;
            let (events, usage) = self.apply_result(&result);
            // 上游："有流式事件报过用量就不再用结果里的" —— 但代码实际是非零覆盖。
            if let Some(usage) = usage.filter(|usage| is_non_zero(*usage)) {
                self.result_usage = Some(usage);
            }
            return events;
        }
        self.raw_lines.push(line.to_owned());
        Vec::new()
    }

    fn finish(&mut self) -> Vec<RuntimeEvent> {
        let mut events = Vec::new();

        if !self.usage_done {
            let usage = self.result_usage.or_else(|| {
                if is_non_zero(self.stream_usage) {
                    Some(self.stream_usage)
                } else {
                    None
                }
            });
            if let Some(usage) = usage {
                events.extend(self.state.add_usage(&self.usage_model(), usage));
            }
        }

        if !self.got_events {
            let raw = self.raw_lines.join("\n");
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                // 差异 1：上游只把它写进 `Result.Output`，这里补一条 `Text` 让
                // "正文 = Text 事件拼接"这个 crate 口径与它重合。
                events.extend(self.state.text(trimmed));
            } else if self.state.terminal_error().is_none() {
                events.extend(self.state.note_error(NO_PARSEABLE_OUTPUT));
            }
        }
        events
    }
}

impl CliDecoder for OpenclawDecoder {
    fn summary(&self) -> CliSummary {
        self.state.summary()
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn is_non_zero(usage: TokenUsage) -> bool {
    usage.total_tokens > 0
}

/// 整段（先原样、再剥前导日志行）解析结果 blob —— 上游 `parseWholeBufferOpenclawResult`。
fn parse_whole_buffer_result(buffer: &str) -> Option<OpenclawResult> {
    let trimmed = buffer.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(result) = parse_result(trimmed) {
        return Some(result);
    }
    let lines: Vec<&str> = trimmed.split('\n').collect();
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with('{') {
            return parse_result(lines[index..].join("\n").trim());
        }
    }
    None
}

/// NDJSON 事件行 —— 上游 `tryParseOpenclawEvent`（要求 `type` 非空）。
fn parse_event(line: &str) -> Option<OpenclawEvent> {
    if !line.starts_with('{') {
        return None;
    }
    let event: OpenclawEvent = serde_json::from_str(line).ok()?;
    if event.kind.is_empty() {
        return None;
    }
    Some(event)
}

/// 最终结果 blob 行 —— 上游 `tryParseOpenclawResult`（`payloads` 或 `durationMs` 至少有一个）。
fn parse_result(raw: &str) -> Option<OpenclawResult> {
    if !raw.starts_with('{') {
        return None;
    }
    let result: OpenclawResult = serde_json::from_str(raw).ok()?;
    if result.payloads.is_none() && result.meta.duration_ms == 0 {
        return None;
    }
    Some(result)
}

/// 上游 `parseOpenclawUsage`（多套字段名，取第一个非零值）。
fn parse_usage(data: &Map<String, Value>) -> TokenUsage {
    tokens(
        first_u64(data, &["input", "inputTokens", "input_tokens"]),
        first_u64(data, &["output", "outputTokens", "output_tokens"]),
        first_u64(
            data,
            &[
                "cacheRead",
                "cachedInputTokens",
                "cached_input_tokens",
                "cache_read",
                "cache_read_input_tokens",
            ],
        ),
        first_u64(
            data,
            &[
                "cacheWrite",
                "cacheCreationInputTokens",
                "cache_creation_input_tokens",
                "cache_write",
            ],
        ),
    )
}

fn first_u64(data: &Map<String, Value>, keys: &[&str]) -> u64 {
    for key in keys {
        let value = json_u64(data.get(*key));
        if value != 0 {
            return value;
        }
    }
    0
}

/// `openclaw agent --json` 的最终结果 blob（`payloads` + `meta`）。
#[derive(Debug, serde::Deserialize)]
struct OpenclawResult {
    #[serde(default)]
    payloads: Option<Vec<OpenclawPayload>>,
    #[serde(default)]
    meta: OpenclawMeta,
}

#[derive(Debug, serde::Deserialize)]
struct OpenclawPayload {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenclawMeta {
    #[serde(default)]
    duration_ms: i64,
    #[serde(default)]
    agent_meta: Option<Map<String, Value>>,
}

/// 一条 NDJSON 事件。
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenclawEvent {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    usage: Option<Value>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    error: Option<OpenclawError>,
    #[serde(default)]
    message: Option<String>,
}

impl OpenclawEvent {
    /// 上游 `errorMessage()`：结构化错误 → `text` → `message` → 兜底串。
    fn error_message(&self) -> String {
        if let Some(message) = self.error.as_ref().and_then(OpenclawError::message) {
            return message;
        }
        if let Some(text) = non_empty(self.text.as_deref()) {
            return text.to_owned();
        }
        if let Some(message) = non_empty(self.message.as_deref()) {
            return message.to_owned();
        }
        UNKNOWN_ERROR.to_owned()
    }
}

#[derive(Debug, serde::Deserialize)]
struct OpenclawError {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    data: Option<OpenclawErrorData>,
    #[serde(default)]
    message: Option<String>,
}

impl OpenclawError {
    /// `data.message` → `message` → `name`。
    fn message(&self) -> Option<String> {
        if let Some(message) = self
            .data
            .as_ref()
            .and_then(|data| non_empty(data.message.as_deref()))
        {
            return Some(message.to_owned());
        }
        if let Some(message) = non_empty(self.message.as_deref()) {
            return Some(message.to_owned());
        }
        non_empty(self.name.as_deref()).map(str::to_owned)
    }
}

#[derive(Debug, serde::Deserialize)]
struct OpenclawErrorData {
    #[serde(default)]
    message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder() -> OpenclawDecoder {
        OpenclawDecoder::new(&LaunchRequest::new("干点活").with_model("my-agent"))
    }

    fn pump(decoder: &mut OpenclawDecoder, stream: &str) -> Vec<RuntimeEvent> {
        let mut events: Vec<RuntimeEvent> = stream
            .lines()
            .flat_map(|line| decoder.push_line(line))
            .collect();
        events.extend(decoder.finish());
        events
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
    fn a_pretty_printed_result_blob_after_a_log_banner_is_the_fast_path() {
        let mut decoder = decoder();
        let stream = concat!(
            "openclaw: 启动横幅\n",
            "{\n",
            "  \"payloads\": [\n",
            "    { \"text\": \"前半段\" },\n",
            "    { \"text\": \"后半段\" }\n",
            "  ],\n",
            "  \"meta\": {\n",
            "    \"durationMs\": 812,\n",
            "    \"agentMeta\": {\n",
            "      \"sessionId\": \"oc-ses-1\",\n",
            "      \"model\": \" deepseek-chat \",\n",
            "      \"usage\": { \"input_tokens\": 10, \"output_tokens\": 5 }\n",
            "    }\n",
            "  }\n",
            "}\n",
        );
        let events = pump(&mut decoder, stream);

        assert_eq!(text_of(&events), "前半段后半段");
        // 快路径整段只交付一次：正文不会被后续（不存在的）行重复追加。
        let summary = decoder.summary();
        assert_eq!(summary.output, "前半段后半段");
        assert_eq!(summary.session_id.as_deref(), Some("oc-ses-1"));
        assert_eq!(summary.terminal_error, None);
        assert_eq!(summary.usage.len(), 1);
        assert_eq!(summary.usage[0].model, "deepseek-chat");
        assert_eq!(summary.usage[0].usage.total_tokens, 15);
        assert_eq!(summary.usage[0].usage.input, 10);
        assert_eq!(summary.usage[0].usage.output, 5);
    }

    #[test]
    fn the_fast_path_fires_once_even_if_more_lines_follow() {
        let mut decoder = decoder();
        decoder.push_line(r#"{"payloads":[{"text":"ok"}],"meta":{"durationMs":1}}"#);
        let late = decoder.push_line(r#"{"type":"text","text":"不该出现"}"#);
        assert!(late.is_empty(), "快路径之后的行必须被忽略：{late:?}");
        assert_eq!(decoder.summary().output, "ok");
    }

    #[test]
    fn ndjson_events_are_handled_line_by_line() {
        let mut decoder = decoder();
        let stream = concat!(
            r#"{"type":"step_start"}"#,
            "\n",
            r#"{"type":"tool_use","tool":"read","callId":"c1","input":{"path":"a"}}"#,
            "\n",
            r##"{"type":"tool_result","tool":"read","callId":"c1","text":"# a"}"##,
            "\n",
            r#"{"type":"step_finish","usage":{"input":10,"output":5,"cacheRead":2}}"#,
            "\n",
            r#"{"type":"text","text":"ok","sessionId":"oc-ses-2"}"#,
            "\n",
        );
        let events = pump(&mut decoder, stream);

        assert!(events.iter().any(
            |event| matches!(event, RuntimeEvent::Progress { status } if status == "running")
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::ToolUse { call_id, tool, input }
                if call_id == "c1" && tool == "read" && input["path"] == "a"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::ToolResult { call_id, output, is_error }
                if call_id == "c1" && output == "# a" && !*is_error
        )));

        let summary = decoder.summary();
        assert_eq!(summary.output, "ok");
        assert_eq!(summary.session_id.as_deref(), Some("oc-ses-2"));
        assert_eq!(summary.usage.len(), 1);
        // `step_finish` 的用量在收尾时按"结果 blob 没报 model ⇒ LaunchRequest.model"落表。
        assert_eq!(summary.usage[0].model, "my-agent");
        assert_eq!(summary.usage[0].usage.total_tokens, 17);
    }

    #[test]
    fn a_result_line_overrides_the_streamed_usage() {
        let mut decoder = decoder();
        let stream = concat!(
            r#"{"type":"step_finish","usage":{"input":10,"output":5}}"#,
            "\n",
            r#"{"payloads":[{"text":"ok"}],"meta":{"durationMs":1,"agentMeta":{"model":"m","usage":{"input":1,"output":1}}}}"#,
            "\n",
        );
        pump(&mut decoder, stream);
        let summary = decoder.summary();
        assert_eq!(summary.output, "ok");
        assert_eq!(summary.usage.len(), 1);
        assert_eq!(summary.usage[0].model, "m");
        assert_eq!(
            summary.usage[0].usage.total_tokens, 2,
            "结果里的用量覆盖流式累加"
        );
    }

    #[test]
    fn error_and_lifecycle_failures_are_terminal() {
        let mut error_event = decoder();
        pump(
            &mut error_event,
            r#"{"type":"error","error":{"data":{"message":"炸了"}}}"#,
        );
        assert_eq!(
            error_event.summary().terminal_error.as_deref(),
            Some("炸了")
        );

        let mut lifecycle = decoder();
        pump(
            &mut lifecycle,
            r#"{"type":"lifecycle","phase":"cancelled","text":"被撤了"}"#,
        );
        assert_eq!(
            lifecycle.summary().terminal_error.as_deref(),
            Some("被撤了")
        );

        // 非失败 phase 的 lifecycle 不产生错误。
        let mut running = decoder();
        let events = pump(&mut running, r#"{"type":"lifecycle","phase":"thinking"}"#);
        assert!(events.is_empty());
        assert_eq!(running.summary().terminal_error, None);

        // 首错胜出。
        let mut twice = decoder();
        pump(
            &mut twice,
            concat!(
                r#"{"type":"error","text":"第一处"}"#,
                "\n",
                r#"{"type":"error","text":"第二处"}"#,
                "\n",
            ),
        );
        assert_eq!(twice.summary().terminal_error.as_deref(), Some("第一处"));
    }

    #[test]
    fn the_error_message_falls_back_through_text_message_and_a_constant() {
        let event: OpenclawEvent = serde_json::from_str(r#"{"type":"error"}"#).expect("事件");
        assert_eq!(event.error_message(), UNKNOWN_ERROR);

        let event: OpenclawEvent =
            serde_json::from_str(r#"{"type":"error","error":{"name":"E_NAME"}}"#).expect("事件");
        assert_eq!(event.error_message(), "E_NAME");

        let event: OpenclawEvent =
            serde_json::from_str(r#"{"type":"error","message":"M"}"#).expect("事件");
        assert_eq!(event.error_message(), "M");
    }

    #[test]
    fn a_non_object_tool_input_becomes_an_empty_object() {
        let mut decoder = decoder();
        let events = pump(
            &mut decoder,
            r#"{"type":"tool_use","tool":"t","callId":"c1","input":[1,2]}"#,
        );
        let input = match &events[0] {
            RuntimeEvent::ToolUse { input, .. } => input.clone(),
            other => panic!("意外事件：{other:?}"),
        };
        assert_eq!(input, serde_json::json!({}));
    }

    #[test]
    fn unknown_event_types_count_as_structured_output_but_produce_nothing() {
        let mut decoder = decoder();
        let events = pump(
            &mut decoder,
            concat!(
                r#"{"type":"future.chunk","payload":{"x":1}}"#,
                "\n",
                r#"{"type":"step_start"}"#,
                "\n",
            ),
        );
        // 没有正文也没有错误：既不是"纯日志兜底"，也不是"无输出"。
        assert_eq!(text_of(&events), "");
        assert_eq!(decoder.summary().output, "");
        assert_eq!(decoder.summary().terminal_error, None);
    }

    #[test]
    fn pure_log_output_falls_back_to_the_raw_text() {
        let mut decoder = decoder();
        let events = pump(&mut decoder, "第一行日志\n\n  第二行日志  \n最后一行\n");
        assert_eq!(text_of(&events), "第一行日志\n第二行日志\n最后一行");
        assert_eq!(decoder.summary().output, "第一行日志\n第二行日志\n最后一行");
        assert_eq!(decoder.summary().terminal_error, None);
    }

    #[test]
    fn an_empty_stream_is_the_canonical_no_parseable_output_failure() {
        let mut decoder = decoder();
        let events = decoder.finish();
        assert_eq!(events.len(), 1);
        assert_eq!(
            decoder.summary().terminal_error.as_deref(),
            Some(NO_PARSEABLE_OUTPUT)
        );
        // 上游逐字串：外部告警依赖它。
        assert_eq!(NO_PARSEABLE_OUTPUT, "openclaw returned no parseable output");
    }

    #[test]
    fn an_unknown_json_object_is_not_a_result_and_falls_back_to_raw_text() {
        // `payloads` 缺席 + `durationMs == 0` ⇒ 不是结果 blob ⇒ 逐行路径把它当日志行，
        // 于是落进"纯日志兜底"（上游同款：`gotEvents == false` 时用原文当输出）。
        let raw = r#"{"meta":{"agentMeta":{"sessionId":"s"}}}"#;
        let mut raw_only = decoder();
        let events = pump(&mut raw_only, raw);
        assert_eq!(text_of(&events), raw);
        assert_eq!(raw_only.summary().terminal_error, None);
        assert_eq!(
            raw_only.summary().session_id,
            None,
            "兜底路径不认 agentMeta"
        );

        // `payloads: []` 是"有 payloads"（Go 里非 nil 空切片同款）。
        let mut empty_payloads = decoder();
        pump(
            &mut empty_payloads,
            r#"{"payloads":[],"meta":{"durationMs":0}}"#,
        );
        assert_eq!(empty_payloads.summary().terminal_error, None);
        assert_eq!(empty_payloads.summary().session_id, None);
        assert_eq!(empty_payloads.summary().output, "");
    }

    #[test]
    fn usage_field_names_from_all_known_versions_are_accepted() {
        let mut decoder = decoder();
        pump(
            &mut decoder,
            concat!(
                r#"{"payloads":[{"text":"ok"}],"meta":{"durationMs":1,"agentMeta":{"usage":{"#,
                r#""inputTokens":100,"outputTokens":50,"cachedInputTokens":7,"cache_creation_input_tokens":3}}}}"#,
                "\n",
            ),
        );
        let summary = decoder.summary();
        assert_eq!(summary.usage[0].usage.input, 100);
        assert_eq!(summary.usage[0].usage.output, 50);
        assert_eq!(summary.usage[0].usage.cache_read, 7);
        assert_eq!(summary.usage[0].usage.cache_write, 3);
        assert_eq!(summary.usage[0].usage.total_tokens, 160);
    }

    #[test]
    fn the_usage_model_falls_back_to_the_request_then_to_unknown() {
        let mut with_request_model = OpenclawDecoder::new(&LaunchRequest::new("p").with_model("a"));
        pump(
            &mut with_request_model,
            r#"{"type":"step_finish","usage":{"input":1}}"#,
        );
        assert_eq!(with_request_model.summary().usage[0].model, "a");

        let mut without = OpenclawDecoder::new(&LaunchRequest::new("p"));
        pump(
            &mut without,
            r#"{"type":"step_finish","usage":{"input":1}}"#,
        );
        assert_eq!(without.summary().usage[0].model, "unknown");
    }
}
