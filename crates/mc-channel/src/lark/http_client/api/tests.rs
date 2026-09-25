//! 端点面的用例（wire 形状：路径 / 查询 / 请求体 / 解码）。
//!
//! 替身与装置在父测试模块（[`crate::lark::http_client::tests`]）里 —— 三个测试模块共享
//! 同一份 raw-TCP 替身，所以它只写一次。

use crate::lark::client::{ApiClient, ApiError, ErrorClass};
use crate::lark::http_client::tests::{
    client, credentials, json_reply, recorded, serve, token_reply,
};
use crate::lark::params::{
    AddReactionParams, DeleteReactionParams, ListMessagesParams, PatchCardParams, ReplyTarget,
    SendMarkdownCardParams, SendTextParams, BOT_INFO_PATH, CONTACT_USERS_BATCH_PATH,
};
use crate::lark::types::ChatId;

#[tokio::test]
async fn send_text_encodes_the_double_encoded_text_envelope() {
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(200, r#"{"code":0,"data":{"message_id":"om_1"}}"#),
    ])
    .await;
    let client = client(&base);
    let params = SendTextParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        text: "line1\n\"quoted\" 中文".to_string(),
        reply_target: ReplyTarget::default(),
    };
    assert_eq!(
        client.send_text_message(params).await.expect("sent"),
        "om_1"
    );

    let requests = recorded(&records);
    assert_eq!(
        requests[1].target(),
        "/open-apis/im/v1/messages?receive_id_type=chat_id"
    );
    assert_eq!(requests[1].bearer(), Some("t1"));
    let body = requests[1].json();
    assert_eq!(body["receive_id"], "oc_1");
    assert_eq!(body["msg_type"], "text");
    // `content` 是**字符串**，其内容是再编码一层的 `{"text": …}`。
    let content: serde_json::Value =
        serde_json::from_str(body["content"].as_str().expect("content is a string"))
            .expect("decode inner");
    assert_eq!(content["text"], "line1\n\"quoted\" 中文");
}

#[tokio::test]
async fn a_threaded_markdown_card_goes_through_the_reply_endpoint() {
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(200, r#"{"code":0,"data":{"message_id":"om_2"}}"#),
    ])
    .await;
    let client = client(&base);
    let params = SendMarkdownCardParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        markdown: "# 标题\n\n- a\n- b".to_string(),
        summary: "预览".to_string(),
        reply_target: ReplyTarget {
            message_id: "om_parent".to_string(),
            in_thread: true,
        },
    };
    assert_eq!(
        client.send_markdown_card(params).await.expect("sent"),
        "om_2"
    );

    let requests = recorded(&records);
    assert_eq!(
        requests[1].target(),
        "/open-apis/im/v1/messages/om_parent/reply"
    );
    let body = requests[1].json();
    assert_eq!(body["msg_type"], "interactive");
    assert_eq!(body["reply_in_thread"], true);
    let card: serde_json::Value =
        serde_json::from_str(body["content"].as_str().expect("card json")).expect("decode card");
    assert_eq!(card["schema"], "2.0");
    assert_eq!(card["body"]["elements"][0]["tag"], "markdown");
    assert_eq!(card["body"]["elements"][0]["content"], "# 标题\n\n- a\n- b");
    assert_eq!(card["config"]["summary"]["content"], "预览");
}

#[tokio::test]
async fn a_markdown_card_without_a_summary_omits_the_config_block() {
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(200, r#"{"code":0,"data":{"message_id":"om_3"}}"#),
    ])
    .await;
    let client = client(&base);
    let params = SendMarkdownCardParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        markdown: "plain".to_string(),
        summary: String::new(),
        reply_target: ReplyTarget::default(),
    };
    assert_eq!(
        client.send_markdown_card(params).await.expect("sent"),
        "om_3"
    );
    let body = recorded(&records)[1].json();
    let card: serde_json::Value =
        serde_json::from_str(body["content"].as_str().expect("card json")).expect("decode card");
    assert!(card.get("config").is_none());
}

#[tokio::test]
async fn missing_required_fields_are_rejected_before_any_request() {
    let (base, records) = serve(vec![]).await;
    let client = client(&base);

    let error = client
        .send_text_message(SendTextParams {
            credentials: credentials(),
            chat_id: ChatId::new(""),
            text: "hi".to_string(),
            reply_target: ReplyTarget::default(),
        })
        .await
        .expect_err("missing chat_id");
    assert_eq!(
        error,
        ApiError::InvalidRequest {
            op: "send text message",
            reason: "missing chat_id"
        }
    );
    let error = client
        .send_markdown_card(SendMarkdownCardParams {
            credentials: credentials(),
            chat_id: ChatId::new("oc_1"),
            markdown: String::new(),
            summary: String::new(),
            reply_target: ReplyTarget::default(),
        })
        .await
        .expect_err("missing markdown");
    assert_eq!(error.class(), ErrorClass::Malformed);

    let error = client
        .get_message(credentials(), "")
        .await
        .expect_err("missing message_id");
    assert_eq!(
        error,
        ApiError::InvalidRequest {
            op: "get message",
            reason: "missing message_id"
        }
    );
    assert!(recorded(&records).is_empty());
}

#[tokio::test]
async fn patch_and_reactions_use_the_upstream_paths() {
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(200, r#"{"code":0}"#),
        json_reply(200, r#"{"code":0,"data":{"reaction_id":"r_1"}}"#),
        json_reply(200, r#"{"code":0}"#),
    ])
    .await;
    let client = client(&base);

    client
        .patch_interactive_card(PatchCardParams {
            credentials: credentials(),
            card_message_id: "om_1".to_string(),
            card_json: r#"{"schema":"2.0"}"#.to_string(),
        })
        .await
        .expect("patched");
    let reaction_id = client
        .add_message_reaction(AddReactionParams {
            credentials: credentials(),
            message_id: "om_1".to_string(),
            emoji_type: "Typing".to_string(),
        })
        .await
        .expect("reaction");
    assert_eq!(reaction_id, "r_1");
    client
        .delete_message_reaction(DeleteReactionParams {
            credentials: credentials(),
            message_id: "om_1".to_string(),
            reaction_id: "r_1".to_string(),
        })
        .await
        .expect("deleted");

    let requests = recorded(&records);
    assert_eq!(requests[1].method(), "PATCH");
    assert_eq!(requests[1].target(), "/open-apis/im/v1/messages/om_1");
    // Lark 的 patch 端点**整卡替换**：体里只有 `content`。
    assert_eq!(requests[1].json()["content"], r#"{"schema":"2.0"}"#);

    assert_eq!(requests[2].method(), "POST");
    assert_eq!(
        requests[2].target(),
        "/open-apis/im/v1/messages/om_1/reactions"
    );
    assert_eq!(requests[2].json()["reaction_type"]["emoji_type"], "Typing");

    assert_eq!(requests[3].method(), "DELETE");
    assert_eq!(
        requests[3].target(),
        "/open-apis/im/v1/messages/om_1/reactions/r_1"
    );
    // 删表态那条**不带体**。
    assert!(requests[3].body.is_empty());
}

#[tokio::test]
async fn get_message_and_list_decode_the_item_arrays() {
    let items = r#"{"code":0,"data":{"items":[
        {"message_id":"om_a","msg_type":"text","create_time":"1700000000000",
         "sender":{"id":"ou_1","sender_type":"user"},
         "body":{"content":"{\"text\":\"a\"}"},
         "mentions":[{"key":"@_user_1","id":"ou_2","name":"B"}]},
        {"message_id":"om_b","msg_type":"merge_forward"}
    ]}}"#;
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(200, items),
        json_reply(200, items),
    ])
    .await;
    let client = client(&base);

    let messages = client
        .get_message(credentials(), "om_a")
        .await
        .expect("get");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].message_id, "om_a");
    assert_eq!(messages[0].sender_id, "ou_1");
    assert_eq!(messages[0].mentions[0].name, "B");

    let listed = client
        .list_chat_messages(
            credentials(),
            ListMessagesParams {
                chat_id: ChatId::new("oc_1"),
                thread_id: String::new(),
                page_size: 10,
                end_time: 1_700_000_000,
            },
        )
        .await
        .expect("list");
    assert_eq!(listed.len(), 2);

    let requests = recorded(&records);
    assert_eq!(
        requests[1].target(),
        "/open-apis/im/v1/messages/om_a?user_id_type=open_id"
    );
    assert!(requests[2].target().contains("container_id_type=chat"));
    assert!(requests[2].target().contains("end_time=1700000000"));
}

#[tokio::test]
async fn bot_info_resolves_the_union_id_in_a_second_call() {
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(200, r#"{"code":0,"bot":{"open_id":"ou_bot"}}"#),
        json_reply(200, r#"{"code":0,"data":{"user":{"union_id":"on_bot"}}}"#),
    ])
    .await;
    let client = client(&base);
    let info = client.get_bot_info(credentials()).await.expect("bot info");
    assert_eq!(info.open_id.as_str(), "ou_bot");
    assert_eq!(info.union_id, "on_bot");
    assert!(info.has_union_id());

    let requests = recorded(&records);
    assert_eq!(requests[1].target(), BOT_INFO_PATH);
    assert_eq!(
        requests[2].target(),
        "/open-apis/contact/v3/users/ou_bot?user_id_type=open_id"
    );
}

#[tokio::test]
async fn bot_info_soft_fails_when_the_union_id_lookup_is_denied() {
    // 通讯录范围受限：安装**仍然可用**（p2p 不受影响）⇒ 记警告、继续。
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(200, r#"{"code":0,"bot":{"open_id":"ou_bot"}}"#),
        json_reply(403, r#"{"code":99991672,"msg":"no contact scope"}"#),
    ])
    .await;
    let client = client(&base);
    let info = client
        .get_bot_info(credentials())
        .await
        .expect("软失败不该让安装不可用");
    assert_eq!(info.open_id.as_str(), "ou_bot");
    assert_eq!(info.union_id, "");
    assert!(!info.has_union_id());
    assert_eq!(recorded(&records).len(), 3);
}

#[tokio::test]
async fn batch_get_users_maps_the_ids_the_api_actually_returns() {
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(
            200,
            r#"{"code":0,"data":{"items":[
                {"open_id":"ou_1","name":"A"},
                {"open_id":"ou_2","name":""}
            ]}}"#,
        ),
    ])
    .await;
    let client = client(&base);
    let names = client
        .batch_get_users(credentials(), vec!["ou_1".to_string(), "ou_2".to_string()])
        .await
        .expect("batch");
    assert_eq!(names.len(), 1, "没名字的那些**不**进映射（上游同款降级）");
    assert_eq!(names.get("ou_1").map(String::as_str), Some("A"));

    let requests = recorded(&records);
    assert!(requests[1].target().starts_with(CONTACT_USERS_BATCH_PATH));
    assert!(requests[1].target().contains("user_id_type=open_id"));
    assert!(requests[1].target().contains("user_ids=ou_1"));
}

#[tokio::test]
async fn batch_get_users_with_no_ids_makes_no_request() {
    let (base, records) = serve(vec![]).await;
    let client = client(&base);
    let names = client
        .batch_get_users(credentials(), vec![])
        .await
        .expect("empty");
    assert!(names.is_empty());
    assert!(recorded(&records).is_empty());
}

// =====================================================================
// 资源下载（含上限与 JSON 错误体）
// =====================================================================
