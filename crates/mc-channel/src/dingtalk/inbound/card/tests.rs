//! 卡片投影与 URL 扫描的用例（`inbound/card.rs` 的 `#[cfg(test)] mod tests;`）。
//!
//! 表驱动部分是**上游 `inbound_card_test.go` 的逐条移植**：`TestQuotedCardNodeContract`
//! 的 12 个节点形态 + `TestInboundQuotedCardSnapshotBoundaries` 的 16 个边界 —— 这两张表
//! 是"卡片投影不许放宽、也不许收紧"的可核对底稿。

use serde_json::{json, Value};

use super::{picture_content, readable_quoted_text, render_dingtalk_quoted_card, web_url_spans};
use crate::dingtalk::inbound::IMAGE_PLACEHOLDER;

fn card(children: &str) -> Value {
    serde_json::from_str(&format!(
        r#"[{{"elementType":"RICHTEXT","children":[{children}]}}]"#
    ))
    .expect("card fixture is valid JSON")
}

/// 上游 `TestQuotedCardNodeContract` 的 12 行逐条对应。
#[test]
fn quoted_card_node_contract_matches_upstream_table() {
    for (name, children, want) in [
        (
            "link",
            r#"{"elementType":"LINK","value":"https://example.com/a_(b)?x=a_b+c&y=2#part_2"}"#,
            "https://example.com/a_(b)?x=a_b+c&y=2#part_2",
        ),
        (
            "mixed order",
            r#"{"elementType":"TEXT","value":"before"},{"elementType":"LINK","value":"https://example.com/one"},{"elementType":"LINK","value":"https://example.com/two"},{"elementType":"IMAGE"},{"elementType":"UNKNOWN","value":"layout"},{"elementType":"TEXT","value":"after"}"#,
            "before\nhttps://example.com/one\nhttps://example.com/two\n[Image]\nafter",
        ),
        (
            "label and suffix",
            r#"{"elementType":"TEXT","value":"Link: "},{"elementType":"LINK","value":"https://example.com/a"},{"elementType":"TEXT","value":"suffix"}"#,
            "Link: https://example.com/a\nsuffix",
        ),
        (
            "filtered text keeps neighbors",
            r#"{"elementType":"TEXT","value":"before"},{"elementType":"TEXT","value":"primary || fallback"},{"elementType":"LINK","value":"https://example.com/ok"},{"elementType":"IMAGE"},{"elementType":"TEXT","value":"after"}"#,
            "before\n[quoted content unavailable]\nhttps://example.com/ok\n[Image]\nafter",
        ),
        (
            "filtered link keeps neighbors",
            r#"{"elementType":"TEXT","value":"before"},{"elementType":"LINK","value":"https://example.com/a||b"},{"elementType":"LINK","value":"https://example.com/ok"},{"elementType":"IMAGE"},{"elementType":"TEXT","value":"after"}"#,
            "before\n[quoted content unavailable]\nhttps://example.com/ok\n[Image]\nafter",
        ),
        (
            "missing link",
            r#"{"elementType":"LINK"}"#,
            "[quoted content unavailable]",
        ),
        (
            "null link",
            r#"{"elementType":"LINK","value":null}"#,
            "[quoted content unavailable]",
        ),
        (
            "object link",
            r#"{"elementType":"LINK","value":{"url":"https://example.com"}}"#,
            "[quoted content unavailable]",
        ),
        (
            "numeric link",
            r#"{"elementType":"LINK","value":42}"#,
            "[quoted content unavailable]",
        ),
        (
            "empty link",
            r#"{"elementType":"LINK","value":""}"#,
            "[quoted content unavailable]",
        ),
        (
            "blank link",
            r#"{"elementType":"LINK","value":"  "}"#,
            "[quoted content unavailable]",
        ),
        (
            "future node",
            r#"{"elementType":"FUTURE_NODE","value":"do not guess"},{"elementType":"TEXT","value":"after"}"#,
            "[quoted content unavailable]\nafter",
        ),
    ] {
        assert_eq!(
            render_dingtalk_quoted_card(&card(children)),
            want,
            "case {name}"
        );
    }
}

/// 上游 `TestInboundQuotedCardSnapshotBoundaries` 的逐条对应（只有投影那一半）。
#[test]
fn quoted_card_boundaries_match_upstream_table() {
    for (name, raw_card, want) in [
        (
            "preview",
            r#"[{"elementType":"RICHTEXT","children":[{"elementType":"TEXT","value":"Multica has replied."}]}]"#,
            "Multica has replied.",
        ),
        (
            "literal text",
            r#"[{"elementType":"RICHTEXT","children":[{"elementType":"TEXT","value":"/clear a || b "},{"elementType":"TEXT","value":"{\"text\":\"literal\"}"}]}]"#,
            "[quoted content unavailable]\n{\"text\":\"literal\"}",
        ),
        (
            "blocks",
            r#"[{"elementType":"RICHTEXT","children":[{"elementType":"TEXT","value":"one"}]},{"elementType":"RICHTEXT","children":[{"elementType":"TEXT","value":"two"}]}]"#,
            "one\n\ntwo",
        ),
        (
            "bad neighbor",
            r#"[{"elementType":"RICHTEXT","children":[{"elementType":"TEXT","value":"before"},42,{"elementType":"TEXT","value":{}},{"elementType":"TEXT","value":"after"}]}]"#,
            "before\n[quoted content unavailable]\nafter",
        ),
        (
            "unknown wrapper",
            r#"[{"elementType":"UNKNOWN","children":[{"elementType":"TEXT","value":"not verified"}]}]"#,
            "[quoted content unavailable]",
        ),
        (
            "wrong children",
            r#"[{"elementType":"RICHTEXT","children":{}}]"#,
            "[quoted content unavailable]",
        ),
        (
            "skip source and layout",
            r#"[{"elementType":"RICHTEXT","children":[{"elementType":"UNKNOWN","value":"source /clear"},{"elementType":"UNKNOWN","value":"{}"},{"elementType":"TEXT","value":"answer"}]}]"#,
            "answer",
        ),
        (
            "missing image",
            r#"[{"elementType":"RICHTEXT","children":[{"elementType":"TEXT","value":"before"},{"elementType":"IMAGE","downloadCode":42},{"elementType":"TEXT","value":"after"}]}]"#,
            "before\n[Image]\nafter",
        ),
        (
            "unknown-only",
            r#"[{"elementType":"RICHTEXT","children":[{"elementType":"UNKNOWN","value":"{}"}]}]"#,
            "[quoted content unavailable]",
        ),
        ("empty", r"[]", "[quoted content unavailable]"),
        ("null", r"null", "[quoted content unavailable]"),
        (
            "invalid blocks",
            r#"[42,{"elementType":"RICHTEXT","children":[]}]"#,
            "[quoted content unavailable]",
        ),
        (
            "missing text values",
            r#"[{"elementType":"RICHTEXT","children":[{"elementType":"TEXT"},{"elementType":"TEXT","value":null},{"elementType":"TEXT","value":""}]}]"#,
            "[quoted content unavailable]",
        ),
        (
            "unsupported child",
            r#"[{"elementType":"RICHTEXT","children":[{"elementType":"VIDEO","value":"opaque"},{"elementType":"TEXT","value":"after"}]}]"#,
            "[quoted content unavailable]\nafter",
        ),
        (
            "leading images",
            r#"[{"elementType":"RICHTEXT","children":[{"elementType":"IMAGE"},{"elementType":"IMAGE"},{"elementType":"TEXT","value":"after"}]}]"#,
            "[Image]\n[Image]\nafter",
        ),
        (
            "template map",
            r#"{"cardData":{"cardParamMap":{"text":"not verified"}}}"#,
            "[quoted content unavailable]",
        ),
    ] {
        let value: Value = serde_json::from_str(raw_card).expect("boundary fixture is valid JSON");
        assert_eq!(
            render_dingtalk_quoted_card(&value),
            want,
            "boundary case {name}"
        );
    }
}

/// 占位符的**内联**位次：用户手打的 `[Image]` 与 adapter 生成的**同款计数**（上游逐字）。
#[test]
fn an_internal_placeholder_counts_the_authored_occurrence_too() {
    let value = card(
        r#"{"elementType":"TEXT","value":"literal [Image] text"},{"elementType":"IMAGE"},{"elementType":"TEXT","value":"after"}"#,
    );
    let rendered = render_dingtalk_quoted_card(&value);
    assert_eq!(rendered, "literal [Image] text\n[Image]\nafter");
    assert_eq!(rendered.matches(IMAGE_PLACEHOLDER).count(), 2);
}

/// `||` 的保守策略只作用于**平台给的文本值**：渲染好的引用块整体不过这道闸门。
#[test]
fn readable_quoted_text_only_filters_platform_values() {
    assert_eq!(readable_quoted_text("plain"), "plain");
    assert_eq!(
        readable_quoted_text("a || b"),
        "[quoted content unavailable]"
    );
    assert_eq!(readable_quoted_text(""), "");
}

/// URL 跨度：终止符 / 配对括号 / 不配对括号三种形态。
#[test]
fn web_url_spans_terminate_like_upstream() {
    fn only_span(body: &str) -> &str {
        let spans = web_url_spans(body);
        assert_eq!(spans.len(), 1, "body = {body:?}");
        &body[spans[0].0..spans[0].1]
    }
    // 配对括号的整个查询串是一个跨度。
    assert_eq!(
        only_span("see https://example.com/a_(b)?x=a_b+c&y=2#part_2 now"),
        "https://example.com/a_(b)?x=a_b+c&y=2#part_2"
    );
    // 空白终止。
    assert_eq!(
        only_span("https://example.com/x y"),
        "https://example.com/x"
    );
    // 不配对的闭合括号就地终止。
    assert_eq!(
        only_span("https://example.com/a)b"),
        "https://example.com/a"
    );
    // `www.` 也是起点；大小写不敏感，且原拼写保留。
    assert_eq!(only_span("WWW.Example.com/x"), "WWW.Example.com/x");
    // 终止符集里除空白外还有控制字符与 `<>"\``。
    assert_eq!(only_span("http://example.com/a<b"), "http://example.com/a");
    assert!(web_url_spans("no link here").is_empty());
}

/// `msgtype=picture` 的两个下载码：字符串 ok、缺席 = 空、非字符串 = 解码失败。
#[test]
fn picture_content_decodes_like_upstream() {
    let both = picture_content(&json!({"downloadCode": "a", "pictureDownloadCode": "b"}))
        .expect("both codes are strings");
    assert_eq!(both.download_code, "a");
    assert_eq!(both.picture_download_code, "b");

    let missing = picture_content(&json!({"downloadCode": "a"})).expect("absent code is empty");
    assert_eq!(missing.download_code, "a");
    assert!(missing.picture_download_code.is_empty());

    let null = picture_content(&json!({"downloadCode": null})).expect("null is an empty code");
    assert!(null.download_code.is_empty());

    assert!(picture_content(&json!({"downloadCode": 42})).is_none());
    assert!(picture_content(&json!("not-an-object")).is_none());
    assert!(picture_content(&Value::Null).is_none());
}
