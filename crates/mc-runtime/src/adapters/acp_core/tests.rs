//! ACP 状态机的一致性/行为测试（只走公开 API：构造 → 喂行 → 看事件与出帧）。
//!
//! 6 个 ACP provider 共用这一个解码器，所以这里用 `kimi` 的 flavor 当"默认样本"，
//! 再单独覆盖每个差异点（resume 方法、prompt 字段、认证步骤、推理等级）。

use super::{
    client::AcpDecoder, conformance_junk_stdout, conformance_success_stdout, AcpAuth, AcpFlavor,
    AcpPromptFields, AcpResume, AcpToolAliases,
};
use crate::adapter::{EventDecoder, LaunchRequest, RuntimeEvent};
use crate::adapters::cli_core::CliDecoder;
use crate::catalog::AgentType;

/// 默认样本：kimi（`session/resume` + 纯 `prompt` + 无认证 + 支持推理等级）。
static KIMI: AcpFlavor = AcpFlavor {
    kind: AgentType::Kimi,
    label: "kimi",
    resume: AcpResume::Resume,
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::Prompt,
    thinking_config: Some("thinking"),
    tool_aliases: AcpToolAliases::Kimi,
};

/// kiro：`session/load` + 两种 prompt 键。
static KIRO: AcpFlavor = AcpFlavor {
    kind: AgentType::Kiro,
    label: "kiro",
    resume: AcpResume::Load,
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::PromptAndContent,
    thinking_config: None,
    tool_aliases: AcpToolAliases::Kiro,
};

/// grok：`session/load` + 先认证。
static GROK: AcpFlavor = AcpFlavor {
    kind: AgentType::Grok,
    label: "grok",
    resume: AcpResume::Load,
    auth: AcpAuth::XaiApiKey,
    prompt_fields: AcpPromptFields::Prompt,
    thinking_config: None,
    tool_aliases: AcpToolAliases::Kimi,
};

fn success_lines() -> Vec<String> {
    conformance_success_stdout("s1", "ok", false)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn auth_lines() -> Vec<String> {
    conformance_success_stdout("s1", "ok", true)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// 喂行并收集**每一次** push_line 之后新出队的帧。
fn pump(decoder: &mut AcpDecoder, lines: &[String]) -> Vec<String> {
    let mut frames = Vec::new();
    for line in lines {
        decoder.push_line(line);
        frames.extend(decoder.take_outbox());
    }
    frames
}

/// 喂行并收集事件。
fn events(decoder: &mut AcpDecoder, lines: &[String]) -> Vec<RuntimeEvent> {
    let mut collected = Vec::new();
    for line in lines {
        collected.extend(decoder.push_line(line));
    }
    collected
}

fn text_events(collected: Vec<RuntimeEvent>) -> Vec<String> {
    collected
        .into_iter()
        .filter_map(|event| match event {
            RuntimeEvent::Text { delta } => Some(delta),
            _ => None,
        })
        .collect()
}

#[test]
fn handshake_is_initialize_then_session_then_prompt() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    let init = decoder.initial_frames();
    assert_eq!(init.len(), 1);
    assert!(init[0].contains("\"method\":\"initialize\""), "{}", init[0]);
    assert!(
        init[0].contains("\"clientCapabilities\":{}"),
        "本片不宣告任何客户端能力：{}",
        init[0]
    );

    let frames = pump(&mut decoder, &success_lines());
    assert_eq!(frames.len(), 2, "会话帧 + prompt 帧：{frames:?}");
    assert!(frames[0].contains("\"method\":\"session/new\""), "{}", frames[0]);
    assert!(frames[0].contains("\"cwd\":\".\""), "{}", frames[0]);
    assert!(frames[0].contains("\"mcpServers\":[]"), "{}", frames[0]);
    assert!(frames[1].contains("\"method\":\"session/prompt\""), "{}", frames[1]);
    assert!(frames[1].contains("\"sessionId\":\"s1\""), "{}", frames[1]);
    assert!(frames[1].contains("\"text\":\"hi\""), "{}", frames[1]);
}

#[test]
fn handshake_frames_are_only_sent_once() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    assert_eq!(decoder.initial_frames().len(), 1);
    assert!(decoder.initial_frames().is_empty(), "握手帧只发一次");
}

#[test]
fn session_response_supplies_the_session_id_and_the_prompt() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    let lines = success_lines();
    let frames = pump(&mut decoder, &lines[..2]);
    assert_eq!(frames.len(), 2);
    assert_eq!(decoder.summary().session_id.as_deref(), Some("s1"));
}

#[test]
fn resume_uses_the_flavor_method_and_falls_back_to_the_requested_id() {
    let request = LaunchRequest::new("hi").with_resume_session("old-session");
    let mut decoder = AcpDecoder::new(&KIRO, &request);
    decoder.initial_frames();
    // 只回 initialize 应答：会话帧该用 kiro 的 `session/load`。
    let frames = pump(&mut decoder, &[auth_lines()[0].clone()]);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("\"method\":\"session/load\""), "{}", frames[0]);
    assert!(frames[0].contains("\"sessionId\":\"old-session\""), "{}", frames[0]);
    // 对端没回 sessionId 时沿用请求里那个（上游 resolveResumedSessionID 的回退）。
    let frames = pump(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":3,"result":{}}"#.to_owned()],
    );
    assert_eq!(decoder.summary().session_id.as_deref(), Some("old-session"));
    assert!(frames[0].contains("\"sessionId\":\"old-session\""), "{}", frames[0]);
}

#[test]
fn fresh_session_without_an_id_fails_the_run() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    let frames = pump(
        &mut decoder,
        &[
            success_lines()[0].clone(),
            r#"{"jsonrpc":"2.0","id":3,"result":{}}"#.to_owned(),
        ],
    );
    assert_eq!(frames.len(), 1, "只该有会话帧：{frames:?}");
    let error = decoder.summary().terminal_error;
    assert!(error.unwrap_or_default().contains("没有返回会话 id"));
}

#[test]
fn kiro_prompt_carries_both_prompt_and_content() {
    let mut decoder = AcpDecoder::new(&KIRO, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    let frames = pump(&mut decoder, &success_lines());
    let prompt = frames.last().expect("prompt 帧");
    assert!(prompt.contains("\"content\":[{\"text\":\"hi\",\"type\":\"text\"}]"), "{prompt}");
    assert!(prompt.contains("\"prompt\":[{\"text\":\"hi\",\"type\":\"text\"}]"), "{prompt}");
}

#[test]
fn notifications_yield_text_and_usage() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    let collected = events(&mut decoder, &success_lines());
    assert_eq!(text_events(collected), vec!["ok".to_owned()]);
    let summary = decoder.summary();
    assert_eq!(summary.output, "ok");
    assert_eq!(summary.session_id.as_deref(), Some("s1"));
    assert_eq!(summary.usage.len(), 1);
    assert_eq!(summary.usage[0].model, "kimi");
    assert_eq!(summary.usage[0].usage.input, 10);
    assert_eq!(summary.usage[0].usage.output, 5);
    assert_eq!(summary.usage[0].usage.total_tokens, 15);
}

#[test]
fn thought_chunks_become_thinking_events() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    let collected = events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"想"}}}}"#.to_owned()],
    );
    assert_eq!(
        collected,
        vec![RuntimeEvent::Thinking {
            delta: "想".to_owned()
        }]
    );
}

#[test]
fn notification_method_and_update_shapes_are_all_understood() {
    // `session/notification` 也是通知方法；类型也可以走 `type` 键或外部标记包装。
    for line in [
        r#"{"jsonrpc":"2.0","method":"session/notification","params":{"update":{"type":"agent_message_chunk","content":{"type":"text","text":"a"}}}}"#,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"agentMessageChunk":{"content":{"type":"text","text":"b"}}}}}"#,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"Agent_Message-Chunk":{"content":{"type":"text","text":"c"}}}}}"#,
    ] {
        let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
        assert_eq!(text_events(events(&mut decoder, &[line.to_owned()])).len(), 1);
    }
}

#[test]
fn junk_transcript_only_yields_the_valid_chunk() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    let lines: Vec<String> = conformance_junk_stdout("ok")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(text_events(events(&mut decoder, &lines)), vec!["ok".to_owned()]);
}

#[test]
fn deferred_tool_call_waits_for_the_completion_update() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    let start = events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"c1","title":"Run command: ls","kind":"execute","content":[{"type":"content","content":{"type":"text","text":"{\"command\":\"ls\"}"}}]}}}"#.to_owned()],
    );
    assert!(start.is_empty(), "参数是流式来的，起始帧不发 ToolUse");

    let done = events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed","rawOutput":"a\nb"}}}"#.to_owned()],
    );
    assert_eq!(done.len(), 2, "{done:?}");
    match &done[0] {
        RuntimeEvent::ToolUse {
            call_id,
            tool,
            input,
        } => {
            assert_eq!(call_id, "c1");
            assert_eq!(tool, "terminal");
            assert_eq!(input["command"], "ls");
        }
        other => panic!("期望 ToolUse，得到 {other:?}"),
    }
    match &done[1] {
        RuntimeEvent::ToolResult {
            call_id,
            output,
            is_error,
        } => {
            assert_eq!(call_id, "c1");
            assert_eq!(output, "a\nb");
            assert!(!is_error);
        }
        other => panic!("期望 ToolResult，得到 {other:?}"),
    }
}

#[test]
fn tool_call_with_inline_input_is_emitted_immediately() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    let collected = events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"c2","title":"Read file: a.rs","kind":"read","rawInput":{"path":"a.rs"}}}}"#.to_owned()],
    );
    assert_eq!(collected.len(), 1);
    match &collected[0] {
        RuntimeEvent::ToolUse { tool, input, .. } => {
            assert_eq!(tool, "read_file");
            assert_eq!(input["path"], "a.rs");
        }
        other => panic!("期望 ToolUse，得到 {other:?}"),
    }
}

#[test]
fn failed_tool_calls_are_marked_as_errors() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    let collected = events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"c3","status":"failed","output":"boom"}}}"#.to_owned()],
    );
    assert_eq!(collected.len(), 2, "延迟发射：ToolUse + ToolResult");
    match &collected[1] {
        RuntimeEvent::ToolResult { is_error, .. } => assert!(is_error),
        other => panic!("期望 ToolResult，得到 {other:?}"),
    }
}

#[test]
fn usage_updates_merge_monotonically() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    events(
        &mut decoder,
        &[
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"usage_update","usage":{"inputTokens":10,"outputTokens":2}}}}"#.to_owned(),
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"usage_update","usage":{"input_tokens":40,"output_tokens":7,"cache_read_tokens":3,"total_tokens":50}}}}"#.to_owned(),
        ],
    );
    let usage = decoder.summary().usage;
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].usage.input, 40, "同一快照重复/偏小都不该覆盖大值");
    assert_eq!(usage[0].usage.output, 7);
    assert_eq!(usage[0].usage.cache_read, 3);
    assert_eq!(usage[0].usage.total_tokens, 50);
}

#[test]
fn permission_requests_prefer_single_use_grants_and_fail_closed() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    let reply = events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":99,"method":"session/request_permission","params":{"options":[{"optionId":"always","kind":"allow_always"},{"optionId":"ok","kind":"allow_once"}]}}"#.to_owned()],
    );
    assert!(reply.is_empty(), "回帧走 outbox，不是事件");
    let frames = decoder.take_outbox();
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("\"optionId\":\"ok\""), "{}", frames[0]);

    // 只有会话级授权 → 选它；只有永久授权 → 协议错误。
    let frames = pump(
        &mut decoder,
        &[
            r#"{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{"options":[{"optionId":"approve_for_session","kind":"allow_always"}]}}"#.to_owned(),
            r#"{"jsonrpc":"2.0","id":101,"method":"session/request_permission","params":{"options":[{"optionId":"always","kind":"allow_always"}]}}"#.to_owned(),
        ],
    );
    assert!(frames[0].contains("\"optionId\":\"approve_for_session\""), "{}", frames[0]);
    assert!(frames[1].contains("-32603"), "{}", frames[1]);
    assert!(frames[1].contains("no auto-selectable permission option offered"));
}

#[test]
fn permission_requests_deny_a_single_action_with_reject_once() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    let frames = pump(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":5,"method":"session/request_permission","params":{"options":[{"optionId":"no","kind":"reject_once"}]}}"#.to_owned()],
    );
    assert!(frames[0].contains("\"optionId\":\"no\""), "{}", frames[0]);
}

#[test]
fn terminal_requests_and_unknown_methods_are_refused() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    let frames = pump(
        &mut decoder,
        &[
            r#"{"jsonrpc":"2.0","id":7,"method":"terminal/create","params":{}}"#.to_owned(),
            r#"{"jsonrpc":"2.0","id":8,"method":"fs/read_text_file","params":{}}"#.to_owned(),
        ],
    );
    assert!(frames[0].contains("-32601"), "{}", frames[0]);
    assert!(frames[0].contains("terminal capability is not enabled"));
    assert!(frames[1].contains("method not found: fs/read_text_file"), "{}", frames[1]);
}

#[test]
fn cancel_sends_session_cancel_once_and_then_stops() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    let lines = success_lines();
    // 走到正文通知（会话 id 已到手），但还没收到 prompt 应答。
    events(&mut decoder, &lines[..3]);
    let frames = decoder.cancel_frames();
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("\"method\":\"session/cancel\""), "{}", frames[0]);
    assert!(frames[0].contains("\"sessionId\":\"s1\""), "{}", frames[0]);
    assert!(decoder.cancel_frames().is_empty(), "取消帧只发一次");
}

#[test]
fn cancelled_run_before_any_session_has_nothing_to_send() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    assert!(decoder.cancel_frames().is_empty(), "还没有 sessionId，没什么可取消");
}

#[test]
fn truncated_stream_is_not_a_failure() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    events(&mut decoder, &success_lines()[..1]);
    assert!(decoder.finish().is_empty());
    assert_eq!(decoder.summary().terminal_error, None);
    assert_eq!(decoder.summary().session_id, None);
}

#[test]
fn prompt_rpc_error_marks_the_run_failed() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    events(&mut decoder, &success_lines()[..2]);
    let collected = events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":6,"error":{"code":-32603,"message":"Internal error","data":"boom"}}"#.to_owned()],
    );
    assert_eq!(collected.len(), 1);
    let error = decoder.summary().terminal_error.expect("终态失败");
    assert!(error.contains("-32603"), "{error}");
    assert!(error.contains("boom"), "{error}");
}

#[test]
fn initialize_failure_stops_the_handshake() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    let collected = events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32700,"message":"parse error"}}"#.to_owned()],
    );
    assert_eq!(collected.len(), 1);
    assert!(decoder.take_outbox().is_empty(), "握手失败不该再往下走");
    assert!(decoder
        .summary()
        .terminal_error
        .unwrap_or_default()
        .contains("initialize"));
}

#[test]
fn stop_reason_cancelled_marks_the_run_failed() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    events(&mut decoder, &success_lines()[..2]);
    events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":6,"result":{"stopReason":"cancelled"}}"#.to_owned()],
    );
    assert!(decoder
        .summary()
        .terminal_error
        .unwrap_or_default()
        .contains("cancelled"));
}

#[test]
fn responses_from_the_wrong_phase_are_ignored() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    // 还没发会话帧就收到 id=3 的应答：忽略（不建会话、不炸）。
    let collected = events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":3,"result":{"sessionId":"s9"}}"#.to_owned()],
    );
    assert!(collected.is_empty());
    assert_eq!(decoder.summary().session_id, None);
    assert!(decoder.take_outbox().is_empty());
}

#[test]
fn floating_point_request_ids_are_accepted() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    let frames = pump(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":1.0,"result":{"protocolVersion":1}}"#.to_owned()],
    );
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("\"method\":\"session/new\""));
}

#[test]
fn thinking_level_goes_through_set_config_option() {
    let request = LaunchRequest::new("hi").with_thinking_level("high");
    let mut decoder = AcpDecoder::new(&KIMI, &request);
    decoder.initial_frames();
    let frames = pump(&mut decoder, &success_lines()[..2]);
    assert_eq!(frames.len(), 2, "会话帧 + set_config 帧：{frames:?}");
    assert!(
        frames[1].contains("\"method\":\"session/set_config_option\""),
        "{}",
        frames[1]
    );
    assert!(frames[1].contains("\"configId\":\"thinking\""), "{}", frames[1]);
    assert!(frames[1].contains("\"value\":\"high\""), "{}", frames[1]);
    // set_config 的应答（id=5）到了才发 prompt。
    let frames = pump(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":5,"result":{}}"#.to_owned()],
    );
    assert!(frames[0].contains("\"method\":\"session/prompt\""), "{}", frames[0]);
}

#[test]
fn thinking_level_failure_does_not_block_the_prompt() {
    let request = LaunchRequest::new("hi").with_thinking_level("high");
    let mut decoder = AcpDecoder::new(&KIMI, &request);
    decoder.initial_frames();
    let frames = pump(&mut decoder, &success_lines()[..2]);
    assert_eq!(frames.len(), 2, "会话帧 + set_config 帧：{frames:?}");
    let collected = events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":5,"error":{"code":-32601,"message":"unknown config"}}"#.to_owned()],
    );
    assert_eq!(
        collected.len(),
        2,
        "一条 Error（推理等级没生效）+ 一条 Progress（prompt 已发出）：{collected:?}"
    );
    assert_eq!(decoder.summary().terminal_error, None, "推理等级失败不改终态");
    let frames = decoder.take_outbox();
    assert!(frames[0].contains("\"method\":\"session/prompt\""), "{}", frames[0]);
}

#[test]
fn model_is_selected_before_the_prompt() {
    let request = LaunchRequest::new("hi").with_model("kimi-k2");
    let mut decoder = AcpDecoder::new(&KIMI, &request);
    decoder.initial_frames();
    let frames = pump(&mut decoder, &success_lines()[..2]);
    assert_eq!(frames.len(), 2);
    assert!(
        frames[1].contains("\"method\":\"session/set_model\""),
        "{}",
        frames[1]
    );
    assert!(frames[1].contains("\"modelId\":\"kimi-k2\""), "{}", frames[1]);
    let frames = pump(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":4,"result":{}}"#.to_owned()],
    );
    assert!(frames[0].contains("\"method\":\"session/prompt\""), "{}", frames[0]);
}

#[test]
fn model_switch_failure_is_fatal() {
    let request = LaunchRequest::new("hi").with_model("kimi-k2");
    let mut decoder = AcpDecoder::new(&KIMI, &request);
    decoder.initial_frames();
    let frames = pump(&mut decoder, &success_lines()[..2]);
    assert_eq!(frames.len(), 2, "会话帧 + set_model 帧：{frames:?}");
    events(
        &mut decoder,
        &[r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"unknown model"}}"#.to_owned()],
    );
    let error = decoder.summary().terminal_error.expect("选模型失败要判失败");
    assert!(error.contains("无法切换到模型"), "{error}");
    assert!(decoder.take_outbox().is_empty(), "失败后不该再发 prompt");
}

#[test]
fn grok_authenticates_before_creating_the_session() {
    let mut decoder = AcpDecoder::new(&GROK, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    let lines = auth_lines();
    // id=1 的应答里有 authMethods → 立刻发 authenticate（id=2）。
    let frames = pump(&mut decoder, &lines[..1]);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("\"method\":\"authenticate\""), "{}", frames[0]);
    assert!(frames[0].contains("\"methodId\":\"cached_token\""), "{}", frames[0]);
    assert!(frames[0].contains("\"headless\":true"), "{}", frames[0]);
    // authenticate 应答之后才是会话帧。
    let frames = pump(&mut decoder, &lines[1..2]);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("\"method\":\"session/new\""), "{}", frames[0]);
}

#[test]
fn grok_without_a_usable_auth_method_fails_the_run() {
    let mut decoder = AcpDecoder::new(&GROK, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    events(
        &mut decoder,
        &[
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"oauth2"}]}}"#.to_owned(),
        ],
    );
    let error = decoder.summary().terminal_error.expect("认证不可用要判失败");
    assert!(error.contains("unsupported authentication methods"), "{error}");
    assert!(decoder.take_outbox().is_empty());
}

#[test]
fn usage_model_falls_back_to_the_meta_model_id() {
    let mut decoder = AcpDecoder::new(&KIMI, &LaunchRequest::new("hi"));
    decoder.initial_frames();
    events(&mut decoder, &success_lines()[..2]);
    events(
        &mut decoder,
        &[
            r#"{"jsonrpc":"2.0","id":6,"result":{"stopReason":"end_turn","_meta":{"modelId":"grok-4"},"usage":{"inputTokens":1,"outputTokens":2}}}"#
                .to_owned(),
        ],
    );
    let usage = decoder.summary().usage;
    assert_eq!(usage[0].model, "grok-4");
}

/// 见 `mod.rs`：`ID_*` 是握手各帧的固定 id（1..=6），回放脚本也按它断言。
#[test]
fn resume_methods_match_upstream_per_provider_choice() {
    assert_eq!(AcpResume::Resume.method(), "session/resume");
    assert_eq!(AcpResume::Load.method(), "session/load");
}

/// 共享 transcript 的帧顺序是**契约**：正文通知必须在 prompt 应答之前，
/// 否则取消用例拿不到 sessionId（`conformance.rs` 的假 CLI 靠这个顺序）。
#[test]
fn success_transcript_has_the_probe_friendly_frame_order() {
    let text = conformance_success_stdout("s1", "ok", false);
    let ids: Vec<i64> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|value| value.get("id").and_then(serde_json::Value::as_i64))
        .collect();
    assert_eq!(ids, vec![1, 3, 6]);
    let chunk = text.find("agent_message_chunk").expect("正文通知");
    let prompt = text.find("\"stopReason\"").expect("prompt 应答");
    assert!(chunk < prompt);
}

#[test]
fn shared_transcripts_only_use_frames_the_decoder_understands() {
    let junk = conformance_junk_stdout("ok");
    assert!(junk.starts_with("kimi: 无法解析的横幅"));
    assert!(junk.contains("future.chunk"));
}
