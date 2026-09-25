//! `replier/tests.rs` 的**文案与落点**用例：`/issue` 的两种产品结果 / 深链 / 消毒 / 回复目标。
//!
//! 拆出本文件是**门 ⑩**（单文件 800 行硬限）的要求；切点与 `replier/text.rs` 一致
//! （纯函数那一半）。共用装置在 [`super`]。

use uuid::Uuid;

use super::{group_message, inst, replier, INST};
use crate::lark::replier::text::{
    inbound_reply_target, inbound_reply_target_of_binding, issue_result_identifier,
    issue_title_sanitized,
};
use crate::lark::replier::Outcome;
use crate::lark::resolvers::DispatchResult;
use crate::lark::tests::support::inbound_message;
use crate::lark::tests::support::Call;
use crate::lark::tests::support::{FakeApi, FakeMinter};
use crate::lark::types::ChatType;
use mc_core::id::Id;

// ---------------------------------------------------------------------
// `/issue` 的产品结果（上游 `sendIssueOutcome`）
// ---------------------------------------------------------------------

fn issue_result(duplicate: bool) -> DispatchResult {
    DispatchResult {
        outcome: Outcome::Ingested,
        sender_open_id: "ou_sender".to_string(),
        issue_id: Some(Id(Uuid::from_u128(0x1234))),
        issue_number: 42,
        issue_identifier: "ABC-42".to_string(),
        issue_workspace_slug: "acme".to_string(),
        issue_title: "Fix the thing".to_string(),
        issue_duplicate: duplicate,
        ..DispatchResult::default()
    }
}

/// 新建 / 重复两条文案 + 深链。
#[tokio::test]
async fn issue_outcomes_send_the_created_or_duplicate_text_with_a_deep_link() {
    for duplicate in [false, true] {
        let api = FakeApi::new();
        let minter = FakeMinter::new();
        let replier = replier(&api, &minter, "https://app.example");
        replier
            .reply_now(&inst(), &group_message(), &issue_result(duplicate))
            .await;

        match &api.log()[0] {
            Call::SendText { text, .. } => {
                assert!(text.contains("ABC-42"));
                assert!(text.contains("Fix the thing"));
                assert!(
                    text.contains("https://app.example/acme/issues/ABC-42"),
                    "{text}"
                );
                if duplicate {
                    assert!(text.starts_with("Not created — active issue ABC-42"));
                } else {
                    assert!(text.starts_with("Created ABC-42 — Fix the thing"));
                }
            }
            other => panic!("expected SendText, got {other:?}"),
        }
    }
}

/// 没有标识符 ⇒ 显示 `#<number>`（降级标签），**深链仍然只用 `issue_identifier`**
/// （所以这里没有链接）。
#[tokio::test]
async fn a_missing_identifier_degrades_the_label_but_never_the_link() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    let mut result = issue_result(false);
    result.issue_identifier = String::new();
    replier.reply_now(&inst(), &group_message(), &result).await;

    match &api.log()[0] {
        Call::SendText { text, .. } => {
            assert!(text.starts_with("Created #42"), "{text}");
            assert!(
                !text.contains("http"),
                "没有可路由的标识符 ⇒ 不带链接: {text}"
            );
        }
        other => panic!("expected SendText, got {other:?}"),
    }
}

/// 没配 app url ⇒ 文案仍然确认，只是没有可点的深链。
#[tokio::test]
async fn no_app_url_still_confirms_without_a_link() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "");
    replier
        .reply_now(&inst(), &group_message(), &issue_result(false))
        .await;
    match &api.log()[0] {
        Call::SendText { text, .. } => {
            assert!(text.starts_with("Created ABC-42"));
            assert!(!text.contains("http"));
        }
        other => panic!("expected SendText, got {other:?}"),
    }
}

/// 标题消毒：**先**拆链接邻接、**再**转义 `<`（顺序不能反）。
#[test]
fn issue_title_sanitization_orders_the_two_steps() {
    let with_lt = DispatchResult {
        issue_title: "  a < b  ".to_string(),
        ..issue_result(false)
    };
    assert_eq!(issue_title_sanitized(&with_lt), "a &lt; b");

    // 裸 `<` 变 `&lt;`；由消毒产生的 `&lt;` **不得**被二次转义。
    let already = DispatchResult {
        issue_title: "x &lt; y".to_string(),
        ..issue_result(false)
    };
    assert_eq!(issue_title_sanitized(&already), "x &lt; y");

    let empty = DispatchResult {
        issue_title: "   ".to_string(),
        ..issue_result(false)
    };
    assert_eq!(issue_title_sanitized(&empty), "");
}

/// 没有标题 ⇒ 只有标识符（上游 `line = "Created %s"`）。
#[tokio::test]
async fn an_issue_without_a_title_renders_only_the_identifier() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    let mut result = issue_result(false);
    result.issue_title = String::new();
    replier.reply_now(&inst(), &group_message(), &result).await;
    match &api.log()[0] {
        Call::SendText { text, .. } => {
            assert!(text.starts_with("Created ABC-42\n"), "{text}");
            assert!(!text.contains("—"));
        }
        other => panic!("expected SendText, got {other:?}"),
    }

    // 纯函数面：标识符回落。
    let mut no_identifier = issue_result(false);
    no_identifier.issue_identifier = String::new();
    assert_eq!(issue_result_identifier(&no_identifier), "#42");
    assert_eq!(issue_result_identifier(&issue_result(false)), "ABC-42");
}

/// 没有 `chat_id` ⇒ 不发、不 panic。
#[tokio::test]
async fn an_issue_outcome_without_a_chat_id_is_not_sent() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    let mut message = group_message();
    message.source.chat_id = String::new();
    replier
        .reply_now(&inst(), &message, &issue_result(false))
        .await;
    assert!(api.log().is_empty());
}

// ---------------------------------------------------------------------
// 回复目标（上游 `inboundReplyTarget`）
// ---------------------------------------------------------------------

/// 三档与 patcher 的 `threadReplyTarget` **逐条对齐**（上游逐字：两边不得放得不一样）。
#[test]
fn inbound_reply_target_has_the_same_three_tiers_as_the_patcher() {
    let group = inbound_message("oc_main", ChatType::Group, "om_1", "", "");
    let target = inbound_reply_target(&group);
    assert_eq!(target.message_id, "om_1");
    assert!(!target.in_thread);

    let topic = inbound_message("oc_main", ChatType::Group, "om_1", "t_1", "");
    let target = inbound_reply_target(&topic);
    assert!(target.in_thread, "话题里留在话题内");

    let p2p = inbound_message("oc_dm", ChatType::P2p, "om_1", "", "");
    assert!(!inbound_reply_target(&p2p).is_set(), "1:1 不回帖");

    let mut without_id = group.clone();
    without_id.message_id = String::new();
    assert!(!inbound_reply_target(&without_id).is_set());
}

/// 绑定行的形态给出**同一个**目标（两处判决一致）。
#[test]
fn the_binding_form_of_the_reply_target_matches() {
    use crate::lark::tests::support::delivery_row;
    let binding = crate::lark::store::ChatSessionBinding::from_delivery_row(&delivery_row(
        Id(Uuid::from_u128(0x4000)),
        Id(Uuid::from_u128(0x5000)),
        Id(Uuid::from_u128(INST)),
        "oc_main",
        ChatType::Group,
        Some("om_1"),
        None,
        serde_json::json!({}),
    ));
    assert_eq!(
        inbound_reply_target_of_binding(&binding),
        inbound_reply_target(&inbound_message("oc_main", ChatType::Group, "om_1", "", ""))
    );
}
