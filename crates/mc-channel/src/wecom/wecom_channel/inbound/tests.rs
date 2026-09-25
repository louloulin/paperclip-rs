//! `inbound.rs` 的用例（上游 `ws_frame_test.go` / `inbox_message_test.go` 里与归一化有关的那几组）。
//!
//! 上游这一段的用例分散在 `ws_frame_test.go`（`ownText` / `ownCommandSource` / `quotedContext` /
//! `stripLeadingMentions` / `channelMsgType` / `channelMessageFromCallback`）；
//! 本仓把它们收在一个子模块里，因为**本文件是那一段的落点**。

use mc_core::channel::message::{ChatType, MessageKind};
use serde_json::json;

use super::{
    channel_message_from_callback, channel_msg_type, is_issue_command, media_for,
    media_placeholder, normalize_wecom_control_layout, strip_leading_mentions, MediaKind,
    MAX_QUOTED_RUNES, UNSUPPORTED_MSG_TYPE_RECEIPT,
};
use crate::wecom::ws_frame::AibotMsgCallback;

/// 从一段 JSON 造一条回调（wire 形态与上游逐字一致）。
fn callback(value: serde_json::Value) -> AibotMsgCallback {
    serde_json::from_value(value).expect("callback")
}

fn text_callback(msg_type: &str, content: &str) -> AibotMsgCallback {
    callback(json!({
        "msgid": "m1",
        "aibotid": "bot_1",
        "chatid": "chat_1",
        "chattype": "single",
        "from": { "userid": "u1" },
        "msgtype": msg_type,
        "text": { "content": content },
    }))
}

// =====================================================================
// own_text
// =====================================================================

/// 上游 `TestOwnText` 的六种形态。
#[test]
fn own_text_covers_every_readable_kind() {
    // 纯文本：正文本身。
    let (text, ok) = text_callback("text", "你好").own_text();
    assert_eq!((text.as_str(), ok), ("你好", true));

    // 语音：`WeCom` 的转写，**不下载**。
    let mut voice = text_callback("voice", "");
    voice.voice.content = "  说出来的话  ".to_string();
    let (text, ok) = voice.own_text();
    assert_eq!((text.as_str(), ok), ("说出来的话", true));

    // 空转写 ⇒ 读不出东西（识别在背景噪音或半秒按键上会回来是空的）。
    let empty_voice = text_callback("voice", "");
    let (text, ok) = empty_voice.own_text();
    assert_eq!((text.as_str(), ok), ("", false));

    // 图片 / 文件 / 视频：占位符，条件是**有 url**。
    let mut image = text_callback("image", "");
    image.image.url = "https://cos.example/x".to_string();
    image.image.aeskey = "k".to_string();
    assert_eq!(image.own_text(), ("[Image]".to_string(), true));
    let mut file = text_callback("file", "");
    file.file.url = "https://cos.example/f".to_string();
    assert_eq!(file.own_text(), ("[File]".to_string(), true));
    let mut video = text_callback("video", "");
    video.video.url = "https://cos.example/v".to_string();
    assert_eq!(video.own_text(), ("[Video]".to_string(), true));

    // 没有 url 的图片回调：**读不出来**（拿走占位符路径，回执路径接手）。
    let no_url = text_callback("image", "");
    assert_eq!(no_url.own_text(), (String::new(), false));

    // 图文混排：各段按**用户编排的顺序**，附件以占位符站在那里。
    let mixed = callback(json!({
        "msgid": "m1", "aibotid": "bot_1", "chatid": "c", "chattype": "single",
        "from": {"userid": "u1"}, "msgtype": "mixed",
        "mixed": { "msg_item": [
            {"msgtype": "text", "text": {"content": "看这个"}},
            {"msgtype": "image", "image": {"url": "https://x/1", "aeskey": "k"}},
            {"msgtype": "voice", "voice": {"content": "顺便说一句"}},
        ]},
    }));
    assert_eq!(
        mixed.own_text(),
        ("看这个\n[Image]\n顺便说一句".to_string(), true)
    );

    // 一个空段贡献**空串**而不是占位符；全空 ⇒ 读不出来。
    let empty_mixed = callback(json!({
        "msgid": "m1", "msgtype": "mixed",
        "mixed": { "msg_item": [ {"msgtype": "image", "image": {}}, {"msgtype": "location"} ] },
    }));
    assert_eq!(empty_mixed.own_text(), (String::new(), false));

    // 认不出的一种（一张位置卡）。
    assert_eq!(
        text_callback("location", "x").own_text(),
        (String::new(), false)
    );
}

// =====================================================================
// own_command_source
// =====================================================================

/// 上游 `TestOwnCommandSource`：它**不是** `own_text` —— 占位符被排除，而且只有发件人的字。
#[test]
fn own_command_source_drops_the_placeholders() {
    // 纯文本 / 语音：与 own_text 相同（去空白）。
    assert_eq!(
        text_callback("text", "/issue 登录坏了").own_command_source(),
        "/issue 登录坏了"
    );
    let mut voice = text_callback("voice", "");
    voice.voice.content = " /issue 登录坏了 ".to_string();
    assert_eq!(voice.own_command_source(), "/issue 登录坏了");

    // 一张**单独**的照片：整条正文就是占位符 ⇒ 空串（"占位符不是发件人打的字"）。
    let mut image = text_callback("image", "");
    image.image.url = "https://x/1".to_string();
    assert_eq!(image.own_text(), ("[Image]".to_string(), true));
    assert_eq!(image.own_command_source(), "");

    // 图文混排：只留发件人写的 / 说的那几段（附件不贡献）。
    let mixed = callback(json!({
        "msgid": "m1", "msgtype": "mixed",
        "mixed": { "msg_item": [
            {"msgtype": "image", "image": {"url": "https://x/1"}},
            {"msgtype": "text", "text": {"content": "/issue 登录坏了"}},
        ]},
    }));
    assert_eq!(
        mixed.own_text(),
        ("[Image]\n/issue 登录坏了".to_string(), true)
    );
    assert_eq!(mixed.own_command_source(), "/issue 登录坏了");
}

// =====================================================================
// quoted_context
// =====================================================================

/// 上游 `TestQuotedContext`：引用渲染成引用块、首行带标签、超长按 **rune** 截断。
#[test]
fn quoted_context_renders_a_blockquote() {
    let mut quoted = text_callback("text", "这个怎么处理");
    quoted.quote.msgtype = "text".to_string();
    quoted.quote.text.content = "告警：磁盘 95%".to_string();
    assert_eq!(quoted.quoted_context(), "> [Quote] 告警：磁盘 95%");

    // 多行：只有**第一**行带标签，其余挂在同一个引用块里。
    let mut quoted = text_callback("text", "x");
    quoted.quote.msgtype = "text".to_string();
    quoted.quote.text.content = "第一行\n第二行".to_string();
    assert_eq!(quoted.quoted_context(), "> [Quote] 第一行\n> 第二行");

    // 没有引用 ⇒ 空串。
    assert_eq!(text_callback("text", "x").quoted_context(), "");

    // 超长：按 rune 截到上限 + 省略号，且**不**留下一串空白。
    let mut quoted = text_callback("text", "x");
    quoted.quote.msgtype = "text".to_string();
    quoted.quote.text.content = "字".repeat(MAX_QUOTED_RUNES + 50);
    let rendered = quoted.quoted_context();
    let runes = rendered.chars().count();
    assert!(
        runes <= MAX_QUOTED_RUNES + QUOTE_PREFIX_RUNES_AND_MARKER,
        "{runes}"
    );
    assert!(rendered.ends_with('…'), "{rendered}");

    // 引用一块**媒体**：渲染成它的占位符。
    let mut quoted = text_callback("text", "x");
    quoted.quote.msgtype = "image".to_string();
    quoted.quote.image.url = "https://x/1".to_string();
    assert_eq!(quoted.quoted_context(), "> [Quote] [Image]");

    // 引用一段**图文混排**：比普通的一段多嵌一层。
    let mut quoted = text_callback("text", "x");
    quoted.quote.msgtype = "mixed".to_string();
    quoted.quote.mixed.msg_item = vec![
        serde_json::from_value(json!({"msgtype": "text", "text": {"content": "看这个"}}))
            .expect("item"),
        serde_json::from_value(json!({"msgtype": "file", "file": {"url": "https://x/f"}}))
            .expect("item"),
    ];
    assert_eq!(quoted.quoted_context(), "> [Quote] 看这个\n> [File]");
}

/// `[Quote] ` 加省略号的余量（`MAX_QUOTED_RUNES` 是渲染**之前**的上限，加前缀不影响它）。
const QUOTE_PREFIX_RUNES_AND_MARKER: usize = "[Quote] > ".len() + 1;

// =====================================================================
// strip_leading_mentions
// =====================================================================

/// 上游 `TestStripLeadingMentions`：剥开头、只剥开头、自己名字整名匹配、单聊不剥。
#[test]
fn mentions_are_stripped_from_the_front_only() {
    assert_eq!(strip_leading_mentions("@Andrew /clear", ""), "/clear");
    assert_eq!(
        strip_leading_mentions("  @Andrew   /clear  ", ""),
        "/clear  "
    );
    // 名字含空格：整名匹配，否则 "Multica Bot" 会被切成 "Bot /clear …"。
    assert_eq!(
        strip_leading_mentions("@Multica Bot /clear 重新分析", "Multica Bot"),
        "/clear 重新分析"
    );
    // 没有名字可匹配时按"到下一个空格"的启发式。
    assert_eq!(
        strip_leading_mentions("@Andrew /clear", "Multica Bot"),
        "/clear"
    );
    // 连着一个又一个提及。
    assert_eq!(strip_leading_mentions("@a @b /clear", ""), "/clear");
    // 句子更深处的一个名字是"在说别人" ⇒ 不动。
    assert_eq!(
        strip_leading_mentions("@Andrew 问一下 @李雷 昨天那件事", ""),
        "问一下 @李雷 昨天那件事"
    );
    // 整条消息就是一个提及：没有命令也没有话 ⇒ 原样留着（空正文由调用方决定）。
    assert_eq!(strip_leading_mentions("@Andrew", ""), "@Andrew");
    // 单聊里那个 `@` 是发件人在谈论某个同事 —— 调用方按 chat type 把关，这里只证明函数本身
    // 对不以 `@` 开头的文本是恒等的。
    assert_eq!(strip_leading_mentions("你好 @李雷", ""), "你好 @李雷");
}

// =====================================================================
// channel_msg_type / media_for / media_placeholder
// =====================================================================

/// 上游 `TestChannelMsgType`。
#[test]
fn msg_type_mapping() {
    assert_eq!(channel_msg_type("text"), MessageKind::Text);
    assert_eq!(channel_msg_type("TEXT"), MessageKind::Text);
    assert_eq!(channel_msg_type("image"), MessageKind::Image);
    assert_eq!(channel_msg_type("file"), MessageKind::File);
    assert_eq!(channel_msg_type("voice"), MessageKind::Audio);
    assert_eq!(channel_msg_type("audio"), MessageKind::Audio);
    assert_eq!(channel_msg_type("video"), MessageKind::Video);
    // 图文混排**就是**文本（跑完 `own_text` 之后），而它的附件另行走 `MediaRef`。
    assert_eq!(channel_msg_type("mixed"), MessageKind::Text);
    assert_eq!(channel_msg_type("location"), MessageKind::Unknown);
    assert_eq!(channel_msg_type(""), MessageKind::Unknown);
}

/// 三个占位符与 lark / dingtalk **逐字节一致**（agent 用同一个 prompt 读所有渠道）。
#[test]
fn placeholders_match_the_other_channels() {
    assert_eq!(media_placeholder(MessageKind::Image), "[Image]");
    assert_eq!(media_placeholder(MessageKind::File), "[File]");
    assert_eq!(media_placeholder(MessageKind::Video), "[Video]");
    // 别的种类（含 `Text` / `Audio` / `Unknown`）回落到 `[File]` —— 它们本来就不该经过这里。
    assert_eq!(media_placeholder(MessageKind::Text), "[File]");
}

/// `media_for` 只认三种可下载类型，且大小写不敏感。
#[test]
fn media_for_matches_the_three_downloadable_kinds() {
    let body = crate::wecom::ws_frame::MediaBody {
        url: "https://x/1".to_string(),
        aeskey: "k".to_string(),
    };
    let empty = crate::wecom::ws_frame::MediaBody::default();
    let (got, kind) = media_for("IMAGE", &body, &empty, &empty).expect("image");
    assert_eq!((got.url.as_str(), kind), ("https://x/1", MediaKind::Image));
    assert_eq!(MediaKind::Image.message_kind(), MessageKind::Image);
    assert_eq!(MediaKind::File.message_kind(), MessageKind::File);
    assert_eq!(MediaKind::Video.message_kind(), MessageKind::Video);
    assert!(media_for("voice", &empty, &empty, &empty).is_none());
    assert!(media_for("text", &empty, &empty, &empty).is_none());
}

/// `InboundMedia` 的 `Debug` 不回显预签名地址与单次密钥（凭据纪律）。
#[test]
fn inbound_media_debug_redacts() {
    let media = super::InboundMedia {
        kind: MediaKind::Image,
        url: "https://cos.example/PRESIGNED-URL".to_string(),
        aeskey: "AESKEY-SECRET".to_string(),
    };
    let rendered = format!("{media:?}");
    assert!(!rendered.contains("PRESIGNED"), "{rendered}");
    assert!(!rendered.contains("AESKEY-SECRET"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    let empty = super::InboundMedia {
        kind: MediaKind::File,
        url: String::new(),
        aeskey: String::new(),
    };
    assert!(format!("{empty:?}").contains("<empty>"));
}

// =====================================================================
// channel_message_from_callback
// =====================================================================

/// 单聊的路由身份与四张字段表。
#[test]
fn a_p2p_text_callback_becomes_a_p2p_envelope() {
    let mut cb = text_callback("text", "你好");
    cb.chatid = "chat_1".to_string();
    let (text, ok) = cb.own_text();
    assert!(ok);
    let message = channel_message_from_callback("bot_1", "Multica Bot", &cb, &text, "req-1");

    assert_eq!(message.event_id, "m1");
    assert_eq!(message.message_id, "m1");
    assert_eq!(message.kind, MessageKind::Text);
    assert_eq!(message.text, "你好");
    assert_eq!(message.command_text, "你好");
    assert_eq!(message.source.chat_id, "chat_1");
    assert_eq!(message.source.chat_type, ChatType::P2p);
    assert_eq!(message.source.sender_id, "u1");
    assert_eq!(message.source.sender_stable_id, "");
    assert_eq!(message.source.thread_id, "");
    assert!(message.addressed_to_bot);
    assert!(!message.force_fresh);
    assert!(!message.has_selected_context);
    assert!(!message.skip_agent_run);
    // 形态纪律：adapter **不得**预填 `media_refs`。
    assert!(message.media_refs.is_empty());
    assert!(message.reply_to.is_none());

    // 单聊里 `chatid` 缺席 ⇒ 退到发件人（有些形态只给群聊设它）。
    let mut cb = text_callback("text", "你好");
    cb.chatid = String::new();
    let message = channel_message_from_callback("bot_1", "", &cb, "你好", "req-1");
    assert_eq!(message.source.chat_id, "u1");
    assert_eq!(message.source.chat_type, ChatType::P2p);
}

/// 群聊：路由身份换一组，`@` 从**指令源**上剥掉而正文保留。
#[test]
fn a_group_callback_strips_the_addressing_from_the_command_source() {
    let mut cb = text_callback("text", "@Multica Bot /clear 重新分析");
    cb.chattype = "group".to_string();
    cb.chatid = "group_9".to_string();
    let (text, _ok) = cb.own_text();
    let message = channel_message_from_callback("bot_1", "Multica Bot", &cb, &text, "req-2");
    assert_eq!(message.source.chat_type, ChatType::Group);
    assert_eq!(message.source.chat_id, "group_9");
    assert_eq!(message.source.sender_id, "u1");
    // 指令是 `/clear 重新分析` ⇒ 控制布局重排把指令从**可见正文**里摘掉，只留正文。
    assert_eq!(message.text, "重新分析");
    // 而**指令源**原样带着那条指令：`/clear` 的语义差别由 Router 独自施加（上游逐字）。
    assert_eq!(message.command_text, "/clear 重新分析");
    assert!(message.force_fresh);
    assert!(!message.skip_agent_run);
}

/// 单聊里那个开头的 `@` **不**被剥掉（那是在说某个同事），所以 `/issue` 不会凭空建单。
#[test]
fn a_p2p_leading_mention_is_not_addressing() {
    let mut cb = text_callback("text", "@李雷 /issue 帮我问问他");
    cb.chattype = "single".to_string();
    let (text, _ok) = cb.own_text();
    let message = channel_message_from_callback("bot_1", "Multica Bot", &cb, &text, "req-3");
    // 单聊不摘 `@`，所以第一行不是 `/issue` ⇒ 没有 issue、也没有"跳过 agent"。
    assert_eq!(message.command_text, "@李雷 /issue 帮我问问他");
    assert!(!message.skip_agent_run);
    assert_eq!(message.text, "@李雷 /issue 帮我问问他");
}

/// 一条**纯** `/issue` 在 wecom 上不触发 agent（`skip_agent_run`），群聊里也一样。
#[test]
fn a_pure_issue_command_skips_the_agent() {
    let cb = text_callback("text", "/issue 登录坏了\n复现步骤");
    let (text, _ok) = cb.own_text();
    let message = channel_message_from_callback("bot_1", "", &cb, &text, "req-4");
    assert!(
        message.skip_agent_run,
        "wecom 独一份：纯 /issue 不触发 agent"
    );
    assert_eq!(message.command_text, "/issue 登录坏了\n复现步骤");

    let mut group = text_callback("text", "@Bot /issue 登录坏了");
    group.chattype = "group".to_string();
    let (text, _why) = group.own_text();
    let message = channel_message_from_callback("bot_1", "Bot", &group, &text, "req-5");
    // 读的是**同一个**源：群里的 /issue 与单聊的一致（既不重复问 agent，也不漏建）。
    assert_eq!(message.command_text, "/issue 登录坏了");
    assert!(message.skip_agent_run);
    // 正文保留发件人**编排的**那一份：`/issue` **不是**控制指令（只有 `/clear` / `/new` 是），
    // 所以没有重排，`@Bot` 与那条指令都还在。
    assert_eq!(message.text, "@Bot /issue 登录坏了");
    assert!(!message.force_fresh);

    // U+3000（中文全角输入法的表意空格）开头的 `/issue`：委托 engine 的解析器 ⇒ 两处**同判**。
    //
    // 上游在这里踩过一次坑：adapter 自己那份镜像用 `strings.TrimSpace`（会裁掉 U+3000），于是
    // 它判"是命令"而 engine 判"是散文" —— `SkipAgentRun` 被置上所以没有 agent 跑，而解析器拒了
    // 所以没有 issue 被建，发件人什么都没收到。本仓**委托**出去 ⇒ 不连这一步都不可能走岔。
    let wide = text_callback("text", "\u{3000}/issue 登录坏了");
    let (text, _why) = wide.own_text();
    let message = channel_message_from_callback("bot_1", "", &wide, &text, "req-6");
    assert_eq!(
        message.skip_agent_run,
        is_issue_command(&message.command_text),
        "两处必须同判（这一条就是那个不变式本身）"
    );
    assert!(!message.skip_agent_run, "U+3000 开头的行两处都读成散文");

    assert!(is_issue_command("/issue x"));
    assert!(!is_issue_command("说 /issue x"));
    assert!(!is_issue_command("\u{3000}/issue x"));
}

/// 引用：进正文（引用块在前）、置 `has_selected_context`、**不进**指令源。
#[test]
fn a_quote_is_context_and_never_a_command() {
    let mut cb = text_callback("text", "这个怎么处理");
    cb.quote.msgtype = "text".to_string();
    cb.quote.text.content = "告警：磁盘 95%".to_string();
    let (text, _ok) = cb.own_text();
    let message = channel_message_from_callback("bot_1", "", &cb, &text, "req-7");
    assert_eq!(message.text, "> [Quote] 告警：磁盘 95%\n\n这个怎么处理");
    assert_eq!(message.command_text, "这个怎么处理");
    assert!(message.has_selected_context);
    assert!(!message.skip_agent_run);

    // 引用一段 `/issue …`：它**不是**发件人在这里打的，所以一句 issue 都不该被建。
    let mut cb = text_callback("text", "这是怎么回事");
    cb.quote.msgtype = "text".to_string();
    cb.quote.text.content = "/issue 别人写的标题".to_string();
    let (text, _ok) = cb.own_text();
    let message = channel_message_from_callback("bot_1", "", &cb, &text, "req-8");
    assert!(!message.skip_agent_run, "引用的 /issue 不许建单");
    assert_eq!(message.command_text, "这是怎么回事");

    // 一张**单独**的截图 + 一条引用：指令源交回富化**之前**的正文（#8058 的形态）。
    let mut cb = text_callback("image", "");
    cb.image.url = "https://x/1".to_string();
    cb.quote.msgtype = "text".to_string();
    cb.quote.text.content = "被引用的东西".to_string();
    let (text, has_body) = cb.own_text();
    assert!(has_body);
    let message = channel_message_from_callback("bot_1", "", &cb, &text, "req-9");
    assert_eq!(message.text, "> [Quote] 被引用的东西\n\n[Image]");
    // 占位符是 adapter 写的，而引用是发件人选中的 —— 所以指令源拿到的是**占位符那一份**的
    // 富化前正文（这里就是 "[Image]"），不是引用。
    assert_eq!(message.command_text, "[Image]");
    assert!(message.has_selected_context);
}

/// 图文混排：正文按序、指令源只看发件人写的段（于是"先截图再 /issue"也能建单）。
#[test]
fn a_mixed_message_keeps_the_command_on_the_first_authored_run() {
    let cb = callback(json!({
        "msgid": "m1", "aibotid": "bot_1", "chatid": "c", "chattype": "single",
        "from": {"userid": "u1"}, "msgtype": "mixed",
        "mixed": { "msg_item": [
            {"msgtype": "image", "image": {"url": "https://x/1", "aeskey": "k"}},
            {"msgtype": "text", "text": {"content": "/issue 登录坏了"}},
        ]},
    }));
    let (text, _ok) = cb.own_text();
    let message = channel_message_from_callback("bot_1", "", &cb, &text, "req-10");
    assert_eq!(message.text, "[Image]\n/issue 登录坏了");
    assert_eq!(message.command_text, "/issue 登录坏了");
    assert!(message.skip_agent_run);
    assert_eq!(message.kind, MessageKind::Text);

    // 附件按序进 `raw.media`，而**不进** `media_refs`（那是 engine 的输出通道）。
    let raw: super::WeComInboundMessage = serde_json::from_value(message.raw.clone()).expect("raw");
    assert_eq!(raw.media.len(), 1);
    assert_eq!(raw.media[0].kind, MediaKind::Image);
    assert_eq!(raw.bot_id, "bot_1");
    assert_eq!(raw.req_id, "req-10");
    assert_eq!(raw.msg_type, "mixed");
    assert_eq!(raw.chat_id, "c");
    assert_eq!(raw.sender_user_id, "u1");
    assert!(message.media_refs.is_empty());
}

/// 读不懂的一种：正文空、`kind` 未知 —— 读循环据此走回执路径。
#[test]
fn an_unreadable_kind_carries_no_text() {
    let cb = text_callback("location", "");
    let (text, ok) = cb.own_text();
    assert!(!ok);
    let message = channel_message_from_callback("bot_1", "", &cb, &text, "req-11");
    assert_eq!(message.kind, MessageKind::Unknown);
    assert_eq!(message.text, "");
    assert!(!message.skip_agent_run);
    // 回执那句话是**常量**，不是从消息里拼出来的（凭据 / 内容纪律）。
    assert!(!UNSUPPORTED_MSG_TYPE_RECEIPT.is_empty());
}

// =====================================================================
// 控制指令的正文重排
// =====================================================================

/// 上游 `TestNormalizeWeComControlLayout`：裸指令是共享的 pending 哨兵，带着内容来的才是真回合。
#[test]
fn a_bare_directive_is_left_for_router() {
    let cb = text_callback("text", "/clear");
    let (visible, control) =
        normalize_wecom_control_layout(&cb, "/clear", "/clear", ChatType::P2p, "", false);
    assert_eq!(visible, "/clear");
    assert!(control.is_none(), "什么都没带 ⇒ 交给 Router 认那条哨兵");
}

/// 带正文的指令：可见正文变成指令**之后**的那一段。
#[test]
fn a_directive_with_a_body_is_consumed() {
    let cb = text_callback("text", "/clear 重新分析");
    let (visible, control) = normalize_wecom_control_layout(
        &cb,
        "/clear 重新分析",
        "/clear 重新分析",
        ChatType::P2p,
        "",
        false,
    );
    assert_eq!(visible, "重新分析");
    assert_eq!(
        control.map(|control| control.kind),
        Some(crate::engine::commands::ControlCommandKind::FreshSession)
    );
}

/// 带着媒体来的**裸**指令：仍然被消费（可见正文变空），而 `force_fresh` 驮着它。
#[test]
fn a_bare_directive_beside_content_is_consumed() {
    let mut cb = text_callback("image", "");
    cb.image.url = "https://x/1".to_string();
    let (visible, control) =
        normalize_wecom_control_layout(&cb, "/clear", "/clear", ChatType::P2p, "", true);
    assert_eq!(
        visible, "/clear",
        "图片这一支不重排正文（不是 text / voice / mixed）"
    );
    assert!(control.is_none());

    // 而**文本 + 媒体**的裸指令走 text 那一支：正文被摘掉、指令被交回来。
    let mut cb = text_callback("text", "/clear");
    cb.image.url = "https://x/1".to_string();
    let (visible, control) =
        normalize_wecom_control_layout(&cb, "/clear", "/clear", ChatType::P2p, "", true);
    assert_eq!(visible, "");
    assert!(control.is_some());
}

/// 群里：重排用的是**剥掉提及之后**的那一份，于是机器人提及不会被存成 prompt 文本。
#[test]
fn the_group_rewrite_uses_the_addressing_cleaned_body() {
    let mut cb = text_callback("text", "@Multica Bot /clear 重新分析");
    cb.chattype = "group".to_string();
    let (visible, control) = normalize_wecom_control_layout(
        &cb,
        "@Multica Bot /clear 重新分析",
        "/clear 重新分析",
        ChatType::Group,
        "Multica Bot",
        false,
    );
    assert_eq!(visible, "重新分析");
    assert!(control.is_some());
}

/// 图文混排里指令落在某一段上：那一段被换成指令之后的正文，其余各段按序保留。
#[test]
fn the_mixed_rewrite_consumes_the_directive_in_place() {
    let cb = callback(json!({
        "msgid": "m1", "msgtype": "mixed", "chattype": "single",
        "mixed": { "msg_item": [
            {"msgtype": "image", "image": {"url": "https://x/1"}},
            {"msgtype": "text", "text": {"content": "/new 换个话题"}},
        ]},
    }));
    let visible = "[Image]\n/new 换个话题";
    let (got, control) =
        normalize_wecom_control_layout(&cb, visible, "/new 换个话题", ChatType::P2p, "", false);
    assert_eq!(got, "[Image]\n换个话题");
    assert_eq!(
        control.map(|control| control.kind),
        Some(crate::engine::commands::ControlCommandKind::NewChat)
    );
}

/// 指令种类**不**一致时不重排（`/clear` 的指令源配一段 `/new` 的正文 ⇒ 不动）。
#[test]
fn a_mismatched_directive_leaves_the_body_alone() {
    let cb = text_callback("text", "/new 换个话题");
    let visible = "/new 换个话题";
    let (got, control) =
        normalize_wecom_control_layout(&cb, visible, "/clear 重新分析", ChatType::P2p, "", false);
    assert_eq!(got, visible);
    assert!(control.is_none());
}
