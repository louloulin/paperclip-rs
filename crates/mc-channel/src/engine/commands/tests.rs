//! 命令解析 / 标题 / 出处的向量用例（上游 `fresh_command_test.go` / `issue_command_test.go` /
//! `title_test.go` 的逐条移植）。
//!
//! 三条形态纪律（大小写敏感 / 只认第一非空行 / token 完整）在上游是**产品故意**的行为，
//! 所以这里逐条钉住 —— 它们一旦漂移，20 个 adapter 切片会各自"修"出不同版本。

use super::*;
use crate::engine::resolvers::{CommandClassifier, CommandIntent};

fn classifier() -> ChannelCommandClassifier {
    ChannelCommandClassifier
}

// ---------------------------------------------------------------------
// /clear 与 /new
// ---------------------------------------------------------------------

#[test]
fn control_command_classifies_the_shared_syntax() {
    assert_eq!(
        parse_control_command("/clear reset this"),
        Some(ControlCommand {
            kind: ControlCommandKind::FreshSession,
            body: "reset this".to_string()
        })
    );
    assert_eq!(
        parse_control_command("/new start this"),
        Some(ControlCommand {
            kind: ControlCommandKind::NewChat,
            body: "start this".to_string()
        })
    );
    assert_eq!(
        parse_control_command("please /new later"),
        None,
        "句子中间的指令不匹配"
    );
}

#[test]
fn fresh_session_command_follows_the_issue_rules() {
    let cases: [(&str, bool, &str); 8] = [
        ("/clear start from scratch", true, "start from scratch"),
        (
            "\n\n/clear re-check the deploy",
            true,
            "re-check the deploy",
        ),
        (
            "/clear title\nline one\nline two",
            true,
            "title\nline one\nline two",
        ),
        ("/clear", true, ""),
        ("/clearness is not a command", false, ""),
        ("please /clear this run", false, ""),
        ("/Clear help", false, ""),
        ("help me normally", false, ""),
    ];
    for (body, want_match, want_body) in cases {
        assert_eq!(
            parse_fresh_session_command(body).as_deref(),
            want_match.then_some(want_body),
            "ParseFreshSessionCommand({body:?})"
        );
    }
}

#[test]
fn new_chat_command_tolerates_leading_blank_lines_and_tabs() {
    let cases: [(&str, bool, &str); 7] = [
        ("/new investigate this", true, "investigate this"),
        (
            "\n\t\n/new\tkeep layout\nsecond line",
            true,
            "keep layout\nsecond line",
        ),
        ("/new", true, ""),
        ("/new /issue ordinary text", true, "/issue ordinary text"),
        ("/newness", false, ""),
        ("/New", false, ""),
        ("please /new", false, ""),
    ];
    for (body, want_match, want_body) in cases {
        assert_eq!(
            parse_new_chat_command(body).as_deref(),
            want_match.then_some(want_body),
            "ParseNewChatCommand({body:?})"
        );
    }
}

// ---------------------------------------------------------------------
// /issue
// ---------------------------------------------------------------------

#[test]
fn issue_command_recognizes_exactly_the_documented_shapes() {
    let cases: [(&str, bool, &str, &str); 11] = [
        ("/issue Fix the login bug", true, "Fix the login bug", ""),
        (
            "/issue Fix login\nIt 500s on submit\nsince Tuesday",
            true,
            "Fix login",
            "It 500s on submit\nsince Tuesday",
        ),
        ("/issue", true, "", ""),
        ("/issue   ", true, "", ""),
        ("\n\n/issue Title", true, "Title", ""),
        ("/issue\tTabbed", true, "Tabbed", ""),
        ("/issuetracker do thing", false, "", ""),
        ("hey /issue not a command", false, "", ""),
        ("/Issue Title", false, "", ""),
        ("", false, "", ""),
        ("   \n  ", false, "", ""),
    ];
    for (body, want_ok, want_title, want_desc) in cases {
        let parsed = parse_issue_command(body);
        assert_eq!(parsed.is_some(), want_ok, "ParseIssueCommand({body:?})");
        if want_ok {
            let (title, description) = parsed.expect("parsed");
            assert_eq!(
                (title.as_str(), description.as_str()),
                (want_title, want_desc)
            );
        }
    }
}

#[test]
fn issue_description_preserves_the_inline_media_layout() {
    let body = "/issue explain below questions\nWhat is this?\n[Image]\nAnd what is this?\n[Image]";
    assert_eq!(
        issue_description_from_command_body(
            body,
            "/issue explain below questions\nWhat is this?And what is this?",
            "flattened fallback"
        ),
        "What is this?\n[Image]\nAnd what is this?\n[Image]"
    );
}

#[test]
fn issue_description_excludes_the_enriched_prefix() {
    let body = "> quoted context\n/issue Real intent\nrepro steps";
    assert_eq!(
        issue_description_from_command_body(body, "/issue Real intent\nrepro steps", "fallback"),
        "repro steps"
    );
}

#[test]
fn issue_description_ignores_directive_lines_inside_the_enriched_prefix() {
    let body =
        "<quoted_message>\n/issue Old intent\n</quoted_message>\n/issue Real intent\nrepro steps";
    assert_eq!(
        issue_description_from_command_body(body, "/issue Real intent\nrepro steps", "fallback"),
        "repro steps"
    );
}

#[test]
fn issue_description_handles_a_repeated_directive_line() {
    let body =
        "<quoted_message>\n/issue Same\n</quoted_message>\n/issue Same\nDetails\n/issue Same";
    let command_text = "/issue Same\nDetails\n/issue Same";
    assert_eq!(
        issue_description_from_command_body(body, command_text, "fallback"),
        "Details\n/issue Same"
    );
}

#[test]
fn issue_description_falls_back_when_the_directive_is_gone() {
    assert_eq!(
        issue_description_from_command_body(
            "rewritten body",
            "/issue Missing",
            "parsed description"
        ),
        "parsed description"
    );
}

// ---------------------------------------------------------------------
// 分类器（Router 的唯一入口）
// ---------------------------------------------------------------------

#[test]
fn the_classifier_returns_the_shared_vocabulary() {
    let classifier = classifier();
    assert_eq!(
        classifier.classify("/new turn the page"),
        CommandIntent::NewChat {
            body: "turn the page".to_string()
        }
    );
    assert_eq!(
        classifier.classify("/clear"),
        CommandIntent::FreshSession {
            body: String::new()
        }
    );
    assert_eq!(
        classifier.classify("/issue Fix it\nsteps"),
        CommandIntent::Issue {
            title: "Fix it".to_string(),
            description: "steps".to_string()
        }
    );
    assert_eq!(classifier.classify("ordinary text"), CommandIntent::None);
    // `/clear` 与 `/issue` 在同一第一行上**互斥**（只有第一非空行能是命令）。
    assert!(matches!(
        classifier.classify("/clear /issue not a command"),
        CommandIntent::FreshSession { .. }
    ));
    assert_eq!(
        classifier.classify("/issuetracker"),
        CommandIntent::None,
        "token 必须是完整的"
    );
}

// ---------------------------------------------------------------------
// 标题
// ---------------------------------------------------------------------

#[test]
fn chat_title_derivation_matches_upstream_vectors() {
    let cases: [(&str, &str); 5] = [
        ("\n#  发布检查\n后续", "发布检查"),
        ("[部署文档](https://example.com) **失败**", "部署文档 失败"),
        ("![架构图](https://example.com/a.png)", "架构图"),
        (
            "一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十一二三",
            "一二三四五六七八九十一二三四五六七八九十一二三四五六七八九…",
        ),
        (
            "12345678901234567890123456 890123",
            "12345678901234567890123456 89…",
        ),
    ];
    for (body, want) in cases {
        assert_eq!(derive_chat_title(body), want, "derive_chat_title({body:?})");
    }
}

#[test]
fn fences_are_not_cross_line_and_only_the_first_line_is_titled() {
    // 围栏正则**不吃换行**（Go 的 `.` 不吃），而标题只看第一非空行
    // ⇒ 单独一行上的 ```` ```rust ```` 去掉反引号之后就是 `rust`（上游同款行为）。
    assert_eq!(
        derive_chat_title("```rust\nfn main() {}\n```\nreal question"),
        "rust"
    );
    assert_eq!(
        derive_chat_title("```rust``` 怎么用"),
        "怎么用",
        "同一行上的成对围栏被整体换成空格"
    );
    assert_eq!(derive_chat_title("   \n\t\n"), "");
}

#[test]
fn the_title_limit_counts_unicode_code_points() {
    // 30 个码点整 ⇒ 原样（不截断、不加省略号）。
    let exactly = "一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十";
    assert_eq!(exactly.chars().count(), DETERMINISTIC_TITLE_LIMIT);
    assert_eq!(derive_chat_title(exactly), exactly);
    // 截断处**不留**尾随空白（29 个码点里第 29 个是空格 ⇒ 去掉）。
    assert_eq!(
        derive_chat_title("abcdefghijklmnopqrstuvwxyzab tail"),
        "abcdefghijklmnopqrstuvwxyzab…"
    );
}

#[test]
fn media_placeholder_lines_wait_for_the_filename() {
    assert_eq!(derive_first_message_title("[Image]\n[File]", true), "");
    assert_eq!(
        derive_first_message_title("[Image]\n点评一下", true),
        "点评一下"
    );
    assert_eq!(
        derive_first_message_title("Inspect this\n[Image]", true),
        "Inspect this"
    );
    assert_eq!(
        derive_first_message_title("Use [Image] literally\n[Image]", true),
        "Use [Image] literally",
        "行内的字面占位不算占位行"
    );
    assert_eq!(
        derive_first_message_title("[Image]", false),
        "[Image]",
        "没有媒体时它就是用户打的字"
    );
    assert_eq!(media_type_title(MessageKind::Image), "Image chat");
    assert_eq!(media_type_title(MessageKind::Unknown), "File chat");
}

#[test]
fn the_title_source_consumes_only_an_applied_fresh_directive() {
    let cases: [(&str, &str, bool, &str); 6] = [
        (
            "<quoted_message>history</quoted_message>\n\nanswer",
            "/clear answer",
            true,
            "answer",
        ),
        (
            "/clear answer",
            "/clear /clear answer",
            true,
            "/clear answer",
        ),
        (
            "<recent_context>history</recent_context>\n\n/clear answer",
            "/clear answer",
            false,
            "/clear answer",
        ),
        (
            "<recent_context>history</recent_context>\n\nanswer",
            "answer",
            true,
            "answer",
        ),
        ("[Image]", "/clear", true, ""),
        ("body fallback", "  ", true, "body fallback"),
    ];
    for (body, command, fresh, want) in cases {
        assert_eq!(
            chat_title_source(body, command, fresh),
            want,
            "chat_title_source({body:?}, {command:?}, {fresh})"
        );
    }
}

// ---------------------------------------------------------------------
// 出处
// ---------------------------------------------------------------------

#[test]
fn provenance_defaults_to_deliver_for_pre_sealing_tasks() {
    let task_id = mc_core::id::Id::new();
    assert!(
        task_input_is_channel_ingested(None, false),
        "没有输入批次 = 密封之前的渠道任务 ⇒ 默认投递"
    );
    assert!(task_input_is_channel_ingested(Some(task_id), true));
    assert!(
        !task_input_is_channel_ingested(Some(task_id), false),
        "直接（web/mobile）任务复用渠道会话，但回复留在 Multica"
    );
}
