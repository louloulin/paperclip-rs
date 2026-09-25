//! [`super`]（摊平 / 提及 / markdown 探测）的用例 —— 上游
//! `{content_flatten_test.go, mention_test.go, markdown_detect_test.go}` 的等价集合，
//! 加上 M7-11 的交接项（`ws_frame_decoder_test.go` 里那批 `resolveMentions` 用例，
//! 见 `docs/32` §28 的 H1：那批 helper 归本片）。
//!
//! 五组：摊平（含 `post` 二维结构）/ 提及改写（含前缀撞车与空白保真）/
//! `contains_mention` 的 `union_id` 优先规则 / 出站提及的两个 wire 形态 / markdown 探测。

use super::*;
use crate::lark::ws_frame_decoder::LarkSenderId;

// =====================================================================
// 一、摊平：按 msg_type 分派
// =====================================================================

/// 上游 `TestFlattenContent_DispatchByType` 的逐行等价。
#[test]
fn flatten_content_dispatches_by_message_type() {
    let cases = [
        ("text", r#"{"text":"hello"}"#, "hello"),
        ("image", r#"{"image_key":"img_x"}"#, "[Image]"),
        ("file", r#"{"file_key":"f"}"#, "[File]"),
        ("audio", r#"{"file_key":"f"}"#, "[Audio]"),
        ("media", r#"{"file_key":"f"}"#, "[Video]"),
        ("video", r#"{"file_key":"f"}"#, "[Video]"),
        ("sticker", r#"{"file_key":"f"}"#, "[Sticker]"),
        ("interactive", r#"{"title":"t"}"#, "[interactive card]"),
        ("share_chat", r#"{"chat_id":"oc"}"#, "[Shared Chat]"),
        ("share_user", r#"{"user_id":"ou"}"#, "[Shared User Card]"),
        ("system", "{}", "[System Message]"),
        (
            "merge_forward",
            r#"{"content":"Merged and Forwarded Message"}"#,
            "[forwarded messages]",
        ),
        ("totally_new_type", "{}", ""),
    ];
    for (msg_type, content, want) in cases {
        assert_eq!(
            flatten_content(msg_type, content),
            want,
            "flatten_content({msg_type})"
        );
    }
}

/// `text` 正文解不开 / 为空 ⇒ 空串（不是错误）。
#[test]
fn text_body_degrades_to_empty_string() {
    assert_eq!(extract_text_body(""), "");
    assert_eq!(extract_text_body("not json"), "");
    assert_eq!(extract_text_body(r#"{"other":"x"}"#), "");
    assert_eq!(extract_text_body(r#"{"text":""}"#), "");
}

/// `msg_type` → [`MessageKind`]：文本四型全摊成 `Text`，媒体一一对应。
#[test]
fn message_kind_maps_text_ish_types_to_text() {
    for msg_type in ["", "text", "post", MSG_TYPE_MERGE_FORWARD, "interactive"] {
        assert_eq!(message_kind(msg_type), MessageKind::Text, "{msg_type}");
    }
    assert_eq!(message_kind("image"), MessageKind::Image);
    assert_eq!(message_kind("file"), MessageKind::File);
    assert_eq!(message_kind("audio"), MessageKind::Audio);
    assert_eq!(message_kind("media"), MessageKind::Video);
    assert_eq!(message_kind("video"), MessageKind::Video);
    assert_eq!(message_kind("sticker"), MessageKind::Unknown);
}

// =====================================================================
// 二、富文本 `post`
// =====================================================================

/// 上游 `TestFlattenPostContent_IssueExample`：标题行 + 正文段 + 「文本 + 超链接」段。
/// 链接必须渲染成 `text (href)`，URL 才能活到 agent 的上下文里（上游 MUL-2951 的原例）。
#[test]
fn post_with_title_and_hyperlink_flattens_verbatim() {
    let raw = r#"{
        "title": "周报",
        "content": [
            [{ "tag": "text", "text": "本周完成：" }],
            [
                { "tag": "text", "text": "Lark 集成" },
                { "tag": "a", "href": "https://github.com/louloulin/paperclip-rs/pull/3277", "text": "PR #3277" }
            ]
        ]
    }"#;
    assert_eq!(
        flatten_post_content(raw),
        "周报\n本周完成：\nLark 集成 PR #3277 (https://github.com/louloulin/paperclip-rs/pull/3277)"
    );
}

/// 上游 `TestFlattenPostContent_NoTitle`。
#[test]
fn post_without_title_has_no_leading_blank_line() {
    let raw =
        r#"{"content":[[{"tag":"text","text":"line one"}],[{"tag":"text","text":"line two"}]]}"#;
    assert_eq!(flatten_post_content(raw), "line one\nline two");
}

/// 上游 `TestFlattenPostContent_MediaAndMentionSpans`：`at` 带占位、`img` 退化、
/// `emotion` 整个跳过（它的 `emoji_type` 是枚举键，不是显示文本）。
#[test]
fn post_with_at_image_and_emotion_spans() {
    let raw = r#"{"content":[[
        {"tag":"at","user_id":"@_user_1","user_name":""},
        {"tag":"text","text":"look"},
        {"tag":"img","image_key":"img_x"},
        {"tag":"emotion","emoji_type":"SMILE"}
    ] ]}"#;
    assert_eq!(flatten_post_content(raw), "@_user_1 look [Image]");
}

/// 上游 `TestFlattenPostContent_AtPrefersPlaceholderWhenBothPresent` 与
/// `…AtFallsBackToUserNameWhenNoPlaceholder` 两个方向。
#[test]
fn at_span_prefers_placeholder_then_user_name() {
    assert_eq!(
        flatten_post_content(
            r#"{"content":[[{"tag":"at","user_id":"@_user_1","user_name":"ReviewBot"}]]}"#
        ),
        "@_user_1"
    );
    assert_eq!(
        flatten_post_content(
            r#"{"content":[[{"tag":"at","user_id":"","user_name":"ReviewBot"}]]}"#
        ),
        "@ReviewBot"
    );
    // 两者都空 ⇒ 这个 span 不产出任何东西（不留一个孤零零的空格）。
    assert_eq!(
        flatten_post_content(r#"{"content":[[{"tag":"at","user_id":"","user_name":""}]]}"#),
        ""
    );
}

/// 上游 `TestFlattenPostContent_TopicGroupMentionSlashCommand`：话题群里
/// `@bot /issue …` 剥掉提及后**第一行必须以 `/` 开头**（否则 `/issue` 会被富化前缀顶走）。
#[test]
fn post_mention_before_slash_command_survives_stripping() {
    let raw = r#"{"content":[[{"tag":"at","user_id":"@_user_1","user_name":"ReviewBot"},{"tag":"text","text":" /issue review this"}]]}"#;
    let flat = flatten_post_content(raw);
    let mentions = vec![MentionRef {
        key: "@_user_1".to_string(),
        open_id: "ou_bot".to_string(),
        union_id: String::new(),
        name: "ReviewBot".to_string(),
    }];
    let resolved = resolve_mentions(&flat, &mentions, "ou_bot", "");
    assert_eq!(resolved.trim(), "/issue review this");
    assert!(resolved.trim_start().starts_with('/'));
}

/// `post` 的其它 span 形态：`code_block` / `media` / `hr` / 认不出的 tag 取 `text`。
#[test]
fn post_span_variants_render_readably() {
    let raw = r#"{"content":[[
        {"tag":"code_block","text":"fn main() {}"},
        {"tag":"media","file_key":"fk"},
        {"tag":"hr"},
        {"tag":"unknown_tag","text":"kept"},
        {"tag":"unknown_tag"}
    ] ]}"#;
    assert_eq!(flatten_post_content(raw), "fn main() {} [Video] --- kept");
}

/// 上游 `TestFlattenPostContent_Malformed`：坏 JSON / 空串都摊成空串。
#[test]
fn malformed_post_flattens_to_empty() {
    assert_eq!(flatten_post_content("not json"), "");
    assert_eq!(flatten_post_content(""), "");
}

// =====================================================================
// 三、提及改写（上游 `ws_frame_decoder_test.go` 的四个子用例）
// =====================================================================

fn event_mention(key: &str, open_id: &str, union_id: &str, name: &str) -> LarkEventMention {
    LarkEventMention {
        key: key.to_string(),
        id: LarkSenderId {
            open_id: open_id.to_string(),
            union_id: union_id.to_string(),
            user_id: String::new(),
        },
        name: name.to_string(),
    }
}

/// 剥 bot 自己的那一份并吃掉紧邻的一个空格（接缝处不留双空格 / 悬空前导空格）。
#[test]
fn bot_mention_is_stripped_without_doubling_whitespace() {
    let mentions = mentions_from_event(&[event_mention("@_user_1", "ou_bot", "on_bot", "My Bot")]);
    assert_eq!(
        resolve_mentions("@_user_1 summarize this", &mentions, "ou_bot", "on_bot"),
        "summarize this"
    );
    // 中间位置：左侧那个空格留下，右侧那个被吃掉。
    assert_eq!(
        resolve_mentions("hey @_user_1 summarize", &mentions, "ou_bot", "on_bot"),
        "hey summarize"
    );
    // 句尾：回退掉已经写出去的那个尾空格。
    assert_eq!(
        resolve_mentions("summarize this @_user_1", &mentions, "ou_bot", "on_bot"),
        "summarize this"
    );
    // 制表符 / 换行**不**动（只吃一个空格）。
    assert_eq!(
        resolve_mentions("@_user_1\ttabbed", &mentions, "ou_bot", "on_bot"),
        "\ttabbed"
    );
}

/// 上游「avoids `@_user_1` / `@_user_10` prefix collision」：按 key **长度降序**，长占位先赢。
#[test]
fn mention_keys_do_not_prefix_collide() {
    let mentions = mentions_from_event(&[
        event_mention("@_user_1", "ou_bot_wire", "on_bot", "My Bot"),
        event_mention("@_user_10", "ou_alice", "on_alice", "Alice"),
    ]);
    assert_eq!(
        resolve_mentions(
            "@_user_1 forward this to @_user_10 please",
            &mentions,
            "ou_bot",
            "on_bot"
        ),
        "forward this to @Alice please"
    );
}

/// 上游「@-ing both bots in one message strips only self」：兄弟 bot 渲染成 `@名字`。
#[test]
fn only_self_mention_is_stripped_in_a_multi_bot_group() {
    let mentions = mentions_from_event(&[
        event_mention("@_user_1", "ou_self_wire", "on_self_union", "Self Bot"),
        event_mention(
            "@_user_2",
            "ou_sibling_wire",
            "on_sibling_union",
            "Sibling Bot",
        ),
    ]);
    assert_eq!(
        resolve_mentions(
            "@_user_1 @_user_2 please coordinate",
            &mentions,
            "ou_self_canonical",
            "on_self_union"
        ),
        "@Sibling Bot please coordinate"
    );
}

/// 上游「`open_id` match does NOT strip when `union_id` known but differs」：
/// 已知 `union_id` 时 `open_id` 命中**不**代表是自己（多 bot 群的反向映射怪癖）。
#[test]
fn union_id_wins_over_open_id_when_both_are_known() {
    let mentions = mentions_from_event(&[event_mention(
        "@_user_1",
        "ou_self_canonical",
        "on_sibling_union",
        "Sibling Bot",
    )]);
    assert_eq!(
        resolve_mentions(
            "@_user_1 hi",
            &mentions,
            "ou_self_canonical",
            "on_self_union"
        ),
        "@Sibling Bot hi"
    );
    // `union_id` 缺席时**才**回落到 open_id 比较（回填之前的安装）。
    assert_eq!(
        resolve_mentions("@_user_1 hi", &mentions, "ou_self_canonical", ""),
        "hi"
    );
}

/// 名字为空 ⇒ 保留占位（稳定的 token 胜过消失的 @）；两个 bot 标识都空 ⇒ 不剥任何东西。
#[test]
fn unresolved_and_unstrippable_mentions_keep_their_placeholder() {
    let nameless = mentions_from_event(&[event_mention("@_user_7", "ou_x", "", "")]);
    assert_eq!(
        resolve_mentions("hi @_user_7", &nameless, "ou_bot", ""),
        "hi @_user_7"
    );
    let other = mentions_from_event(&[event_mention("@_user_1", "ou_alice", "on_alice", "Alice")]);
    assert_eq!(resolve_mentions("@_user_1 hi", &other, "", ""), "@Alice hi");
}

/// `resolve_mentions` 的空输入短路（正文空 / 提及空都原样返回）。
#[test]
fn resolve_mentions_short_circuits_on_empty_inputs() {
    let mentions = mentions_from_event(&[event_mention("@_user_1", "ou_bot", "", "Bot")]);
    assert_eq!(resolve_mentions("", &mentions, "ou_bot", ""), "");
    assert_eq!(
        resolve_mentions("@_user_1 hi", &[], "ou_bot", ""),
        "@_user_1 hi"
    );
}

/// `contains_mention`：`union_id` 优先、双双为空的失败关闭、REST 形状只有 `open_id`。
#[test]
fn contains_mention_follows_the_union_id_first_rule() {
    let ws = mentions_from_event(&[event_mention("@_user_1", "ou_wire", "on_bot", "Bot")]);
    assert!(contains_mention(&ws, "ou_canonical", "on_bot"));
    // 已知 union_id 但不同 ⇒ 不是自己（哪怕 open_id 撞上）。
    assert!(!contains_mention(&ws, "ou_wire", "on_other"));
    // union_id 未知 ⇒ 回落到 open_id。
    assert!(contains_mention(&ws, "ou_wire", ""));
    // 两个标识都空 ⇒ 失败关闭（**不**匹配每一条）。
    assert!(!contains_mention(&ws, "", ""));
    assert!(!contains_mention(&[], "ou_wire", "on_bot"));
}

/// REST 形状的提及只有裸 `open_id`（`union_id` 空）⇒ 归一后走 `open_id` 分支。
#[test]
fn rest_mentions_only_carry_open_id() {
    let rest = vec![LarkMessageMention {
        key: "@_user_1".to_string(),
        id: "ou_alice".to_string(),
        name: "Alice".to_string(),
    }];
    let mentions = mentions_from_rest(&rest);
    assert_eq!(
        mentions,
        vec![MentionRef {
            key: "@_user_1".to_string(),
            open_id: "ou_alice".to_string(),
            union_id: String::new(),
            name: "Alice".to_string(),
        }]
    );
    // 富上下文装配器传空 bot 标识 ⇒ 所有历史提及都渲染成可读的 @名字。
    assert_eq!(
        resolve_mentions("@_user_1 hi", &mentions, "", ""),
        "@Alice hi"
    );
}

// =====================================================================
// 四、出站提及（上游 `mention_test.go`）
// =====================================================================

/// 上游 `TestPrependMentionWireShapes`：两个 wire 形态逐字分开，**不能**互换。
#[test]
fn outbound_mention_wire_shapes_are_pinned() {
    assert_eq!(
        prepend_text_mention("ou_x", "hi"),
        r#"<at user_id="ou_x"></at> hi"#
    );
    assert_eq!(
        prepend_markdown_mention("ou_x", "hi"),
        "<at id=ou_x></at> hi"
    );
}

/// 上游 `TestPrependMentionDegradesWithoutIdentity`：坏 / 空 id ⇒ 原样返回正文
/// （一条丢提及的回答是小退步，一条提及错人的回答才是要避免的故障；畸形 id 还会破坏
/// 它被插进去的那份卡 JSON）。
#[test]
fn outbound_mention_degrades_without_a_safe_identity() {
    for open_id in [
        "",
        r#"ou_x" onclick=""#,
        "ou_x<br>",
        "ou_x ou_y",
        "ou_x\nou_y",
        r"ou_x\",
        "ou_x\ty",
    ] {
        assert_eq!(
            prepend_text_mention(open_id, "hi"),
            "hi",
            "prepend_text_mention({open_id:?})"
        );
        assert_eq!(
            prepend_markdown_mention(open_id, "hi"),
            "hi",
            "prepend_markdown_mention({open_id:?})"
        );
        assert_eq!(safe_mention_open_id(open_id), None, "safe({open_id:?})");
    }
    assert_eq!(safe_mention_open_id("ou_x"), Some("ou_x"));
}

/// 上游 `TestPrependMentionKeepsBodyIntact`：正文逐字跟在分隔符之后（含前导 markdown）。
#[test]
fn outbound_mention_keeps_the_body_intact() {
    let body = "# heading\n- bullet\n\n```go\nfmt.Println()\n```";
    assert_eq!(
        prepend_markdown_mention("ou_x", body),
        format!("<at id=ou_x></at> {body}")
    );
}

// =====================================================================
// 五、markdown 探测（上游 `markdown_detect_test.go`）
// =====================================================================

/// 上游 `TestContainsMarkdown` 的两组逐条等价。
#[test]
fn contains_markdown_matches_the_upstream_table() {
    let plain = [
        "",
        "Hello!",
        "sure, on it",
        "Hello, world. How are you?",
        "the build is green",
        "我已经创建了 issue MUL-42",
    ];
    for text in plain {
        assert!(
            !contains_markdown(text),
            "contains_markdown({text:?}) 应为 false"
        );
    }

    let markdown = [
        "# Heading",
        "## Second-level",
        "###### Six",
        "**bold** statement",
        "call __init__ then run",
        "- bullet one\n- bullet two",
        "1. first\n2. second",
        "> quoted line",
        "see [docs](https://example.com)",
        "run `make check` first",
        "```go\nfunc foo() {}\n```",
        "| col1 | col2 |\n|------|------|\n| a    | b    |",
        "---",
        "  - indented bullet",
        "plain prose\nthen a `inline code`\nthen more prose",
    ];
    for text in markdown {
        assert!(
            contains_markdown(text),
            "contains_markdown({text:?}) 应为 true"
        );
    }
}

/// 手写判据与上游 9 条 regex 的**边界**逐条对齐（差异登记见模块文档第 2 条）：
/// 七个井号不算标题、裸反引号不算行内代码、`\r` 不属于 `[ \t]`、`----` 不是分隔线、
/// 单个竖线不是表格行、`***bold***`（三个连排标记）仍然算粗体。
#[test]
fn markdown_boundaries_match_the_upstream_patterns() {
    // `^#{1,6}[ \t]`：七个 `#` 之后是 `#` 而不是空白 ⇒ 不匹配。
    assert!(!contains_markdown("####### too many"));
    // 行内代码必须是**成对**且中间有字符 ⇒ ```` `x` ```` 里 j==0 那一支不触发；
    // 这里给一个真成对的形态确认它是那条判据在起作用。
    assert!(contains_markdown("a `b` c"));
    // `^[ \t]*(?:---)[ \t]*$`：四个短横线不匹配。
    assert!(!contains_markdown("----\n"));
    // `^[ \t]*\|.+\|[ \t]*$`：单竖线不算表格行。
    assert!(!contains_markdown("|\n"));
    assert!(contains_markdown("|a|\n"));
    // `\*\*[^*\n]+\*\*` 在 `***bold***` 上从**第二个**标记起算仍然命中。
    assert!(contains_markdown("***bold***"));
    // `\r` 不属于 `[ \t]` ⇒ `---\r` 与 Go 的 `(?m)$` 一样**不**匹配（模块文档差异 2）。
    assert!(!contains_markdown("---\r\n"));
    // 标题的空白边界：`#x` 不是标题，`# x` 是。
    assert!(!contains_markdown("#x"));
    assert!(contains_markdown("# x"));
}
