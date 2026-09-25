//! `inbox_message.rs` 的用例（上游 `inbox_message_test.go`，**424 行**）。
//!
//! 与上游的**形态**差异两处（与 `markdown/tests.rs` 同源，登记 `docs/32` §36）：
//!
//! 1. app URL 是**参数**（本片 D4），所以上游那些 `t.Setenv` 变成直接传值 —— 用例不再依赖进程
//!    环境，也不再需要为清理环境写 `t.Cleanup`；
//! 2. 上游用 goldmark 渲染后查"有没有一条指向 `evil.example` 的链接"，本仓没有 Markdown 解析器
//!    依赖 ⇒ 换成"守过的卡片里**再没有任何** `]:`"（语料里每个目标都合格）加上"未守的那一半
//!    必须有定义形态"（见 `markdown/tests.rs` 的 D3）。

use serde_json::json;

use super::{
    build_inbox_markdown, inbox_app_url_from_env, inbox_item_body, inbox_item_issue_id,
    inbox_item_link, inbox_type_label, path_escape, query_escape, resolve_inbox_app_url,
    truncate_runes, InboxCardRenderer, INBOX_MARKDOWN_MAX_LEN,
};
use crate::wecom::markdown::break_member_links;
use crate::wecom::outbound::{InboxPush, InboxRenderer};

const APP_URL: &str = "https://example.com";

fn item(value: serde_json::Value) -> serde_json::Value {
    value
}

// =====================================================================
// 卡片形态
// =====================================================================

/// 上游 `TestBuildInboxMarkdown_TitleBodyLink`。
#[test]
fn title_body_and_link() {
    let got = build_inbox_markdown(
        &item(json!({
            "type": "status_changed",
            "title": "登录页 500 错误",
            "body": "from: todo\nto: in_review",
            "issue_id": "9194c058-e8a4-4c15-9c65-86d1784ba715",
        })),
        APP_URL,
        "ws-uuid",
        "acme",
    );
    assert!(
        got.contains("**[状态变更] 登录页 500 错误**"),
        "缺带类型的标题行：{got:?}"
    );
    assert!(got.contains("from: todo\nto: in_review"), "缺正文：{got:?}");
    assert!(
        got.contains(
            "[查看详情](https://example.com/acme/inbox?issue=9194c058-e8a4-4c15-9c65-86d1784ba715)"
        ),
        "缺详情链接：{got:?}"
    );
}

/// 上游 `TestBuildInboxMarkdown_UnknownTypeFallsBackToDefault`。
#[test]
fn unknown_type_falls_back_to_the_default_label() {
    assert_eq!(inbox_type_label("some_new_type"), "新消息");
    assert_eq!(inbox_type_label(""), "新消息");
    assert_eq!(inbox_type_label("mentioned"), "提及你");
    assert_eq!(inbox_type_label("new_comment"), "新评论");
    assert_eq!(inbox_type_label("comment_added"), "新评论");
    let got = build_inbox_markdown(
        &item(json!({"type": "some_new_type", "title": "hi"})),
        APP_URL,
        "ws-uuid",
        "acme",
    );
    assert!(got.contains("**[新消息] hi**"), "{got:?}");
}

/// 上游 `TestBuildInboxMarkdown_FallsBackToWorkspaceUUIDWhenSlugMissing`。
#[test]
fn slug_missing_falls_back_to_the_workspace_uuid() {
    let got = build_inbox_markdown(
        &item(json!({"type": "new_comment", "title": "t", "issue_id": "iid"})),
        APP_URL,
        "ws-uuid",
        "",
    );
    assert!(
        got.contains("https://example.com/ws-uuid/inbox?issue=iid"),
        "{got:?}"
    );
}

/// 上游 `TestBuildInboxMarkdown_NoAppURLDropsLink`：没配 app URL ⇒ 整段链接省掉。
#[test]
fn no_app_url_drops_the_link_section() {
    let got = build_inbox_markdown(
        &item(json!({"type": "new_comment", "title": "t"})),
        "",
        "ws-uuid",
        "acme",
    );
    assert_eq!(got, "**[新评论] t**");
    assert!(!got.contains("查看详情"));
}

/// 上游 `TestBuildInboxMarkdown_NonHTTPSAppURLIsRejected`：`http://` 的覆写被**静默丢掉**。
#[test]
fn a_non_https_app_url_is_rejected() {
    assert_eq!(resolve_inbox_app_url("http://insecure.example.com"), None);
    assert_eq!(resolve_inbox_app_url("ftp://x"), None);
    assert_eq!(resolve_inbox_app_url("   "), None);
    // 只有 scheme 本身：`TrimRight("/")` 是上游逐字的做法，所以它把那条斜线也去掉。
    assert_eq!(resolve_inbox_app_url("https://"), Some("https:".into()));
    assert_eq!(
        resolve_inbox_app_url("https://example.com//"),
        Some("https://example.com".into())
    );
    assert_eq!(
        resolve_inbox_app_url("  https://example.com/  "),
        Some("https://example.com".into())
    );
    let got = build_inbox_markdown(
        &item(json!({"type": "new_comment", "title": "t"})),
        "http://insecure.example.com",
        "ws-uuid",
        "acme",
    );
    assert!(!got.contains("insecure.example.com"), "{got:?}");
}

/// 上游的 env 优先级：第一个**可用**的（HTTPS）值获胜，一个非 HTTPS 的前缀不会挡住后面的。
#[test]
fn app_url_resolution_prefers_the_first_usable_value() {
    let got = inbox_app_url_from_env(|name| match name {
        "WECOM_APP_URL" => Some("http://nope".into()),
        "MULTICA_APP_URL" => Some("https://multica.example/".into()),
        "FRONTEND_ORIGIN" => Some("https://frontend.example".into()),
        _ => None,
    });
    assert_eq!(got, Some("https://multica.example".into()));

    let none = inbox_app_url_from_env(|_| None);
    assert_eq!(none, None);
}

/// 上游 `TestBuildInboxMarkdown_TruncatesLongBody`：只截正文，链接必须活下来。
#[test]
fn a_long_body_is_truncated_and_the_link_survives() {
    let body = "我".repeat(5000);
    let got = build_inbox_markdown(
        &item(json!({"type": "new_comment", "title": "hi", "body": body, "issue_id": "iid"})),
        APP_URL,
        "ws-uuid",
        "acme",
    );
    assert!(got.contains("..."), "缺截断标记");
    assert!(
        got.ends_with("acme/inbox?issue=iid)"),
        "链接必须活过截断：{:?}",
        &got[got.len().saturating_sub(80)..]
    );
    assert!(got.chars().count() <= INBOX_MARKDOWN_MAX_LEN);
}

/// 上游 `TestBuildInboxMarkdown_HandlesPointerBodyAndIssueID`：可空字段（JSON `null`）与字符串
/// 取值等价 —— `serde_json` 里那两种写法就是同一件事。
#[test]
fn nullable_body_and_issue_id() {
    let got = build_inbox_markdown(
        &item(json!({"type": "new_comment", "title": "hi", "body": "详情", "issue_id": "iid"})),
        APP_URL,
        "ws-uuid",
        "acme",
    );
    assert!(got.contains("详情"), "{got:?}");
    assert!(got.contains("issue=iid"), "{got:?}");

    // `null` = 缺失。
    let nulled =
        item(json!({"type": "new_comment", "title": "hi", "body": null, "issue_id": null}));
    assert_eq!(inbox_item_body(&nulled), "");
    assert_eq!(inbox_item_issue_id(&nulled), None);
    let got = build_inbox_markdown(&nulled, APP_URL, "ws-uuid", "acme");
    assert_eq!(
        got,
        "**[新评论] hi**\n[查看详情](https://example.com/acme/inbox)"
    );
}

/// 上游 `TestBuildInboxMarkdown_EmptyItemReturnsEmpty`。
#[test]
fn an_empty_item_renders_nothing() {
    assert_eq!(build_inbox_markdown(&item(json!({})), "", "ws", "slug"), "");
    // 只有类型、没有标题：仍然是一张有内容的卡（标签本身就是内容）。
    assert!(
        !build_inbox_markdown(&item(json!({"type": "mentioned"})), "", "ws", "slug").is_empty()
    );
}

/// 上游 `TestTruncateRunes`。
#[test]
fn truncate_runes_is_rune_based() {
    let cases = [
        ("abc", 0, ""),
        ("abc", 3, "abc"),
        ("abc", 2, "ab"),
        ("你好世界", 2, "你好"),
        ("你好世界", 4, "你好世界"),
        ("你好世界", 5, "你好世界"),
    ];
    for (input, max, want) in cases {
        assert_eq!(
            truncate_runes(input, max),
            want,
            "truncate_runes({input:?},{max})"
        );
    }
}

/// 深链的两段编码：路径段保留 `/` 以外的可读字符，查询值只保留 `-_.~`。
#[test]
fn link_escaping() {
    assert_eq!(path_escape("a b"), "a%20b");
    assert_eq!(path_escape("a/b"), "a%2Fb");
    assert_eq!(path_escape("acme"), "acme");
    assert_eq!(query_escape("a b"), "a%20b");
    assert_eq!(query_escape("a/b"), "a%2Fb");
    assert_eq!(query_escape("a-b_c.d~e"), "a-b_c.d~e");
    let link = inbox_item_link(&item(json!({"issue_id": "A B"})), "https://x/", "ws", "a b");
    assert_eq!(link, "https://x/a%20b/inbox?issue=A%20B");
}

// =====================================================================
// 成员文本不许变成机器人签名的 Markdown
// =====================================================================

/// 上游 `TestInboxCardDoesNotRenderMemberAuthoredLinks`（见模块文档的判据替换）。
#[test]
fn member_links_do_not_survive_into_the_card() {
    let title = "[click here](http://evil.example)";
    let body = "and the body [too](http://evil.example)";
    let out = build_inbox_markdown(
        &item(json!({"type": "mentioned", "title": title, "body": body})),
        "",
        "ws-uuid",
        "acme",
    );
    assert!(
        !out.contains("]("),
        "\"](\" 相邻地出现在机器人签名的卡片里：{out:?}"
    );
}

/// 上游 `TestInboxCardFitsTheCapEvenWithAHugeTitle`。
#[test]
fn the_card_always_fits_the_cap() {
    let huge = "标题".repeat(4000);
    let out = build_inbox_markdown(
        &item(json!({"type": "mentioned", "title": huge, "body": "body"})),
        "https://multica.example",
        "ws-uuid",
        "acme",
    );
    let runes = out.chars().count();
    assert!(
        runes <= INBOX_MARKDOWN_MAX_LEN,
        "卡片 {runes} rune，上限 {INBOX_MARKDOWN_MAX_LEN} —— WeCom 会拒帧，这条推送就丢了"
    );
    assert!(out.contains("查看详情"), "查看详情入口被丢掉了");
}

/// 上游 `TestInboxCardKeepsAnOrdinaryBracketedTitleVerbatim`：日常标题形态必须逐字通过，
/// 且**绝不**出现反斜杠。
#[test]
fn an_ordinary_bracketed_title_survives_verbatim() {
    let out = build_inbox_markdown(
        &item(json!({
            "type": "status_changed",
            "title": "[Bug] 登录失败",
            "body": "从 (todo) 到 (in_review)!",
        })),
        "",
        "ws-uuid",
        "acme",
    );
    assert_eq!(
        out,
        "**[状态变更] [Bug] 登录失败**\n从 (todo) 到 (in_review)!"
    );
    assert!(!out.contains('\\'), "反斜杠进了卡片：{out:?}");
}

/// 上游 `TestInboxCardKeepsAReferenceLikeTitleVerbatim`：`[Bug]: 登录失败` 是日常标题，
/// 一条"见到 `]:` 就破"的规则会把它换掉 —— 逐字节，否则闸太钝。
#[test]
fn a_reference_like_title_survives_verbatim() {
    let out = build_inbox_markdown(
        &item(json!({
            "type": "status_changed",
            "title": "[Bug]: 登录失败",
            "body": "[WIP]: 明天再看\n\n[复现步骤]: 见 (todo) 到 (in_review)!",
        })),
        "",
        "ws-uuid",
        "acme",
    );
    assert_eq!(
        out,
        "**[状态变更] [Bug]: 登录失败**\n[WIP]: 明天再看\n\n[复现步骤]: 见 (todo) 到 (in_review)!"
    );
}

/// 上游 `TestInboxCardNeverPutsCloseBracketNextToOpenParen`：卡片安全性所依赖的那条性质 ——
/// `](` 绝不相邻地出现。app URL 置空，所以输出里的任何 `](` 都出自成员之手。
#[test]
fn no_seam_ever_fuses_a_bracket_onto_a_paren() {
    let cases: [(&str, String, String); 7] = [
        (
            "标题里的链接",
            "[click here](http://evil.example)".into(),
            "body".into(),
        ),
        (
            "正文里的链接",
            "title".into(),
            "and the body [too](http://evil.example)".into(),
        ),
        (
            "图片",
            "![img](http://evil.example/x.png)".into(),
            "body".into(),
        ),
        (
            "嵌套方括号",
            "[a[b]](http://evil.example)".into(),
            "body".into(),
        ),
        (
            "成员写的反斜杠",
            r"x\](http://evil.example)".into(),
            r"y\](http://evil.example)".into(),
        ),
        (
            "正文在接缝处被截",
            "t".into(),
            format!("{}{}", "a".repeat(3900), "](x)".repeat(100)),
        ),
        ("标题在接缝处被截", "标题](x)".repeat(2000), "body".into()),
    ];
    for (name, title, body) in cases {
        let out = build_inbox_markdown(
            &item(json!({"type": "mentioned", "title": title, "body": body})),
            "",
            "ws-uuid",
            "acme",
        );
        assert!(
            !out.contains("]("),
            "{name}: \"](\" 相邻地出现 —— 那是机器人签名卡片里一条能用的链接：{}",
            window(&out)
        );
    }
}

/// 上游 `TestInboxCardSeamSurvivesEveryCutOffset`：把截断点扫过一段密集的 `](`，于是总有若干次
/// 迭代切在图案内部的每一个偏移上。
#[test]
fn the_seam_survives_every_cut_offset() {
    for pad in 0..12 {
        let body = format!("{}{}", "a".repeat(3900 + pad), "](x)".repeat(60));
        let title = format!("{}{}", "标".repeat(3900 + pad), "](x)".repeat(60));
        for (name, title, body) in [
            ("body", "t".to_string(), body),
            ("title", title, "b".to_string()),
        ] {
            let out = build_inbox_markdown(
                &item(json!({"type": "mentioned", "title": title, "body": body})),
                "",
                "ws-uuid",
                "acme",
            );
            assert!(
                !out.contains("]("),
                "{name} 在 pad={pad} 处把一个 \"]\" 与一个 \"(\" 焊在了一起：{}",
                window(&out)
            );
            let runes = out.chars().count();
            assert!(
                runes <= INBOX_MARKDOWN_MAX_LEN,
                "{name} pad={pad}: 卡片 {runes} rune，上限 {INBOX_MARKDOWN_MAX_LEN}"
            );
        }
    }
}

/// 上游 `TestInboxCardBudgetsTheSpacesItInserts`：破邻接**插入了一个可见字符**，所以一篇密集的
/// 正文出去时比进来时长（极限情况 1.5 倍）。预算必须在**涨完之后**的文本上算。
#[test]
fn the_budget_counts_the_spaces_the_break_inserts() {
    let out = build_inbox_markdown(
        &item(json!({"type": "mentioned", "title": "t", "body": "](".repeat(4000)})),
        "https://multica.example",
        "ws-uuid",
        "acme",
    );
    let runes = out.chars().count();
    assert!(
        runes <= INBOX_MARKDOWN_MAX_LEN,
        "卡片 {runes} rune，上限 {INBOX_MARKDOWN_MAX_LEN} —— 插入的空格没被预算"
    );
}

/// 上游 `TestInboxCardBudgetsTheSpacesTheDefinitionBreakInserts`：另一道闸的预算版本。
#[test]
fn the_budget_counts_the_spaces_the_definition_break_inserts() {
    let out = build_inbox_markdown(
        &item(json!({
            "type": "mentioned",
            "title": "t",
            "body": "[a]: https://evil.example\n".repeat(500),
        })),
        "https://multica.example",
        "ws-uuid",
        "acme",
    );
    let runes = out.chars().count();
    assert!(
        runes <= INBOX_MARKDOWN_MAX_LEN,
        "卡片 {runes} rune，上限 {INBOX_MARKDOWN_MAX_LEN} —— 插入的空格没被预算"
    );
}

/// 上游 `TestInboxCardDefinesNoResolvableLinkReference` 的**卡片级**那一半：证明构造器真的跑了
/// 那一道闸、在**两个字段**上都跑、且两支截断都没有把定义还回来。
///
/// 语料用 `markdown/tests.rs` 里同一张 `linkReferenceAttacks` 的形态（每个目标都合格 ⇒ 守过的
/// 卡片里不该再有任何 `]:`）。上游这一条要保证"定义标签能开始的位置"，所以在标题那一行与攻击之间
/// 放一个空行。
#[test]
fn the_card_defines_no_resolvable_link_reference() {
    let attacks = [
        "[重置密码]: https://evil.example\n\n[重置密码]",
        "[重置密码]:\nhttps://evil.example\n\n[重置密码]",
        "> [重置密码]:\n> https://evil.example\n\n[重置密码]",
        "- [重置密码]:\n  https://evil.example\n\n[重置密码]",
        "[重置密码]: <https://evil.example>\n\n[重置密码]",
        "[重置密码]: //evil.example/x\n\n[重置密码]",
        "[重置密码]: &#x68;ttps://evil.example\n\n[重置密码]",
    ];
    for attack in attacks {
        let body = format!("看这里\n\n{attack}");
        // 未守的卡片（按构造器的组成方式拼）必须**有**定义形态，否则这条用例证明不了任何事。
        let unguarded = format!("**[提及你] t**\n{body}");
        assert!(
            unguarded.contains("]:"),
            "未守的卡片里没有定义形态：{unguarded:?}"
        );
        assert!(
            !break_member_links(&body).contains("]:"),
            "闸没开火：{body:?}"
        );

        let out = build_inbox_markdown(
            &item(json!({"type": "mentioned", "title": "t", "body": body})),
            "",
            "ws-uuid",
            "acme",
        );
        assert!(
            !out.contains("]:"),
            "正文：成员定义的一条链接活进了卡片：{out:?}"
        );

        // 同一条路走标题。一个带换行的标题同样能驮一条定义，而它走的是**另一支**长度分支。
        let out = build_inbox_markdown(
            &item(json!({"type": "mentioned", "title": body, "body": "b"})),
            "",
            "ws-uuid",
            "acme",
        );
        assert!(
            !out.contains("]:"),
            "标题：成员定义的一条链接活进了卡片：{out:?}"
        );
    }
}

/// 上游 `TestInboxCardSeamKeepsDefinitionsBrokenAtEveryCutOffset`：把截断点扫过一段密集的定义。
#[test]
fn the_seam_keeps_definitions_broken_at_every_cut_offset() {
    for pad in 0..12 {
        let filler = "a".repeat(3900 + pad);
        let dense = "[x]: https://evil.example\n\n[x]\n\n".repeat(40);
        for (name, title, body) in [
            ("body", "t".to_string(), format!("{filler}\n\n{dense}")),
            ("title", format!("{filler}\n\n{dense}"), "b".to_string()),
        ] {
            let out = build_inbox_markdown(
                &item(json!({"type": "mentioned", "title": title, "body": body})),
                "",
                "ws-uuid",
                "acme",
            );
            assert!(
                !out.contains("]:"),
                "{name} 在 pad={pad} 处放过了一条成员定义的链接：{out:?}"
            );
            let runes = out.chars().count();
            assert!(
                runes <= INBOX_MARKDOWN_MAX_LEN,
                "{name} pad={pad}: 卡片 {runes} rune，上限 {INBOX_MARKDOWN_MAX_LEN}"
            );
        }
    }
}

/// 失败信息里读起来方便的那一小段窗口。
fn window(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let hi = chars.len().min(60);
    let head: String = chars[..hi].iter().collect();
    let tail: String = chars[chars.len().saturating_sub(20)..].iter().collect();
    format!("{head}…{tail}（共 {} rune）", chars.len())
}

// =====================================================================
// 端口实现
// =====================================================================

/// [`InboxCardRenderer`] 就是那张卡片 —— 端口实现与纯函数之间没有第二份形态。
#[test]
fn the_port_renders_the_same_card() {
    let renderer = InboxCardRenderer::new("https://example.com/");
    let push = InboxPush {
        item_id: "n1".into(),
        item_type: "mentioned".into(),
        issue_id: "iid".into(),
        recipient_type: "member".into(),
        recipient_id: "u1".into(),
        workspace_id: "ws-uuid".into(),
        title: "登录页 500 错误".into(),
        body: "详情".into(),
    };
    let card = renderer.render(&push, "acme").expect("render");
    assert_eq!(
        card,
        build_inbox_markdown(
            &item(
                json!({"type": "mentioned", "title": "登录页 500 错误", "body": "详情", "issue_id": "iid"})
            ),
            "https://example.com",
            "ws-uuid",
            "acme",
        )
    );
    assert!(card.contains("**[提及你] 登录页 500 错误**"), "{card:?}");

    // 既没标题也没类型 ⇒ 失败关闭：**不投递**（返回 `None`）。
    let empty = InboxPush {
        item_id: "n2".into(),
        item_type: String::new(),
        issue_id: String::new(),
        recipient_type: "member".into(),
        recipient_id: "u1".into(),
        workspace_id: "ws-uuid".into(),
        title: String::new(),
        body: String::new(),
    };
    assert_eq!(renderer.render(&empty, "acme"), None);

    // 非 HTTPS 的 app 主机在装配时就被丢掉 ⇒ 卡片不带链接，但仍然发得出去。
    let insecure = InboxCardRenderer::new("http://insecure.example");
    assert_eq!(insecure.app_url(), None);
    let card = insecure.render(&push, "acme").expect("render");
    assert!(!card.contains("insecure.example"), "{card:?}");
    assert!(!card.contains("查看详情"), "{card:?}");
}

/// `InboxCardRenderer` 的 `Debug` 只报"配没配"（app 主机是部署配置，形态随环境而变）。
#[test]
fn the_renderer_debug_reports_only_configuration() {
    let renderer = InboxCardRenderer::new("https://secret-host.internal");
    let shown = format!("{renderer:?}");
    assert!(!shown.contains("secret-host"), "{shown}");
    assert!(shown.contains("app_url_configured: true"), "{shown}");
    assert!(format!("{:?}", InboxCardRenderer::new("")).contains("false"));
}
