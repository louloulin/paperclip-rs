//! `dsh` 的**版本化 JSONL** 解码器（上游 `server/pkg/agent/dsh.go`）。
//!
//! # 协议
//!
//! DSH 不走 ACP：它自己拥有 agent 循环、会话存储、模型目录、工具与 MCP 客户端，
//! Multica 这边只负责把它的帧翻成统一事件。stdin 与 stdout 都是**逐行 JSON**：
//!
//! ```text
//! → {"v":1,"type":"execute","request_id":"…","cwd":…,"prompt":…,"mcp_servers":[]}
//! ← {"v":1,"type":"ready","runtime":"dsh"}
//! ← {"v":1,"type":"session","session_id":"…"}
//! ← {"v":1,"type":"text","content":"…"} / "thinking" / "tool_call" / "tool_result"
//! ← {"v":1,"type":"usage","provider":"…","model":"…","input_tokens":…}
//! ← {"v":1,"type":"result","status":"completed","session_id":"…"}
//! ```
//!
//! `execute` 帧在 spawn 之后立刻写下去（[`CliDecoder::initial_frames`]），prompt 就在
//! 这一帧里 —— 因此 dsh 的 stdin 是**长连接**（要留着写 `cancel`），不是写完即关的
//! `StdinText`。
//!
//! # 帧的接受规则（逐条对齐上游 `handleDshFrame`）
//!
//! 1. `v != 1` ⇒ 协议错误（终态失败）。上游把"任何解析进 `dshFrame` 的对象"都算数，
//!    因此缺 `v` 的 JSON 行视为 `0`，同样判错（见模块文档差异 1 的反面：非对象行
//!    才是静默跳过的那一类）。
//! 2. `request_id` 非空且不等于自己那一帧的 id ⇒ **整帧忽略**（多路复用/迟到帧）。
//! 3. 未知 `type` ⇒ 忽略（前向兼容）。
//!
//! # 与上游的差异
//!
//! 1. **终态正文只认流里的 `text` 增量**：上游 `result.output` 是运行时自己汇总的一份
//!    文本快照，dsh 会在 `result` 帧里回传。本 crate 的 `RunOutcome.output` 统一取
//!    "Text 事件拼接"（`docs/33` §5 的约定），因此 `result.output` 不参与终态。
//! 2. **非 `completed` 的结果一律折成 `Failed`**：上游把 dsh 自报的
//!    `status`（`failed` / `cancelled` / `timeout`）原样透传给调用方；本 crate 的
//!    `RunStatus::Cancelled` / `Timeout` 由 runner 按**取消信号 / 超时**归因，
//!    解码器只能表达"失败"，所以这里把 dsh 自报的非 `completed` 终态判成 agent 级
//!    失败（fail-closed）。
//! 3. **`resume_rejected` 不落地**：`LaunchRequest` 没有这个字段（属 M4 的会话续跑
//!    口径），这里只忽略。
//! 4. **`model` 的解析更宽松**：上游对 `provider/model` 之外的写法直接让 `Execute`
//!    失败；本 crate 的 `build_args` / `decoder_for` 都不能返回错误，因此没有 `/`
//!    或半边为空的写法退化成 `{"id": <原样>}`（省掉 `provider`），而不是拒绝启动。
//! 5. **`%XX` 解码是 lossy 的**：上游 `url.PathUnescape` 遇到非法转义会报错，
//!    这里按字面保留（非法 UTF-8 退化成 U+FFFD）。
//! 6. **`mcp_servers` 恒为 `[]`**：`LaunchRequest` 里没有 MCP 配置字段（与 ACP 同款
//!    缺口，见 `docs/33` §11），上游会从 `ExecOptions.McpConfig` 拼出来。
//!
//! [`CliDecoder::initial_frames`]: super::super::cli_core::CliDecoder::initial_frames

use serde_json::{Map, Value};

use super::super::cli_core::decoder::{
    field_str, field_u64, tokens, CliDecoder, CliSummary, DecoderState,
};
use crate::adapter::{EventDecoder, LaunchRequest, RuntimeEvent};

/// DSH stdio 协议版本（上游 `DshProtocolVersion`）。
pub(super) const PROTOCOL_VERSION: i64 = 1;

/// 请求 id：上游用 task id，缺省 `multica-<纳秒>`；本 crate 用 run id（同一 run 内稳定）。
pub(super) fn request_id(request: &LaunchRequest) -> String {
    format!("multica-{}", request.run_id)
}

/// `execute` 帧（一行 JSON，**含尾换行** —— 对端按行解析）。
pub(super) fn execute_frame(request: &LaunchRequest) -> String {
    let mut frame = Map::new();
    frame.insert("v".to_owned(), Value::from(PROTOCOL_VERSION));
    frame.insert("type".to_owned(), Value::from("execute"));
    frame.insert("request_id".to_owned(), Value::from(request_id(request)));
    frame.insert("cwd".to_owned(), Value::from(cwd_of(request)));
    frame.insert("prompt".to_owned(), Value::from(request.prompt.clone()));
    if let Some(resume) = &request.resume_session {
        frame.insert("resume_session_id".to_owned(), Value::from(resume.clone()));
    }
    if let Some(selection) = request.model.as_deref().and_then(model_selection) {
        frame.insert("model".to_owned(), selection);
    }
    if let Some(level) = &request.thinking_level {
        frame.insert("reasoning_effort".to_owned(), Value::from(level.clone()));
    }
    // 上游字段总是出现（`dshExecuteCommand` 没有 omitempty），空数组即"没有 MCP"。
    frame.insert("mcp_servers".to_owned(), Value::Array(Vec::new()));
    let mut line = Value::Object(frame).to_string();
    line.push('\n');
    line
}

/// `cancel` 帧（取消时补发；同款一行 JSON + 尾换行）。
pub(super) fn cancel_frame(request: &LaunchRequest) -> String {
    let mut frame = Map::new();
    frame.insert("v".to_owned(), Value::from(PROTOCOL_VERSION));
    frame.insert("type".to_owned(), Value::from("cancel"));
    frame.insert("request_id".to_owned(), Value::from(request_id(request)));
    let mut line = Value::Object(frame).to_string();
    line.push('\n');
    line
}

/// 工作目录：`None` ⇒ 空串（上游 `opts.Cwd` 恒原样下发，空串即"用运行时自己的 cwd"）。
fn cwd_of(request: &LaunchRequest) -> String {
    request
        .cwd
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `provider/model` ⇒ `{"provider": …, "id": …}`（上游 `parseDshModelID`）。
///
/// 宽松口径见模块文档差异 4：拆不出两半时退化成只有 `id` 的选择，而不是拒绝启动。
fn model_selection(raw: &str) -> Option<Value> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (provider, model) = match raw.split_once('/') {
        Some((provider, model)) => (
            percent_decode(provider).trim().to_owned(),
            percent_decode(model).trim().to_owned(),
        ),
        None => (String::new(), raw.to_owned()),
    };
    if provider.is_empty() || model.is_empty() {
        return Some(serde_json::json!({ "id": raw }));
    }
    Some(serde_json::json!({ "provider": provider, "id": model }))
}

/// 只解 `%XX`（**不**把 `+` 当空格：与 `url.PathUnescape` 一致），非法转义按字面保留。
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// dsh 的逐行 JSON 解码器。
///
/// 四个布尔位各自记录协议推进的一件事实（`ready` / `seen_result` / `handshaken` /
/// `cancelled`），拆成枚举反而要把"收到 ready 帧时已经握过手"这类组合状态重新编码，
/// 所以这里保留平铺写法。
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct DshDecoder {
    state: DecoderState,
    /// 自己那一帧的 `request_id`（非空且不同的帧整帧丢弃）。
    request_id: String,
    execute: String,
    cancel: String,
    /// 已经收到 `ready` 且 `runtime == "dsh"`。
    ready: bool,
    /// 已经收到 `result` 帧（终态由它自己给出，不再补"没 ready"的兜底错误）。
    seen_result: bool,
    handshaken: bool,
    cancelled: bool,
}

impl DshDecoder {
    /// 按请求拼好 `execute` / `cancel` 两帧（帧内容与解码器一一对应）。
    pub(crate) fn new(request: &LaunchRequest) -> Self {
        Self {
            state: DecoderState::default(),
            request_id: request_id(request),
            execute: execute_frame(request),
            cancel: cancel_frame(request),
            ready: false,
            seen_result: false,
            handshaken: false,
            cancelled: false,
        }
    }
}

impl EventDecoder for DshDecoder {
    fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        // 非对象（数组/标量）或非法 JSON：上游算 invalidFrames，静默跳过。
        let Ok(Value::Object(frame)) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let version = frame.get("v").and_then(Value::as_i64).unwrap_or_default();
        if version != PROTOCOL_VERSION {
            return self.state.note_error(format!(
                "dsh returned unsupported protocol version {version}"
            ));
        }
        let frame_request = frame
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if !frame_request.is_empty() && frame_request != self.request_id {
            return Vec::new();
        }
        self.handle_frame(&Value::Object(frame))
    }

    fn finish(&mut self) -> Vec<RuntimeEvent> {
        // 上游：没有 `result` 帧时，先看协议错误，再看"还没 ready 就退出了"。
        if !self.ready && !self.seen_result && self.state.terminal_error().is_none() {
            return self
                .state
                .note_error("dsh exited before the runtime protocol became ready");
        }
        Vec::new()
    }
}

impl DshDecoder {
    /// 分发一帧（已过版本闸门与 `request_id` 过滤）。
    fn handle_frame(&mut self, frame: &Value) -> Vec<RuntimeEvent> {
        match field_str(frame, "type").unwrap_or_default().as_str() {
            "ready" => {
                self.ready = field_str(frame, "runtime").as_deref() == Some("dsh");
                Vec::new()
            }
            "session" => {
                self.state
                    .set_session_opt(field_str(frame, "session_id").as_deref());
                self.state.progress("running")
            }
            "text" => {
                let content = field_str(frame, "content").unwrap_or_default();
                self.state.text(&content)
            }
            "thinking" => {
                let content = field_str(frame, "content").unwrap_or_default();
                self.state.thinking(&content)
            }
            "tool_call" => {
                let arguments = field_str(frame, "arguments").unwrap_or_default();
                let input = match serde_json::from_str::<Value>(&arguments) {
                    Ok(input @ Value::Object(_)) => input,
                    // 上游：空串 ⇒ `{}`；非空但解不出对象 ⇒ `{"raw": <原样>}`。
                    _ if arguments.is_empty() => serde_json::json!({}),
                    _ => serde_json::json!({ "raw": arguments }),
                };
                self.state.tool_use(
                    field_str(frame, "call_id").unwrap_or_default(),
                    field_str(frame, "name").unwrap_or_default(),
                    input,
                )
            }
            "tool_result" => self.state.tool_result(
                field_str(frame, "call_id").unwrap_or_default(),
                field_str(frame, "output").unwrap_or_default(),
                frame
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or_default(),
            ),
            "usage" => {
                let model = field_str(frame, "model").unwrap_or_default();
                let provider = field_str(frame, "provider").unwrap_or_default();
                let key = if provider.is_empty() {
                    model
                } else {
                    format!("{provider}/{model}")
                };
                if key.is_empty() {
                    return Vec::new();
                }
                let usage = tokens(
                    field_u64(frame, "input_tokens"),
                    field_u64(frame, "output_tokens"),
                    field_u64(frame, "cache_read_tokens"),
                    field_u64(frame, "cache_write_tokens"),
                );
                self.state.add_usage(&key, usage)
            }
            "protocol_error" => {
                let code = field_str(frame, "code").unwrap_or_default();
                let message = field_str(frame, "message").unwrap_or_default();
                self.state
                    .note_error(format!("{code}: {message}").trim().to_owned())
            }
            "result" => {
                self.seen_result = true;
                self.state
                    .set_session_opt(field_str(frame, "session_id").as_deref());
                let error_text = wire_error_text(frame);
                if !error_text.is_empty() {
                    return self.state.note_error(error_text);
                }
                match field_str(frame, "status").unwrap_or_default().as_str() {
                    "" | "completed" => Vec::new(),
                    other => self
                        .state
                        .note_error(format!("dsh run ended with status {other}")),
                }
            }
            _ => Vec::new(),
        }
    }
}

impl CliDecoder for DshDecoder {
    fn summary(&self) -> CliSummary {
        self.state.summary()
    }

    fn initial_frames(&mut self) -> Vec<String> {
        if self.handshaken {
            return Vec::new();
        }
        self.handshaken = true;
        vec![self.execute.clone()]
    }

    fn cancel_frames(&mut self) -> Vec<String> {
        if self.cancelled {
            return Vec::new();
        }
        self.cancelled = true;
        vec![self.cancel.clone()]
    }
}

/// `{"code": …, "message": …}` ⇒ `"code: message"`（上游 `TrimSpace` 后同款）。
fn wire_error_text(frame: &Value) -> String {
    let Some(error) = frame.get("error").and_then(Value::as_object) else {
        return String::new();
    };
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!("{code}: {message}").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder() -> DshDecoder {
        DshDecoder::new(&LaunchRequest::new("干点活"))
    }

    fn texts(events: Vec<RuntimeEvent>) -> String {
        events
            .into_iter()
            .filter_map(|event| match event {
                RuntimeEvent::Text { delta } => Some(delta),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_execute_frame_carries_the_prompt_under_our_own_request_id() {
        let request = LaunchRequest::new("干点活")
            .with_cwd("/tmp/w")
            .with_model("anthropic%2Fteam/claude")
            .with_thinking_level("high")
            .with_resume_session("ses-1");
        let frame: Value = serde_json::from_str(&execute_frame(&request)).expect("合法 JSON");

        assert_eq!(frame["v"], 1);
        assert_eq!(frame["type"], "execute");
        assert_eq!(frame["request_id"], request_id(&request));
        assert_eq!(frame["cwd"], "/tmp/w");
        assert_eq!(frame["prompt"], "干点活");
        assert_eq!(frame["resume_session_id"], "ses-1");
        // `%2F` 先解码再当分隔符：`anthropic%2Fteam` 是 provider，`claude` 是模型。
        assert_eq!(frame["model"]["provider"], "anthropic/team");
        assert_eq!(frame["model"]["id"], "claude");
        assert_eq!(frame["reasoning_effort"], "high");
        assert_eq!(frame["mcp_servers"], serde_json::json!([]));
        assert!(execute_frame(&request).ends_with('\n'), "帧必须以换行结束");
    }

    #[test]
    fn a_model_without_a_slash_degrades_to_an_id_only_selection() {
        let request = LaunchRequest::new("p").with_model("claude");
        let frame: Value = serde_json::from_str(&execute_frame(&request)).expect("合法 JSON");
        assert_eq!(frame["model"], serde_json::json!({ "id": "claude" }));
        assert!(frame["model"].get("provider").is_none());
        // 空串与纯空白 ⇒ 干脆不下发 model 字段。
        let blank: Value =
            serde_json::from_str(&execute_frame(&LaunchRequest::new("p").with_model("   ")))
                .expect("合法 JSON");
        assert!(blank.get("model").is_none());
    }

    #[test]
    fn frames_from_another_request_are_ignored_entirely() {
        let mut decoder = decoder();
        let mine = decoder.request_id.clone();
        let line =
            format!(r#"{{"v":1,"type":"text","content":"别人的","request_id":"{mine}-other"}}"#);
        assert!(decoder.push_line(&line).is_empty());
        // 空 request_id 的帧照收（上游同款）。
        let events = decoder.push_line(r#"{"v":1,"type":"text","content":"我的"}"#);
        assert_eq!(texts(events), "我的");
        assert_eq!(decoder.summary().output, "我的");
    }

    #[test]
    fn a_wrong_protocol_version_fails_the_run() {
        let mut decoder = decoder();
        let events = decoder.push_line(r#"{"type":"text","content":"ok"}"#);
        assert_eq!(events.len(), 1);
        assert_eq!(
            decoder.summary().terminal_error.as_deref(),
            Some("dsh returned unsupported protocol version 0")
        );
        // 首错胜出：后续帧不再改错误串。
        decoder.push_line(r#"{"v":9,"type":"text","content":"ok"}"#);
        assert_eq!(
            decoder.summary().terminal_error.as_deref(),
            Some("dsh returned unsupported protocol version 0")
        );
    }

    #[test]
    fn the_full_success_stream_maps_onto_unified_events() {
        let mut decoder = decoder();
        let stream = concat!(
            r#"{"v":1,"type":"ready","runtime":"dsh"}"#,
            "\n",
            r#"{"v":1,"type":"session","session_id":"ses-1"}"#,
            "\n",
            r#"{"v":1,"type":"thinking","content":"嗯"}"#,
            "\n",
            r#"{"v":1,"type":"tool_call","call_id":"c1","name":"read","arguments":"{\"path\":\"a\"}"}"#,
            "\n",
            r##"{"v":1,"type":"tool_result","call_id":"c1","name":"read","output":"# a","is_error":true}"##,
            "\n",
            r#"{"v":1,"type":"usage","provider":"anthropic","model":"claude","input_tokens":10,"output_tokens":5}"#,
            "\n",
            r#"{"v":1,"type":"text","content":"ok"}"#,
            "\n",
            r#"{"v":1,"type":"result","status":"completed","session_id":"ses-1"}"#,
            "\n",
        );
        let mut events: Vec<RuntimeEvent> = stream
            .lines()
            .flat_map(|line| decoder.push_line(line))
            .collect();
        events.extend(decoder.finish());

        assert!(events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Thinking { delta } if delta == "嗯")));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::ToolUse { call_id, tool, input }
                if call_id == "c1" && tool == "read" && input["path"] == "a"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::ToolResult { call_id, output, is_error }
                if call_id == "c1" && output == "# a" && *is_error
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Usage { model, usage }
                if model == "anthropic/claude" && usage.total_tokens == 15
        )));
        assert!(events.iter().any(
            |event| matches!(event, RuntimeEvent::Progress { status } if status == "running")
        ));

        let summary = decoder.summary();
        assert_eq!(summary.output, "ok");
        assert_eq!(summary.session_id.as_deref(), Some("ses-1"));
        assert_eq!(summary.terminal_error, None);
        assert_eq!(summary.usage.len(), 1);
        assert_eq!(summary.usage[0].usage.total_tokens, 15);
    }

    #[test]
    fn tool_arguments_that_are_not_an_object_fall_back_to_raw() {
        let mut decoder = decoder();
        let events = decoder.push_line(
            r#"{"v":1,"type":"tool_call","call_id":"c1","name":"t","arguments":"[1,2]"}"#,
        );
        let input = match &events[0] {
            RuntimeEvent::ToolUse { input, .. } => input.clone(),
            other => panic!("意外事件：{other:?}"),
        };
        assert_eq!(input, serde_json::json!({ "raw": "[1,2]" }));
    }

    #[test]
    fn a_failed_result_and_a_protocol_error_both_become_terminal_errors() {
        let mut failed = decoder();
        failed.push_line(
            r#"{"v":1,"type":"result","status":"failed","error":{"code":"E_BOOM","message":"炸了"}}"#,
        );
        assert_eq!(
            failed.summary().terminal_error.as_deref(),
            Some("E_BOOM: 炸了")
        );

        // 有 result 帧、没有 error ⇒ 按 status 归因（非 completed 一律失败）。
        let mut cancelled = decoder();
        cancelled.push_line(r#"{"v":1,"type":"result","status":"cancelled"}"#);
        assert_eq!(
            cancelled.summary().terminal_error.as_deref(),
            Some("dsh run ended with status cancelled")
        );

        let mut protocol = decoder();
        protocol.push_line(r#"{"v":1,"type":"protocol_error","code":"E_PROTO","message":"坏了"}"#);
        assert_eq!(
            protocol.summary().terminal_error.as_deref(),
            Some("E_PROTO: 坏了")
        );
    }

    #[test]
    fn exiting_before_ready_is_a_terminal_error_unless_a_result_arrived() {
        let mut never_ready = decoder();
        never_ready.push_line(r#"{"v":1,"type":"text","content":"ok"}"#);
        assert_eq!(never_ready.finish().len(), 1);
        assert_eq!(
            never_ready.summary().terminal_error.as_deref(),
            Some("dsh exited before the runtime protocol became ready")
        );

        // 有 result 帧时不自作主张（终态由 result 说话）。
        let mut finished = decoder();
        finished.push_line(r#"{"v":1,"type":"result","status":"completed"}"#);
        assert!(finished.finish().is_empty());
        assert_eq!(finished.summary().terminal_error, None);

        // `ready` 帧的 `runtime` 不是 dsh ⇒ 不算 ready（上游同款）。
        let mut foreign = decoder();
        foreign.push_line(r#"{"v":1,"type":"ready","runtime":"other"}"#);
        assert_eq!(foreign.finish().len(), 1);
    }

    #[test]
    fn the_cancel_frame_is_sent_at_most_once() {
        let mut decoder = decoder();
        let frames = decoder.initial_frames();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].ends_with('\n'));
        assert!(frames[0].contains(r#""type":"execute""#));
        assert!(decoder.initial_frames().is_empty(), "execute 只写一次");

        let cancel = decoder.cancel_frames();
        assert_eq!(cancel.len(), 1);
        assert!(cancel[0].contains(r#""type":"cancel""#));
        assert!(decoder.cancel_frames().is_empty(), "cancel 只写一次");
    }

    #[test]
    fn percent_decoding_is_path_unescape_and_lossy() {
        assert_eq!(percent_decode("anthropic%2Fteam"), "anthropic/team");
        assert_eq!(percent_decode("a+b"), "a+b", "`+` 不是空格");
        assert_eq!(percent_decode("100%"), "100%", "非法转义按字面保留");
        assert_eq!(percent_decode("%E4%B8%AD"), "中");
    }
}
