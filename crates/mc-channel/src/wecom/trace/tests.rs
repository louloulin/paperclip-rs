//! `trace` 的用例：上游 `trace_test.go` 那六条护栏的**本仓**对应物。
//!
//! # 为什么读开关的用例只有**两个**，而且它们串行
//!
//! `TRACING` 是**进程级**原子，而 `cargo test` 在**同一个 binary 里并行**跑用例 ⇒ 几个都读它的
//! 用例会互相顶掉假设（`strings.rs` 的 `DEPLOYMENT_LOCALE` 那条实测教训同款，`docs/32` §27）。
//! 所以：与开关有关的话题合成**两个**用例（`switch_guard()` 让它们串行），其余一律是**纯函数**
//! （截断 / 脱敏 / 边界），不读开关，可以与它们并行。

use std::sync::{Mutex, MutexGuard};

use serde_json::json;

use super::*;

// =====================================================================
// 夹具
// =====================================================================

/// 与 `replier.rs` 的 `sendBindingPrompt` 拼出来的 URL 同形（`Mint` 产出的 **43 字符**
/// base64url token：32 个随机字节、无填充）。
const PRODUCTION_SHAPED_TOKEN: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

/// 上游那条绑定提示的开头（逐字）。
const BINDING_PROMPT_HEAD: &str = "👋 请先绑定你的 Multica 账号，才能与我对话：\n";

/// 读 / 写进程级开关的用例用的串行闸（见模块文档）。
static SWITCH: Mutex<()> = Mutex::new(());

fn switch_guard() -> MutexGuard<'static, ()> {
    match SWITCH.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn binding_prompt(token: &str) -> String {
    format!("{BINDING_PROMPT_HEAD}https://multica.example/wecom/bind?token={token}")
}

fn subscribe_frame(secret: &str) -> serde_json::Value {
    json!({
        "cmd": "aibot_subscribe",
        "headers": { "req_id": "req-sub-1" },
        "body": { "bot_id": "BOTID", "secret": secret },
    })
}

fn send_msg_frame(chat_id: &str, content: &str) -> serde_json::Value {
    json!({
        "cmd": "aibot_send_msg",
        "headers": { "req_id": "req-send-1" },
        "body": {
            "chatid": chat_id,
            "chat_type": 2,
            "msgtype": "markdown",
            "markdown": { "content": content },
        },
    })
}

fn inbound_callback() -> AibotMsgCallback {
    serde_json::from_value(json!({
        "msgid": "MSG_1",
        "aibotid": "BOTID",
        "chatid": "GROUP_CHAT_ID",
        "chattype": "group",
        "from": { "userid": "SENDER_USERID" },
        "msgtype": "text",
        "text": { "content": "hello from the room" },
    }))
    .expect("callback fixture")
}

fn named_headers() -> MediaHeaders {
    MediaHeaders {
        filename: "report.docx".to_string(),
        disposition: "attachment; filename=\"report.docx\"".to_string(),
    }
}

/// 把一段文本按上游那条夹具的形态百分号转义（逐字节 `%XX`）。
fn percent_escape(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        out.push('%');
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

// =====================================================================
// 纯函数
// =====================================================================

/// 记录名与 emit 那一侧的字面量是**同一个**字符串。
#[test]
fn the_message_name_matches_the_literal_every_emit_uses() {
    assert_eq!(TRACE_MESSAGE, "wecom trace");
}

/// 开关只认 `"1"`（上游接线逐字的那一个形态）—— 一个笔误不许把一个**记录消息正文**的开关打开。
#[test]
fn the_switch_parses_exactly_one() {
    assert!(parse_trace_switch("1"));
    assert!(parse_trace_switch(" 1 "));
    assert!(parse_trace_switch("1\n"), "trim 之后就是 1");
    for other in [
        "", "0", "on", "true", "TRUE", "yes", "\"1\"", "01", "11", "1 1",
    ] {
        assert!(!parse_trace_switch(other), "{other:?} 必须判关");
    }
    assert_eq!(TRACE_ENV, "MULTICA_WECOM_TRACE");
}

/// `\b` 的 ASCII 词边界、大小写不敏感的参数名、值类 `[\s&\"'<>]`、以及"至少一个字符"。
#[test]
fn redaction_covers_the_token_shapes() {
    let cases = [
        (
            "binding url",
            "go to https://x.test/wecom/bind?token=abc123DEF now",
            "go to https://x.test/wecom/bind?token=[redacted] now",
        ),
        (
            "custom path",
            "https://x.test/custom/path?token=abc123DEF",
            "https://x.test/custom/path?token=[redacted]",
        ),
        (
            "binding_token param",
            "?binding_token=abc123DEF",
            "?binding_token=[redacted]",
        ),
        (
            "access_token param",
            "?access_token=abc123DEF",
            "?access_token=[redacted]",
        ),
        ("oauth code", "?code=abc123DEF", "?code=[redacted]"),
        (
            "stops at ampersand",
            "?token=abc123DEF&next=/home",
            "?token=[redacted]&next=/home",
        ),
        ("stops at quote", "?token=abc\"x", "?token=[redacted]\"x"),
        ("stops at angle", "?token=abc<x>", "?token=[redacted]<x>"),
        ("stops at tab", "?token=abc\tx", "?token=[redacted]\tx"),
        (
            "leaves ordinary text alone",
            "the token is not in a url here",
            "the token is not in a url here",
        ),
        // `\b`：`_` 与字母都是词字符 ⇒ 一个**粘在前面**的参数名不算一个参数名。
        (
            "no boundary before the name",
            "?my_token=abc",
            "?my_token=abc",
        ),
        ("no boundary before code", "xcode=abc", "xcode=abc"),
        // `(?i)`：参数名大小写不敏感，而**原文**被保留。
        ("uppercase name", "?TOKEN=abc", "?TOKEN=[redacted]"),
        (
            "mixed case name",
            "?Access_Token=abc",
            "?Access_Token=[redacted]",
        ),
        // `[^…]+` 至少一个字符 ⇒ 一个空值不匹配，整段原样留着。
        (
            "empty value is left alone",
            "?token=&next=1",
            "?token=&next=1",
        ),
        // 交替顺序：`binding_token` 先于 `token`，所以整个参数名被吃掉而值被换掉。
        (
            "longer name wins",
            "?binding_token=abc",
            "?binding_token=[redacted]",
        ),
    ];
    for (name, input, want) in cases {
        assert_eq!(redact_bearer_tokens(input), want, "{name}");
    }
}

/// 截断按 **rune** 而不是字节：中文正文一次 3 字节，按字节切既截得太早、又可能把一个字符切成
/// 非法 UTF-8 —— 那正是这个开关要服务的部署里"日志再也不可读"的来路。
#[test]
fn the_preview_cuts_on_a_rune_boundary() {
    for body in ["测".repeat(300), format!("x{}", "测".repeat(300))] {
        let got = trace_preview(&body);
        let trimmed = got.strip_suffix('…').expect("越界必须带省略号");
        assert_eq!(trimmed.chars().count(), TRACE_PREVIEW_RUNES);
        assert_ne!(trimmed, got);
        assert!(
            !trimmed.contains('\u{fffd}'),
            "多字节字符被切开了：{trimmed:?}"
        );
    }
    // 刚好在上限上：**不**截断、也**不**加省略号。
    let exact = "测".repeat(TRACE_PREVIEW_RUNES);
    assert_eq!(trace_preview(&exact), exact);
    // 上限 0：上游同样报一个省略号（`len(out) == limit` 在第一次迭代就成立）。
    assert_eq!(trace_bound("abc", 0), "…");
}

/// 一帧就是**一行**：换行与回车都变成空格，而内容不许被丢掉。
#[test]
fn the_preview_flattens_newlines() {
    let got = trace_preview("first\nsecond\r\nthird");
    assert!(!got.contains(['\n', '\r']), "{got:?}");
    assert!(got.contains("first") && got.contains("third"), "{got:?}");
}

/// 上游那条"夹具证明不了任何事"的自检：绑定 token 的最后一个字符**必须**落在预览上限**之内** ——
/// 否则"脱敏是必需的"这条理由就够不着，该重新审。
#[test]
fn the_binding_token_really_does_land_inside_the_preview() {
    let prompt = binding_prompt(PRODUCTION_SHAPED_TOKEN);
    assert_eq!(PRODUCTION_SHAPED_TOKEN.chars().count(), 43);
    assert!(
        prompt.chars().count() <= TRACE_PREVIEW_RUNES,
        "token 结束于第 {} 个 rune，超过 {TRACE_PREVIEW_RUNES} ⇒ 这条脱敏理由要重审",
        prompt.chars().count()
    );
    // 不脱敏时它**确实**在预览里（否则这条用例什么都没证明）。
    let unredacted = trace_bound(&prompt, TRACE_PREVIEW_RUNES);
    assert!(unredacted.contains(PRODUCTION_SHAPED_TOKEN));
    // 脱敏之后它**不**在了，而那一行仍然值得记。
    let preview = trace_preview(&prompt);
    assert!(!preview.contains(PRODUCTION_SHAPED_TOKEN), "{preview}");
    assert!(preview.contains("token=[redacted]"), "{preview}");
    assert!(preview.contains("multica.example"), "{preview}");
}

/// 更大的那个上限仍然是**失控闸**：没有哪个真实的 `Content-Disposition` 摸得到它。
#[test]
fn the_media_header_is_still_bounded() {
    let runaway = format!(
        "attachment; filename=\"{}\"",
        "A".repeat(TRACE_HEADER_RUNES * 2)
    );
    let bounded = trace_bound(&runaway, TRACE_HEADER_RUNES);
    assert!(bounded.ends_with('…'), "越界的头必须被切");
    assert_eq!(bounded.chars().count(), TRACE_HEADER_RUNES + 1);
    assert!(runaway.chars().count() > TRACE_HEADER_RUNES);
}

/// 一个带头内换行的头不许把一次附件拆成两条记录。
#[test]
fn the_media_header_stays_on_one_line() {
    let flattened = trace_bound("attachment;\r\n filename=\"a.docx\"", TRACE_HEADER_RUNES);
    assert!(!flattened.contains(['\n', '\r']), "{flattened:?}");
    assert!(flattened.contains("a.docx"));
}

// =====================================================================
// 开关（串行）
// =====================================================================

/// 🔴 上游那一格：附件头**不**按消息预览的上限截断 —— 一个百分号转义的 CJK 名字会被切成半个转义，
/// 而那半个转义**没法**解回它代表的字符。
#[test]
#[allow(clippy::too_many_lines)] // 上游那一条把"两个参数形式都完整"与"名字也在"钉在一个夹具上
fn the_media_header_is_not_cut_to_the_preview_cap() {
    let _guard = switch_guard();
    let installation = mc_core::id::Id::new();
    let name = "季度经营分析报告最终版本二零二六.docx";
    let encoded = percent_escape(name);
    let raw = format!("attachment; filename=\"{encoded}\"; filename*=UTF-8''{encoded}");
    assert!(
        raw.chars().count() > TRACE_PREVIEW_RUNES,
        "夹具证明不了任何事：头只有 {} 个 rune，没到 {}",
        raw.chars().count(),
        TRACE_PREVIEW_RUNES
    );

    let was_on = tracing_on();
    set_trace(true);
    let fields = media_headers_fields(
        "MSGID-MEDIA",
        0,
        &MediaHeaders {
            filename: name.to_string(),
            disposition: raw.clone(),
        },
        Some(installation),
    )
    .expect("开着就必须有字段");
    set_trace(was_on);

    let rendered = fields.render();
    assert_eq!(
        fields.get("content_disposition").matches(&encoded).count(),
        2,
        "两个参数形式都必须完整留着：{rendered}"
    );
    assert!(
        !fields.get("content_disposition").contains('…'),
        "头被截断了，而那正是这一行要防的"
    );
    assert!(
        rendered.contains(&format!("filename={name}")),
        "解出来的名字也要在：{rendered}"
    );
    assert_eq!(fields.get("dir"), "in.media");
    assert_eq!(fields.get("installation_id"), installation.to_string());
}

/// 上游那六条护栏里与开关有关的那几条，一次走完：
///
/// 1. **关着 ⇒ 什么都不记**：五个点全部 `None`（而"记一条 debug 也行"那条退路**不存在** ——
///    本仓的记录走 `info`，开关是**唯一**闸）。
/// 2. **开着 ⇒ 两个方向都记**：出站的尝试 / 结局、入站的帧、解码后的消息、附件的头。
/// 3. **智能机器人的 secret 永不进字段**：`aibot_subscribe` 的 body 提着它，而
///    [`trace_out_fields`] 只读具名字段。
/// 4. **绑定 token 永不进字段**（同一段的另一条断言）。
/// 5. **缺席的字段读作哨兵**，而不是"与一个空值无法区分"。
/// 6. 🔴 **上游"只读 `markdown.content`"那一格照抄**：一条流帧的正文**不**被记录。
#[test]
#[allow(clippy::too_many_lines)] // 上游那六条护栏合成一个用例（见模块文档的串行理由），不再拆
fn the_switch_governs_every_point_and_never_records_a_credential() {
    let _guard = switch_guard();
    let was_on = tracing_on();
    set_trace(false);

    // ---- ① 关着：五个点全是 None ----
    let callback = inbound_callback();
    assert!(trace_out_fields(&send_msg_frame("CHAT-1", "hello")).is_none());
    assert!(inbound_fields(&callback, "hello").is_none());
    assert!(in_fields(&FrameEnvelope::default()).is_none());
    assert!(media_headers_fields("MSGID", 0, &named_headers(), None).is_none());

    // ---- ② 开着：每一个点都给出它该给的字段 ----
    set_trace(true);

    let subscribe = trace_out_fields(&subscribe_frame("THE_SMART_BOT_SECRET")).expect("出站字段");
    assert_eq!(subscribe.cmd(), "aibot_subscribe");
    assert_eq!(subscribe.req_id(), "req-sub-1");
    let attempt = out_attempt_fields(3, &subscribe);
    assert_eq!(attempt.get("dir"), "out");
    assert_eq!(attempt.get("seq"), "3");
    assert_eq!(attempt.get("cmd"), "aibot_subscribe");
    assert_eq!(attempt.get("req_id"), "req-sub-1");
    // ---- ③ 凭据面：整个字段集渲染出来**不含** secret，连字段名都没有 ----
    let rendered = attempt.render();
    assert!(!rendered.contains("THE_SMART_BOT_SECRET"), "{rendered}");
    assert!(!rendered.contains("secret"), "{rendered}");

    // ---- ④ 绑定 token：出站正文的预览里藏着它，而它被脱敏掉 ----
    let prompt = binding_prompt(PRODUCTION_SHAPED_TOKEN);
    let binding = trace_out_fields(&send_msg_frame("CHAT-1", &prompt)).expect("出站字段");
    let binding_fields = out_attempt_fields(4, &binding);
    assert_eq!(binding_fields.get("chatid"), "CHAT-1");
    assert_eq!(binding_fields.get("chat_type"), "2");
    assert_eq!(binding_fields.get("msgtype"), "markdown");
    assert_eq!(
        binding_fields.get("len"),
        prompt.chars().count().to_string()
    );
    let text = binding_fields.get("text");
    assert!(!text.contains(PRODUCTION_SHAPED_TOKEN), "{text}");
    assert!(text.contains("token=[redacted]"), "{text}");

    // ---- ⑤ 缺席的字段是哨兵 ----
    assert_eq!(
        out_attempt_fields(9, &subscribe).get("chatid"),
        TRACE_ABSENT
    );
    assert_eq!(
        out_attempt_fields(9, &subscribe).get("msgtype"),
        TRACE_ABSENT
    );

    // ---- 尝试与结局背着同一个 seq，各自的内容对 ----
    let ok = out_result_fields(4, &binding, TRACE_STAGE_WRITE, None);
    assert_eq!(ok.get("dir"), "out.done");
    assert_eq!(ok.get("seq"), "4");
    assert_eq!(ok.get("ok"), "true");
    assert!(!ok.contains("stage"), "成功不许有 stage");
    assert!(!ok.contains("error"), "成功不许有 error");

    let failed = out_result_fields(
        4,
        &binding,
        TRACE_STAGE_WRITE,
        Some("socket said no\nreally"),
    );
    assert_eq!(failed.get("ok"), "false");
    assert_eq!(failed.get("stage"), "write_message");
    assert_eq!(failed.get("error"), "socket said no really");
    assert_eq!(TRACE_STAGE_DEADLINE, "set_write_deadline");

    // ---- 入站：帧的判决与解码后的消息 ----
    let outbound_ack: FrameEnvelope = serde_json::from_value(json!({
        "headers": { "req_id": "req-1" },
        "errcode": 45009,
        "errmsg": "api freq out of limit",
    }))
    .expect("envelope fixture");
    let ack = in_fields(&outbound_ack).expect("入站字段");
    assert_eq!(ack.get("dir"), "in");
    // 上游 `traceIn` **永远**写这四个 attr（哪怕是空串）⇒ 没 cmd 的帧上是空值，不是哨兵。
    assert_eq!(ack.get("cmd"), "");
    assert!(ack.contains("cmd"));
    assert_eq!(ack.get("req_id"), "req-1");
    assert_eq!(ack.get("errcode"), "45009");
    assert_eq!(ack.get("errmsg"), "api freq out of limit");

    let greeting = "hello from the room";
    let decoded = inbound_fields(&callback, greeting).expect("消息字段");
    assert_eq!(decoded.get("dir"), "in.msg");
    assert_eq!(decoded.get("msg_id"), "MSG_1");
    assert_eq!(decoded.get("chatid"), "GROUP_CHAT_ID");
    assert_eq!(decoded.get("chat_type"), "group");
    assert_eq!(decoded.get("sender"), "SENDER_USERID");
    assert_eq!(decoded.get("msgtype"), "text");
    assert_eq!(decoded.get("len"), greeting.chars().count().to_string());
    assert_eq!(decoded.get("text"), greeting);

    // ---- 附件的头：连缺席的头也**发**一个空值（好分清"服务端没发"与"开关关着"）----
    let empty = media_headers_fields("MSGID", 1, &MediaHeaders::default(), None).expect("头字段");
    assert_eq!(empty.get("dir"), "in.media");
    assert_eq!(empty.get("msg_id"), "MSGID");
    assert_eq!(empty.get("index"), "1");
    assert_eq!(empty.get("content_disposition"), "");
    assert_eq!(empty.get("filename"), "");
    assert_eq!(
        empty.get("installation_id"),
        TRACE_ABSENT,
        "没有安装 id 时是哨兵，不是空串"
    );

    // ---- ⑥ 上游那一格：`markdown.content` 记、`stream.content` 不记 ----
    let stream = trace_out_fields(&json!({
        "cmd": "aibot_respond_msg",
        "headers": { "req_id": "req-stream-1" },
        "body": {
            "msgtype": "stream",
            "stream": { "id": "s-1", "finish": false, "content": "气泡正文" },
        },
    }))
    .expect("出站字段");
    let stream_fields = out_attempt_fields(5, &stream);
    assert_eq!(stream_fields.get("msgtype"), "stream");
    assert!(
        !stream_fields.contains("len") && !stream_fields.contains("text"),
        "上游只读 markdown.content ⇒ 流帧正文不进字段（照抄，见 docs/32 §38 的 R2）：{}",
        stream_fields.render()
    );

    // ---- 复原：一份"记着"的进程不许泄漏给别的用例 ----
    set_trace(was_on);
    assert_eq!(tracing_on(), was_on);
}
