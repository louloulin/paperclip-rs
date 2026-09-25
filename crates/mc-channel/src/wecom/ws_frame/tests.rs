//! `ws_frame` 的用例（上游 `wecom/ws_frame_test.go` 的等价面）。
//!
//! 三条纪律（对手写用例同样适用）：
//!
//! 1. **凭据的断言只断"脱敏了"，不断"值是什么"** —— 用 `assert!(!rendered.contains(secret))`
//!    而不是 `assert_eq!(rendered, format!("…{secret}…"))`，因为后者会把明文写进
//!    `assert_eq!` 的失败回显。
//! 2. **上游注释点名的反例都要有**：KELVIN SIGN 那个切片越界、空收尾帧、整条超限被拒。
//! 3. 门 ⑩ 的 800 行硬限 ⇒ 用例文件大就再拆（`tests.rs` + `tests/*.rs`）。

use super::*;

fn msg_callback_json() -> serde_json::Value {
    serde_json::json!({
        "cmd": "aibot_msg_callback",
        "headers": {"req_id": "req-1"},
        "body": {
            "msgid": "msg-1",
            "aibotid": "bot-1",
            "chatid": "chat-1",
            "chattype": "group",
            "from": {"userid": "u-1"},
            "msgtype": "text",
            "text": {"content": "hello"},
        }
    })
}

#[test]
fn decode_msg_callback_keeps_every_field() {
    let raw = serde_json::to_vec(&msg_callback_json()).unwrap();
    let frame = decode_frame(&raw).expect("decodes");
    let Frame::MsgCallback { req_id, callback } = frame else {
        panic!("expected a msg callback, got {frame:?}");
    };
    assert_eq!(req_id, "req-1");
    assert_eq!(callback.msgid, "msg-1");
    assert_eq!(callback.aibotid, "bot-1");
    assert_eq!(callback.chatid, "chat-1");
    assert_eq!(callback.chattype, "group");
    assert_eq!(callback.from.userid, "u-1");
    assert_eq!(callback.msgtype, "text");
    assert_eq!(callback.text.content, "hello");
}

#[test]
fn decode_voice_carries_the_transcript_not_audio() {
    // 上游逐字：WeCom 在自己那侧做完语音识别，只投结果 ⇒ `voice.content` 是一句话。
    let raw = serde_json::to_vec(&serde_json::json!({
        "cmd": "aibot_msg_callback",
        "headers": {"req_id": "r"},
        "body": {"msgtype": "voice", "voice": {"content": "  登录坏了  "}}
    }))
    .unwrap();
    let Frame::MsgCallback { callback, .. } = decode_frame(&raw).unwrap() else {
        panic!("expected a msg callback");
    };
    assert_eq!(callback.voice.content, "  登录坏了  ");
}

#[test]
fn decode_mixed_message_reads_every_run_in_order() {
    let raw = serde_json::to_vec(&serde_json::json!({
        "cmd": "aibot_msg_callback",
        "headers": {"req_id": "r"},
        "body": {
            "msgtype": "mixed",
            "mixed": {"msg_item": [
                {"msgtype": "text", "text": {"content": "看看这个"}},
                {"msgtype": "image", "image": {"url": "https://cos.example/a", "aeskey": "k"}},
                {"msgtype": "voice", "voice": {"content": "然后呢"}},
            ]}
        }
    }))
    .unwrap();
    let Frame::MsgCallback { callback, .. } = decode_frame(&raw).unwrap() else {
        panic!("expected a msg callback");
    };
    let runs = &callback.mixed.msg_item;
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[0].text.content, "看看这个");
    assert_eq!(runs[1].image.url, "https://cos.example/a");
    assert_eq!(runs[2].voice.content, "然后呢");
}

#[test]
fn decode_quote_nests_one_level_deeper_for_mixed() {
    // 上游逐字：引用的图文混排比普通的一段**多嵌一层**。
    let raw = serde_json::to_vec(&serde_json::json!({
        "cmd": "aibot_msg_callback",
        "headers": {"req_id": "r"},
        "body": {
            "msgtype": "text", "text": {"content": "这个怎么处理"},
            "quote": {"msgtype": "mixed", "mixed": {"msg_item": [
                {"msgtype": "text", "text": {"content": "告警"}}]}}
        }
    }))
    .unwrap();
    let Frame::MsgCallback { callback, .. } = decode_frame(&raw).unwrap() else {
        panic!("expected a msg callback");
    };
    assert_eq!(callback.quote.msgtype, "mixed");
    assert_eq!(callback.quote.mixed.msg_item[0].text.content, "告警");
}

#[test]
fn decode_quote_mirrors_content_with_a_flat_media_body() {
    let raw = serde_json::to_vec(&serde_json::json!({
        "cmd": "aibot_msg_callback",
        "headers": {"req_id": "r"},
        "body": {"msgtype": "text", "text": {"content": "?"},
                 "quote": {"msgtype": "image", "image": {"url": "https://cos.example/q"}}}
    }))
    .unwrap();
    let Frame::MsgCallback { callback, .. } = decode_frame(&raw).unwrap() else {
        panic!("expected a msg callback");
    };
    assert_eq!(callback.quote.image.url, "https://cos.example/q");
}

#[test]
fn decode_event_callback_reads_the_event_type() {
    let raw = serde_json::to_vec(&serde_json::json!({
        "cmd": "aibot_event_callback",
        "headers": {"req_id": "ev-1"},
        "body": {"event": {"eventtype": "disconnected_event"}}
    }))
    .unwrap();
    let Frame::EventCallback { req_id, event } = decode_frame(&raw).unwrap() else {
        panic!("expected an event callback");
    };
    assert_eq!(req_id, "ev-1");
    assert_eq!(event.event.eventtype, EVENT_DISCONNECTED);
}

#[test]
fn decode_ack_is_a_response_carrying_the_verdict() {
    let raw = serde_json::to_vec(&serde_json::json!({
        "headers": {"req_id": "req-9"}, "errcode": 846_608, "errmsg": "expired"
    }))
    .unwrap();
    let Frame::Response(envelope) = decode_frame(&raw).unwrap() else {
        panic!("expected a response");
    };
    assert_eq!(envelope.headers.req_id, "req-9");
    assert_eq!(envelope.errcode, ERRCODE_STREAM_EXPIRED);
    assert_eq!(envelope.error_message, "expired");
    assert!(envelope.is_ack());
    assert_eq!(
        Frame::Response(envelope).req_id(),
        "req-9",
        "req_id() must read through the response variant too"
    );
}

#[test]
fn decode_ping_and_pong_are_their_own_variants() {
    let server_ping = serde_json::to_vec(&serde_json::json!({"cmd": "ping"})).unwrap();
    assert!(matches!(
        decode_frame(&server_ping).unwrap(),
        Frame::ServerPing { .. }
    ));
    let reply_pong = serde_json::to_vec(&serde_json::json!({"cmd": "pong"})).unwrap();
    assert!(matches!(
        decode_frame(&reply_pong).unwrap(),
        Frame::Pong { .. }
    ));
}

#[test]
fn decode_unknown_command_is_skipped_rather_than_failed() {
    // 上游读循环对不认识的帧是"跳过"，而且 WeCom 会加新命令 ⇒ 这不是错误。
    let raw = serde_json::to_vec(&serde_json::json!({"cmd": "aibot_something_new"})).unwrap();
    let Frame::Unknown { cmd, .. } = decode_frame(&raw).unwrap() else {
        panic!("expected an unknown frame");
    };
    assert_eq!(cmd, "aibot_something_new");
}

#[test]
fn decode_rejects_a_command_that_only_ever_goes_the_other_way() {
    let raw = serde_json::to_vec(&serde_json::json!({"cmd": CMD_SUBSCRIBE})).unwrap();
    assert_eq!(
        decode_frame(&raw),
        Err(FrameError::NotInbound {
            cmd: CMD_SUBSCRIBE.to_owned()
        })
    );
}

#[test]
fn decode_rejects_malformed_json_and_a_bad_body_shape() {
    assert!(matches!(
        decode_frame(b"not json"),
        Err(FrameError::Malformed { .. })
    ));
    let raw = serde_json::to_vec(&serde_json::json!({
        "cmd": "aibot_msg_callback", "body": {"from": "not an object"}
    }))
    .unwrap();
    assert!(matches!(
        decode_frame(&raw),
        Err(FrameError::Malformed { .. })
    ));
}

#[test]
fn decode_rejects_a_frame_past_the_cap_before_parsing_it() {
    // 大小先于解析：一条超限的帧**不进** `serde_json`（本仓新增，docs/32 §33 的 D3）。
    let blob = vec![b'a'; MAX_FRAME_BYTES + 1];
    assert_eq!(
        decode_frame(&blob),
        Err(FrameError::TooLarge {
            len: MAX_FRAME_BYTES + 1,
            limit: MAX_FRAME_BYTES,
        })
    );
    let empty_frame = serde_json::to_vec(&serde_json::json!({"cmd": "ping"})).unwrap();
    assert!(decode_frame(&empty_frame).is_ok());
}

#[test]
fn encode_frame_round_trips_and_enforces_the_cap() {
    let bytes = encode_frame(&frame_with("r-1", CMD_PING, Value::Null)).unwrap();
    let Frame::ServerPing { req_id } = decode_frame(&bytes).unwrap() else {
        panic!("expected a ping");
    };
    assert_eq!(req_id, "r-1");
}

#[test]
fn frame_with_shape_is_exactly_cmd_headers_body() {
    let frame = frame_with("r-2", CMD_SEND_MSG, serde_json::json!({"a": 1}));
    assert_eq!(frame["cmd"], CMD_SEND_MSG);
    assert_eq!(frame["headers"]["req_id"], "r-2");
    assert_eq!(frame["body"]["a"], 1);
    assert!(
        frame.get("errcode").is_none(),
        "our frames carry no verdict"
    );
}

#[test]
fn new_req_id_is_sixteen_hex_chars_and_never_repeats() {
    let first = new_req_id();
    assert_eq!(first.len(), 16, "8 random bytes hex-encoded");
    assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    let mut seen = std::collections::HashSet::new();
    for _ in 0..64 {
        assert!(seen.insert(new_req_id()), "req_id must not collide");
    }
}

#[test]
fn new_stream_id_is_prefixed_and_never_repeats() {
    // 复用同一个 id 会**替换**那条消息的正文 ⇒ 跨并发轮次绝不许撞。
    let id = new_stream_id();
    assert!(id.starts_with('s'), "{id}");
    assert_eq!(id.len(), 25);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..64 {
        assert!(seen.insert(new_stream_id()));
    }
}

// =====================================================================
// 凭据面（DoD 第 6 条）
// =====================================================================

const SUBSCRIBE_SECRET: &str = "DO-NOT-LOG-wecom-long-connection-secret";

#[test]
fn subscribe_body_never_renders_the_secret() {
    let secret = PlaintextSecret::new(SUBSCRIBE_SECRET);
    let body = subscribe_body("bot-1", &secret);
    let rendered = format!("{body:?}");
    assert!(rendered.contains("SubscribeBody"));
    assert!(rendered.contains("bot-1"), "the bot id is not a credential");
    assert!(rendered.contains("<redacted>"));
    assert!(
        !rendered.contains(SUBSCRIBE_SECRET),
        "the Debug of the subscribe body must not carry the plaintext"
    );
}

#[test]
fn a_refused_subscribe_never_echoes_the_secret() {
    // DoD 第 6 条的"错误路径不回显凭据"（照 docs/33 §12.2 的
    // `execenv_errors_never_echo_file_contents` 同款）。
    let secret = PlaintextSecret::new(SUBSCRIBE_SECRET);
    let error = FrameError::Malformed {
        message: "aibot_subscribe: invalid character at line 1".to_owned(),
    };
    let rendered = format!("{error} / {error:?}");
    assert!(
        !rendered.contains(SUBSCRIBE_SECRET),
        "neither the error nor its Debug may carry the secret"
    );
    // 而**唯一**的明文出口确实带着它 —— 否则上面那条断言是空的（
    // [`SubscribeBody::into_value`] 的命名刺眼正是为了这件事）。
    let exposed = serde_json::to_string(&subscribe_body("bot-1", &secret).into_value()).unwrap();
    assert!(exposed.contains(SUBSCRIBE_SECRET));
    assert_eq!(subscribe_body("bot-1", &secret).bot_id(), "bot-1");
}

#[test]
fn media_body_never_renders_its_signed_url_or_key() {
    // url 是五分钟有效的预签名地址（能直接把密文取回来），aeskey 是解它的密钥。
    let body = MediaBody {
        url: "https://cos.example/secret-object".to_owned(),
        aeskey: "DO-NOT-LOG-aes-key".to_owned(),
    };
    let rendered = format!("{body:?}");
    assert!(!rendered.contains("cos.example"));
    assert!(!rendered.contains("DO-NOT-LOG-aes-key"));
    assert_eq!(
        rendered,
        "MediaBody { url: Some(\"<redacted>\"), aeskey: Some(\"<redacted>\") }"
    );
    let empty = MediaBody::default();
    assert_eq!(
        format!("{empty:?}"),
        "MediaBody { url: None, aeskey: None }",
        "an absent field must read as absent rather than as redacted"
    );
}

#[test]
fn a_decoded_callback_never_renders_a_media_key() {
    let raw = serde_json::to_vec(&serde_json::json!({
        "cmd": "aibot_msg_callback", "headers": {"req_id": "r"},
        "body": {"msgtype": "image", "image": {"url": "https://cos.example/x", "aeskey": "DO-NOT-LOG-key"}}
    }))
    .unwrap();
    let frame = decode_frame(&raw).unwrap();
    let rendered = format!("{frame:?}");
    assert!(!rendered.contains("DO-NOT-LOG-key"));
    assert!(!rendered.contains("cos.example"));
}

#[test]
fn quoted_message_debug_counts_runs_instead_of_dumping_them() {
    let quoted = QuotedMessage {
        msgtype: "mixed".to_owned(),
        mixed: MixedBody {
            msg_item: vec![MixedItem::default(), MixedItem::default()],
        },
        ..QuotedMessage::default()
    };
    let rendered = format!("{quoted:?}");
    assert!(rendered.contains("mixed_items: 2"), "{rendered}");
}

// =====================================================================
// 出站 body
// =====================================================================

#[test]
fn send_msg_text_body_ships_markdown_and_checks_addressing() {
    // 上游逐字：aibot_send_msg 只接受 markdown / template_card —— text **不**被接受。
    let body = send_msg_text_body("chat-1", CHAT_TYPE_GROUP_INT, "hi").unwrap();
    assert_eq!(body["chatid"], "chat-1");
    assert_eq!(body["chat_type"], 2);
    assert_eq!(body["msgtype"], "markdown");
    assert_eq!(body["markdown"]["content"], "hi");
    assert!(body.get("text").is_none());
}

#[test]
fn send_msg_text_body_refuses_a_missing_chat_and_a_bad_chat_type() {
    assert_eq!(
        send_msg_text_body("", CHAT_TYPE_SINGLE_INT, "x"),
        Err(BodyError::MissingChatId)
    );
    assert_eq!(
        send_msg_text_body("chat-1", 3, "x"),
        Err(BodyError::BadChatType)
    );
    assert!(send_msg_text_body("chat-1", CHAT_TYPE_SINGLE_INT, "x").is_ok());
}

#[test]
fn aibot_chat_type_maps_p2p_to_one_and_group_to_two() {
    assert_eq!(aibot_chat_type_from_channel(ChatType::P2p), 1);
    assert_eq!(aibot_chat_type_from_channel(ChatType::Group), 2);
}

#[test]
fn respond_stream_body_checks_the_stream_id_and_the_closing_frame() {
    let body = respond_stream_body("s-1", "工作中", false).unwrap();
    assert_eq!(body["msgtype"], "stream");
    assert_eq!(body["stream"]["id"], "s-1");
    assert_eq!(body["stream"]["finish"], false);
    assert_eq!(body["stream"]["content"], "工作中");

    assert_eq!(
        respond_stream_body("", "x", false),
        Err(BodyError::MissingStreamId)
    );
    // 一帧全是空格的收尾什么也封不住，留给用户一个永远转圈的气泡 ⇒ 在这里拒掉。
    assert_eq!(
        respond_stream_body("s-1", "   \n\t ", true),
        Err(BodyError::EmptyClosingFrame)
    );
    assert!(
        respond_stream_body("s-1", "", false).is_ok(),
        "a non-closing frame may be empty"
    );
}

#[test]
fn the_opening_frame_is_the_thinking_affordance() {
    let body = respond_stream_body("s-1", STREAM_THINKING_PLACEHOLDER, false).unwrap();
    assert_eq!(body["stream"]["content"], "<think></think>");
    assert_eq!(
        body["stream"]["finish"], false,
        "the placeholder is the opening frame, not a closing one"
    );
}

#[test]
fn defusing_only_applies_to_the_closing_frame() {
    // 开场帧**就是**那个动效 ⇒ 只在收尾时脱敏（上游 `respondStreamBody` 的 if）。
    let opening = respond_stream_body("s-1", "<think></think>", false).unwrap();
    assert_eq!(opening["stream"]["content"], "<think></think>");
    let closing = respond_stream_body("s-1", "safe <think>ok</think>", true).unwrap();
    let content = closing["stream"]["content"].as_str().unwrap();
    assert!(content.contains("<\u{200b}think>"), "{content}");
    assert!(content.contains("<\u{200b}/think>"), "{content}");
}

#[test]
fn respond_stream_body_truncates_to_the_protocol_cap() {
    let long = "汉".repeat(STREAM_CONTENT_LIMIT);
    let body = respond_stream_body("s-1", &long, false).unwrap();
    let content = body["stream"]["content"].as_str().unwrap();
    assert!(content.len() <= STREAM_CONTENT_LIMIT);
    assert!(content.ends_with('…'));
}

// =====================================================================
// 文本工具
// =====================================================================

#[test]
fn think_tags_are_defused_with_a_zero_width_space() {
    assert_eq!(defuse_think_tags("no angle brackets"), "no angle brackets");
    assert_eq!(
        defuse_think_tags("<think>x</think>"),
        format!("<{ZERO_WIDTH_SPACE}think>x<{ZERO_WIDTH_SPACE}/think>")
    );
    assert_eq!(
        defuse_think_tags("<THINK>x"),
        format!("<{ZERO_WIDTH_SPACE}THINK>x"),
        "matching is case-insensitive"
    );
    // 上游只比 `<` 之后那**五个字节**，**不**要求后面跟一个 `>` ⇒ `<thinking>` 同样被
    // 脱敏（这不是宽松：宁可多插一个零宽空格，也不要把半条回答折进一个收不回的折叠里）。
    assert_eq!(
        defuse_think_tags("<thinking>"),
        format!("<{ZERO_WIDTH_SPACE}thinking>")
    );
    // 只动标签自己的开头：比较、泛型、HTML 样例、以及不是 think 的标签都原样通过。
    assert_eq!(
        defuse_think_tags("a < b and <thought>"),
        "a < b and <thought>"
    );
    assert_eq!(defuse_think_tags("< thing"), "< thing");
    assert_eq!(defuse_think_tags("<thin"), "<thin");
}

#[test]
fn defusing_survives_the_kelvin_sign_that_broke_the_folded_scan() {
    // 上游逐字：U+212A 是三字节、折叠成单字节的 "k" ⇒ 按折叠副本的偏移切原串会越界，
    // 而扫的是 **agent 自己的回答**。这条用例就是它不 panic、且输出仍是合法 UTF-8。
    let kelvin = "KK<x"; // 两个 U+212A 加一个尖括号
    let out = defuse_think_tags(kelvin);
    assert_eq!(out, kelvin);
    let mixed = format!("{kelvin}<think>");
    let out = defuse_think_tags(&mixed);
    assert!(out.contains(&format!("<{ZERO_WIDTH_SPACE}think>")));
    assert!(out.is_char_boundary(out.len()));
}

#[test]
fn truncation_cuts_on_a_character_boundary() {
    assert_eq!(truncate_stream_content("short"), "short");
    let long = "汉".repeat(STREAM_CONTENT_LIMIT);
    let cut = truncate_stream_content(&long);
    assert!(cut.len() <= STREAM_CONTENT_LIMIT);
    assert!(cut.ends_with('…'));
    // 切点落在字符中间时往回到边界（三个字节的汉字一个都不许被劈开）。
    assert!(cut.is_char_boundary(cut.len()));
}

#[test]
fn visible_char_is_neither_whitespace_nor_control() {
    assert!(has_visible_char("x"));
    assert!(has_visible_char("  x  "));
    assert!(
        has_visible_char("\u{200b}"),
        "a format rune passes on purpose"
    );
    assert!(!has_visible_char(""));
    assert!(!has_visible_char(" \n\t\r "));
    assert!(!has_visible_char("\u{7}"));
}

#[test]
fn wire_cut_point_prefers_a_late_line_break_then_a_boundary() {
    let text = format!("{}\n{}", "a".repeat(90), "b".repeat(90));
    let cut = wire_cut_point(&text, 100);
    assert_eq!(
        cut, 91,
        "the break sits after the last quarter of the budget"
    );
    // 靠开头的换行不值得取（会浪费大半帧）⇒ 退回 rune 边界。
    let early = format!("x\n{}", "y".repeat(200));
    assert_eq!(wire_cut_point(&early, 100), 100);
    // 预算是 rune 中间时往回到边界 —— **不 panic**（上游 `s[:budget]` 是字节切片，
    // Go 允许在任何字节处切；Rust 的 `&str` 不允许）。
    let wide = "汉".repeat(50);
    assert_eq!(wire_cut_point(&wide, 7), 6);
    assert_eq!(wire_cut_point(&wide, 20472.min(wide.len())), wide.len());
    assert_eq!(wire_cut_point("short", 100), 5);
    // 预算比一个 rune 还窄：往上找一个边界，绝不返回 0（那会让 split_for_wire 死循环）。
    let narrow = wire_cut_point(&wide, 1);
    assert_eq!(narrow, 3);
    assert!(wide.is_char_boundary(narrow));
}

#[test]
fn split_for_wire_passes_a_short_answer_through_untouched() {
    assert_eq!(split_for_wire("短回答"), vec!["短回答".to_owned()]);
    let exact = "a".repeat(SEND_MSG_CONTENT_LIMIT);
    assert_eq!(split_for_wire(&exact), vec![exact]);
}

#[test]
fn split_for_wire_numbers_every_piece_but_the_last() {
    let content = "a".repeat(SEND_MSG_CONTENT_LIMIT * 3);
    let pieces = split_for_wire(&content);
    assert!(pieces.len() >= 3, "got {} pieces", pieces.len());
    for piece in &pieces {
        assert!(piece.len() <= SEND_MSG_CONTENT_LIMIT, "piece over the cap");
    }
    for (index, piece) in pieces.iter().enumerate().take(pieces.len() - 1) {
        let marker = format!("\n\n({}/{})", index + 1, pieces.len());
        assert!(piece.ends_with(&marker), "piece {index} lacks {marker:?}");
    }
    assert!(
        !pieces.last().unwrap().contains("(4/"),
        "the last piece promises nothing after it"
    );
}

#[test]
fn split_for_wire_loses_nothing_at_the_seam() {
    // 上游逐字：切点两侧都保留 ⇒ 去掉标记拼回去逐字节就是原回答。
    let line = format!("{}\n", "z".repeat(199));
    let content = line.repeat(SEND_MSG_CONTENT_LIMIT * 2 / line.len() + 2);
    let pieces = split_for_wire(&content);
    assert!(pieces.len() > 1);
    let mut rebuilt = String::new();
    for (index, piece) in pieces.iter().enumerate() {
        let stripped = if index + 1 == pieces.len() {
            piece.as_str()
        } else {
            let at = piece
                .rfind("\n\n(")
                .expect("a marker on every piece but the last");
            &piece[..at]
        };
        rebuilt.push_str(stripped);
    }
    assert_eq!(rebuilt, content);
}

#[test]
fn split_for_wire_drops_a_piece_with_nothing_visible_in_it() {
    // 一条以连续空行结尾的长回答会把那段空行切成单独一段；它到聊里就是一个空气泡。
    // 让第一个切点正好落在那段空行的**第一个换行**上（`wire_cut_point` 偏好预算后四分
    // 之一的换行），于是剩下的是纯空白、会被丢掉。
    let content = format!(
        "{}\n{}",
        "a".repeat(SEND_MSG_CONTENT_LIMIT - 9),
        "\n".repeat(SEND_MSG_CONTENT_LIMIT)
    );
    let pieces = split_for_wire(&content);
    assert!(
        pieces.iter().all(|piece| has_visible_char(piece)),
        "every piece must have something visible: {} pieces",
        pieces.len()
    );
    assert_eq!(
        pieces.len(),
        1,
        "the blank tail is not a message of its own"
    );
}

// =====================================================================
// 信封
// =====================================================================

#[test]
fn envelope_defaults_match_the_go_zero_values() {
    let envelope: FrameEnvelope = serde_json::from_str("{}").unwrap();
    assert!(envelope.cmd.is_empty());
    assert!(envelope.headers.req_id.is_empty());
    assert!(envelope.body.is_null());
    assert_eq!(envelope.errcode, 0);
    assert!(!envelope.is_ack());
    assert_eq!(value_of(&envelope), "null");
}

fn value_of(envelope: &FrameEnvelope) -> String {
    envelope.body.to_string()
}

#[test]
fn headers_serialise_an_empty_req_id_rather_than_omitting_it() {
    // 上游两个字段都没有 `omitempty` ⇒ 空串照常序列化，本仓不得偷偷加上。
    let headers = FrameHeaders::default();
    assert_eq!(serde_json::to_string(&headers).unwrap(), r#"{"req_id":""}"#);
}
