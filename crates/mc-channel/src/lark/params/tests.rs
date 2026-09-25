//! `params.rs` 的用例：**纯函数**（路径转义与构建、请求体、绑定卡模板）以及**凭据脱敏**。
//!
//! 上游 `http_client.go` 的 `outboundMessageRequest` / `bindingPromptTemplate` / 三条路径构建
//! 逐条钉；凭据面按本片 `DoD` 第 6 条钉"任何 `Debug` 路径都拿不到明文"。

use crate::lark::types::DropReason;

use super::*;

// =====================================================================
// 路径转义（上游 `url.PathEscape` 的等价物）
// =====================================================================

#[test]
fn escape_path_segment_passes_unreserved_bytes_through() {
    // Lark 的 id 实际都是 unreserved 字符（`om_` / `ou_` / `cli_` / 数字）。
    assert_eq!(escape_path_segment("om_1234-abcd.ef"), "om_1234-abcd.ef");
    assert_eq!(escape_path_segment(""), "");
    assert_eq!(escape_path_segment("a~b"), "a~b");
}

#[test]
fn escape_path_segment_escapes_reserved_bytes_and_utf8() {
    assert_eq!(escape_path_segment("a/b"), "a%2Fb");
    assert_eq!(escape_path_segment("a b"), "a%20b");
    assert_eq!(escape_path_segment("a?b=c&d"), "a%3Fb%3Dc%26d");
    assert_eq!(escape_path_segment("100%"), "100%25");
    // 非 ASCII 按 **UTF-8 逐字节**转义（与 Go 的 `PathEscape` 同款）。
    assert_eq!(escape_path_segment("中"), "%E4%B8%AD");
}

// =====================================================================
// 路径构建（上游 `http_client.go` 的逐字路径）
// =====================================================================

#[test]
fn path_builders_match_the_upstream_literals() {
    assert_eq!(message_path("om_1"), "/open-apis/im/v1/messages/om_1");
    assert_eq!(reply_path("om_1"), "/open-apis/im/v1/messages/om_1/reply");
    assert_eq!(
        resource_path("om_1", "file_1"),
        "/open-apis/im/v1/messages/om_1/resources/file_1"
    );
    assert_eq!(
        reactions_path("om_1"),
        "/open-apis/im/v1/messages/om_1/reactions"
    );
    assert_eq!(
        reaction_path("om_1", "r_1"),
        "/open-apis/im/v1/messages/om_1/reactions/r_1"
    );
    assert_eq!(
        contact_user_path("ou_1"),
        "/open-apis/contact/v3/users/ou_1"
    );
    // 路径段里的保留字符被转义（否则会打到另一个端点）。
    assert_eq!(
        message_path("om_1/../x"),
        "/open-apis/im/v1/messages/om_1%2F..%2Fx"
    );
}

#[test]
fn wire_constants_are_the_upstream_literals() {
    assert_eq!(
        TENANT_ACCESS_TOKEN_PATH,
        "/open-apis/auth/v3/tenant_access_token/internal"
    );
    assert_eq!(MESSAGES_PATH, "/open-apis/im/v1/messages");
    assert_eq!(BOT_INFO_PATH, "/open-apis/bot/v3/info");
    assert_eq!(CONTACT_USERS_PATH, "/open-apis/contact/v3/users");
    assert_eq!(
        CONTACT_USERS_BATCH_PATH,
        "/open-apis/contact/v3/users/batch"
    );
}

// =====================================================================
// 出站消息请求（上游 `outboundMessageRequest`）
// =====================================================================

#[test]
fn chat_level_send_uses_the_chat_container_and_a_receive_id() {
    let (path, body) = outbound_message_request(
        &ChatId::new("oc_1"),
        "text",
        "{\"text\":\"hi\"}",
        &ReplyTarget::default(),
    );
    assert_eq!(path, "/open-apis/im/v1/messages?receive_id_type=chat_id");
    assert_eq!(body["receive_id"], "oc_1");
    assert_eq!(body["msg_type"], "text");
    assert_eq!(body["content"], "{\"text\":\"hi\"}");
    // 会话级发送**不带** `reply_in_thread`（上游同款）。
    assert!(body.get("reply_in_thread").is_none());
}

#[test]
fn threaded_send_routes_through_the_reply_endpoint() {
    let target = ReplyTarget {
        message_id: "om_parent".to_string(),
        in_thread: true,
    };
    assert!(target.is_set());
    let (path, body) = outbound_message_request(&ChatId::new("oc_1"), "interactive", "{}", &target);
    assert_eq!(path, "/open-apis/im/v1/messages/om_parent/reply");
    assert_eq!(body["msg_type"], "interactive");
    assert_eq!(body["content"], "{}");
    // `reply_in_thread` 是**布尔**（所以 body 不是 map[string]string）。
    assert_eq!(body["reply_in_thread"], true);
    // 走回复端点时**不带** `receive_id`（上游同款）。
    assert!(body.get("receive_id").is_none());
}

#[test]
fn reply_target_is_only_set_when_a_parent_message_exists() {
    assert!(!ReplyTarget::default().is_set());
    assert!(!ReplyTarget {
        message_id: String::new(),
        in_thread: true,
    }
    .is_set());
    assert!(ReplyTarget {
        message_id: "om_1".to_string(),
        in_thread: false,
    }
    .is_set());
}

// =====================================================================
// 消息列表查询（上游 `ListChatMessages` 的查询构建）
// =====================================================================

#[test]
fn chat_container_query_carries_the_end_time_when_set() {
    let path = list_messages_request(&ListMessagesParams {
        chat_id: ChatId::new("oc_1"),
        thread_id: String::new(),
        page_size: 10,
        end_time: 1_700_000_000,
    });
    assert!(path.starts_with("/open-apis/im/v1/messages?"));
    assert!(path.contains("container_id_type=chat"));
    assert!(path.contains("container_id=oc_1"));
    assert!(path.contains("end_time=1700000000"));
    assert!(path.contains("sort_type=ByCreateTimeDesc"));
    assert!(path.contains("page_size=10"));
    assert!(path.contains("user_id_type=open_id"));
}

#[test]
fn chat_container_query_omits_a_zero_end_time() {
    let path = list_messages_request(&ListMessagesParams {
        chat_id: ChatId::new("oc_1"),
        thread_id: String::new(),
        page_size: 5,
        end_time: 0,
    });
    assert!(!path.contains("end_time"));
}

#[test]
fn thread_container_query_drops_the_end_time() {
    // 话题容器**不接受** `end_time`（上游 #5835 的逐字注释）⇒ 带上它会让 Lark 回 400。
    let path = list_messages_request(&ListMessagesParams {
        chat_id: ChatId::new("oc_1"),
        thread_id: "omt_1".to_string(),
        page_size: 20,
        end_time: 1_700_000_000,
    });
    assert!(path.contains("container_id_type=thread"));
    assert!(path.contains("container_id=omt_1"));
    assert!(!path.contains("end_time"));
    // 话题容器下 `chat_id` 只作为参数存在，不作 container。
    assert!(!path.contains("container_id=oc_1"));
}

#[test]
fn page_size_is_clamped_into_larks_single_page_window() {
    let clamp = |size: usize| {
        list_messages_request(&ListMessagesParams {
            chat_id: ChatId::new("oc_1"),
            page_size: size,
            ..ListMessagesParams::default()
        })
    };
    assert!(clamp(0).contains("page_size=1"));
    assert!(clamp(1).contains("page_size=1"));
    assert!(clamp(50).contains("page_size=50"));
    // 超过 Lark 的硬上限 ⇒ **静默取上限**（不是让 Lark 回 400）。
    assert!(clamp(999).contains("page_size=50"));
    assert_eq!(MAX_LIST_MESSAGES_PAGE_SIZE, 50);
}

// =====================================================================
// 批量查用户（上游 `BatchGetUsers` 的查询构建）
// =====================================================================

#[test]
fn batch_query_skips_empty_ids_and_truncates_at_the_cap() {
    let query = batch_get_users_query(&["ou_1".to_string(), String::new(), "ou_2".to_string()]);
    assert!(query.starts_with("?user_id_type=open_id"));
    assert!(query.contains("&user_ids=ou_1"));
    assert!(query.contains("&user_ids=ou_2"));
    assert_eq!(query.matches("user_ids=").count(), 2);

    let many: Vec<String> = (0..60).map(|index| format!("ou_{index}")).collect();
    let query = batch_get_users_query(&many);
    assert_eq!(query.matches("user_ids=").count(), MAX_BATCH_GET_USERS_IDS);
    assert_eq!(MAX_BATCH_GET_USERS_IDS, 50);
    // 超出部分**丢弃**（上游口径：不报错）。
    assert!(!query.contains("ou_59"));
    assert!(query.contains("ou_49"));
}

// =====================================================================
// 绑定卡模板（上游 `bindingPromptTemplate`）
// =====================================================================

#[test]
fn binding_prompt_card_is_a_single_cta_with_the_bind_url() {
    let raw = binding_prompt_card("https://app.example/bind?t=abc");
    let card: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
    assert_eq!(card["config"]["wide_screen_mode"], true);
    assert_eq!(card["header"]["title"]["content"], "Multica");
    let elements = card["elements"].as_array().expect("elements");
    assert_eq!(elements.len(), 2);
    assert_eq!(elements[0]["tag"], "div");
    assert_eq!(elements[0]["text"]["tag"], "lark_md");
    let action = &elements[1]["actions"][0];
    assert_eq!(action["tag"], "button");
    assert_eq!(action["url"], "https://app.example/bind?t=abc");
    assert_eq!(action["type"], "primary");
}

// =====================================================================
// 凭据面（本片 DoD 第 6 条：任何 `Debug` 路径都拿不到明文）
// =====================================================================

/// 明文 secret 的字面量（用例里到处用它去断言"没漏出去"）。
const PLAINTEXT: &str = "super-secret-app-secret-value";

fn credentials() -> InstallationCredentials {
    InstallationCredentials::new("cli_test_app", AppSecret::new(PLAINTEXT))
        .with_tenant_key("tk_1")
        .with_region(Region::Lark)
}

#[test]
fn app_secret_debug_is_always_redacted() {
    let secret = AppSecret::new(PLAINTEXT);
    let rendered = format!("{secret:?}");
    assert_eq!(rendered, "AppSecret(<redacted>)");
    assert!(!rendered.contains(PLAINTEXT));
    // `Display` 不存在 ⇒ 不存在"另一条格式化路径"（编译期保证）。
    assert_eq!(secret.expose(), PLAINTEXT);
    assert!(!secret.is_empty());
    assert!(AppSecret::default().is_empty());
}

#[test]
fn installation_credentials_debug_redacts_the_secret_only() {
    let rendered = format!("{:?}", credentials());
    assert!(rendered.contains("<redacted>"));
    assert!(!rendered.contains(PLAINTEXT));
    // 非秘密字段仍然可诊断（上游日志逐字打印 `app_id`）。
    assert!(rendered.contains("cli_test_app"));
    assert!(rendered.contains("tk_1"));
    assert!(rendered.contains("Lark"));
}

#[test]
fn every_request_param_struct_redacts_the_secret_in_its_debug() {
    // 注意：这几个结构是**派生** `Debug` 的 —— 它们之所以安全，正是因为
    // `InstallationCredentials` 手写了 `Debug`。用例钉住这条传递性。
    let params = SendCardParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        card_json: "{}".to_string(),
        reply_target: ReplyTarget::default(),
    };
    assert!(!format!("{params:?}").contains(PLAINTEXT));

    let text = SendTextParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        text: "hi".to_string(),
        reply_target: ReplyTarget::default(),
    };
    assert!(!format!("{text:?}").contains(PLAINTEXT));

    let patch = PatchCardParams {
        credentials: credentials(),
        card_message_id: "om_1".to_string(),
        card_json: "{}".to_string(),
    };
    assert!(!format!("{patch:?}").contains(PLAINTEXT));

    let markdown = SendMarkdownCardParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        markdown: "# hi".to_string(),
        summary: "hi".to_string(),
        reply_target: ReplyTarget::default(),
    };
    assert!(!format!("{markdown:?}").contains(PLAINTEXT));

    let binding = BindingPromptParams {
        credentials: credentials(),
        open_id: OpenId::new("ou_1"),
        bind_url: "https://app.example/bind".to_string(),
    };
    assert!(!format!("{binding:?}").contains(PLAINTEXT));

    let reaction = AddReactionParams {
        credentials: credentials(),
        message_id: "om_1".to_string(),
        emoji_type: "Typing".to_string(),
    };
    assert!(!format!("{reaction:?}").contains(PLAINTEXT));

    let delete = DeleteReactionParams {
        credentials: credentials(),
        message_id: "om_1".to_string(),
        reaction_id: "r_1".to_string(),
    };
    assert!(!format!("{delete:?}").contains(PLAINTEXT));
}

#[test]
fn credentials_completeness_needs_both_halves() {
    assert!(credentials().is_complete());
    assert!(!InstallationCredentials::new("", AppSecret::new(PLAINTEXT)).is_complete());
    assert!(!InstallationCredentials::new("cli_x", AppSecret::default()).is_complete());
    // 默认 region 是飞书（历史行口径）；`with_region` 才切到 Lark。
    assert_eq!(
        InstallationCredentials::new("cli_x", AppSecret::new("s")).region,
        Region::Feishu
    );
    assert_eq!(
        InstallationCredentials::new("cli_x", AppSecret::new("s"))
            .with_region(Region::Lark)
            .region,
        Region::Lark
    );
}

// =====================================================================
// DB 参数形状（上游 `params.go`；本仓用 `Id` / `Option` / `Timestamp` 对齐 pgx 类型）
// =====================================================================

#[test]
fn db_param_shapes_keep_the_nullable_fields_nullable() {
    // `pgtype.Text` ⇄ `Option<String>`：`None` 是 SQL `NULL`，**不是**空串。
    let upsert = UpsertInstallationParams {
        workspace_id: Id::new(),
        agent_id: Id::new(),
        app_id: "cli_x".to_string(),
        app_secret_encrypted: vec![1, 2, 3],
        bot_open_id: "ou_bot".to_string(),
        installer_user_id: Id::new(),
        tenant_key: None,
        bot_union_id: None,
        region: Region::Feishu.as_str().to_string(),
    };
    assert!(upsert.tenant_key.is_none());
    assert!(upsert.bot_union_id.is_none());

    let set_union = SetInstallationBotUnionIdParams {
        id: Id::new(),
        bot_union_id: Some("on_1".to_string()),
    };
    assert_eq!(set_union.bot_union_id.as_deref(), Some("on_1"));

    // 无安装的丢弃审计行用 `Id::nil()`（上游"可能无效的 UUID"）。
    let drop = RecordInboundDropParams {
        event_type: "im.message.receive_v1".to_string(),
        drop_reason: DropReason::Duplicate.as_str().to_string(),
        installation_id: Id::nil(),
        channel_chat_id: None,
        channel_event_id: None,
        channel_message_id: Some("om_1".to_string()),
    };
    assert!(drop.installation_id.is_nil());
    assert!(drop.channel_message_id.is_some());

    // `pgtype.Timestamptz` ⇄ `mc_core::timestamp::Timestamp`。
    let token = CreateBindingTokenParams {
        token_hash: "hash".to_string(),
        workspace_id: Id::new(),
        installation_id: Id::new(),
        channel_user_id: "ou_1".to_string(),
        expires_at: Timestamp::now(),
    };
    assert!(token.expires_at.as_unix() > 0);
}
