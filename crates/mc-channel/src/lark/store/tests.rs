//! `store.rs` 的用例（M7-13）：密文 / config 边界 + 两族的扁平行投影 + 卡片状态词表。
//!
//! 全部是纯函数或结构投影 ⇒ **不需要 DB、不需要网络**（本 crate 没有 `sqlx` 依赖）。

use chrono::{DateTime, TimeZone as _, Utc};
use mc_repos::channel::binding::{LarkBindingTokenRow, LarkUserBindingRow};
use mc_repos::channel::dedup::InboundDedupRow;
use mc_repos::channel::delivery::ChannelTaskDeliveryRow;
use mc_repos::channel::inbound_audit::LarkInboundAuditRow;
use mc_repos::channel::outbound::ChannelOutboundCardMessageRow;
use mc_repos::channel::session::LarkChatSessionBindingRow;
use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::lark::types::{ChatId, ChatType, DropReason};

// ---------------------------------------------------------------------
// 造行
// ---------------------------------------------------------------------

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().expect("timestamp")
}

fn legacy_binding_row(chat_id: &str, chat_type: &str) -> LarkChatSessionBindingRow {
    LarkChatSessionBindingRow {
        id: Uuid::from_u128(0x1000),
        chat_session_id: Uuid::from_u128(0x2000),
        installation_id: Uuid::from_u128(0x3000),
        lark_chat_id: chat_id.to_string(),
        lark_chat_type: chat_type.to_string(),
        created_at: at(1_700_000_000),
        last_lark_message_id: Some("om_legacy".to_string()),
        last_lark_thread_id: None,
    }
}

fn delivery_row(
    chat_id: &str,
    chat_type: &str,
    message_id: Option<&str>,
    thread_id: Option<&str>,
    config: Json,
) -> ChannelTaskDeliveryRow {
    ChannelTaskDeliveryRow {
        task_id: Uuid::from_u128(0x4000),
        binding_id: Uuid::from_u128(0x5000),
        installation_id: Uuid::from_u128(0x3000),
        channel_type: "feishu".to_string(),
        channel_chat_id: chat_id.to_string(),
        chat_type: chat_type.to_string(),
        channel_message_id: message_id.map(str::to_string),
        channel_thread_id: thread_id.map(str::to_string),
        route_revision: 7,
        config,
        created_at: at(1_700_000_100),
    }
}

// ---------------------------------------------------------------------
// 密文边界（上游 `decodeSecret` / `stripWhitespace`）
// ---------------------------------------------------------------------

/// 无空白时**原样返回**（上游 `strings.ContainsAny` 的快速路径）。
#[test]
fn strip_whitespace_is_a_noop_without_whitespace() {
    let clean = "aGVsbG8=";
    assert_eq!(strip_whitespace(clean), clean);
}

/// MIME 包装（每 76 字符折行 + 可能的空格 / 制表符）能被剥掉。
#[test]
fn strip_whitespace_removes_the_mime_wrapping_set() {
    assert_eq!(strip_whitespace("aGVs\nbG8=\r\n"), "aGVsbG8=");
    assert_eq!(strip_whitespace("aGV s\tbG8="), "aGVsbG8=");
}

/// 空串 ⇒ 空 `Vec`（上游：注册中途、密文还没封 ⇒ 不是错误）。
#[test]
fn decode_secret_of_empty_is_empty_not_an_error() {
    assert_eq!(decode_secret("").expect("empty decodes"), Vec::<u8>::new());
}

/// 明文编码后能解回来。
#[test]
fn decode_secret_round_trips_plain_base64() {
    use base64::Engine as _;
    let raw = b"secret-box-ciphertext";
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
    assert_eq!(decode_secret(&encoded).expect("decodes"), raw.to_vec());
}

/// **SQL 回填的 MIME 包装形态**与 Go 写的无包装形态**都能解**（上游逐字的理由）。
#[test]
fn decode_secret_accepts_the_mime_wrapped_backfill_form() {
    use base64::Engine as _;
    let raw = vec![0x5au8; 96];
    let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);
    // PostgreSQL 的 encode(...,'base64') 每 76 字符插一个 '\n'。
    let wrapped = encoded
        .as_bytes()
        .chunks(76)
        .map(|chunk| std::str::from_utf8(chunk).expect("ascii"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(wrapped, encoded, "夹具必须真的带折行");
    assert_eq!(decode_secret(&wrapped).expect("decodes"), raw);
}

/// 坏 base64 ⇒ **只报长度**（凭据纪律：错误路径不回显密文）。
#[test]
fn decode_secret_reports_only_the_length_on_garbage() {
    let error = decode_secret("not base64 !!!").expect_err("must fail");
    assert_eq!(error.length, "not base64 !!!".len());
    let rendered = error.to_string();
    assert!(rendered.contains(&error.length.to_string()));
    assert!(
        !rendered.contains("not base64"),
        "错误文案不得回显密文本身: {rendered}"
    );
}

// ---------------------------------------------------------------------
// 绑定行的 config 边界（上游 `larkBindingConfig`）
// ---------------------------------------------------------------------

#[test]
fn binding_config_reads_chat_id_and_ignores_every_other_shape() {
    assert_eq!(
        BindingConfig::chat_id_from(&json!({"chat_id": "oc_real"})),
        Some("oc_real".to_string())
    );
    assert_eq!(BindingConfig::chat_id_from(&json!({"chat_id": ""})), None);
    assert_eq!(BindingConfig::chat_id_from(&json!({"chat_id": 12})), None);
    assert_eq!(BindingConfig::chat_id_from(&json!({"other": "oc_x"})), None);
    assert_eq!(BindingConfig::chat_id_from(&Json::Null), None);
    assert_eq!(BindingConfig::chat_id_from(&json!([1, 2])), None);
}

#[test]
fn binding_config_encodes_empty_as_empty_object() {
    assert_eq!(
        BindingConfig::encode("oc_real"),
        json!({"chat_id": "oc_real"})
    );
    assert_eq!(BindingConfig::encode(""), json!({}));
}

// ---------------------------------------------------------------------
// 会话绑定：投递行与绑定行的互补
// ---------------------------------------------------------------------

/// 遗留行给会话身份；投递行给"**这次**触发的消息"与发件人。
#[test]
fn binding_is_composed_from_the_delivery_row_and_then_completed_by_the_legacy_row() {
    let delivery = delivery_row(
        "oc_main",
        "group",
        Some("om_trigger"),
        None,
        json!({"sender_id": "ou_sender", "chat_id": "oc_main"}),
    );
    let binding = ChatSessionBinding::from_delivery_row(&delivery);
    assert_eq!(binding.id, Id(Uuid::from_u128(0x5000)));
    assert_eq!(binding.chat_session_id, None, "投递行没有这一列");
    assert_eq!(binding.last_message_id.as_deref(), Some("om_trigger"));
    assert_eq!(binding.last_sender_id.as_deref(), Some("ou_sender"));
    assert_eq!(binding.outbound_chat_id(), "oc_main");
    assert!(!binding.is_topic_isolated());

    let completed = binding.with_legacy_row(&legacy_binding_row("oc_main", "group"));
    assert_eq!(
        completed.chat_session_id,
        Some(Id(Uuid::from_u128(0x2000))),
        "遗留行补上会话身份"
    );
    assert_eq!(
        completed.last_message_id.as_deref(),
        Some("om_trigger"),
        "补会话身份**不得**覆盖按任务冻结的游标"
    );
}

/// 话题隔离行：`config.chat_id != channel_chat_id` ⇒ 隔离；出站要寻址真实 chat id。
#[test]
fn topic_isolation_is_derived_from_the_config_boundary() {
    let delivery = delivery_row(
        "oc_main#t_1",
        "group",
        Some("om_trigger"),
        Some("t_1"),
        json!({"chat_id": "oc_main"}),
    );
    let binding = ChatSessionBinding::from_delivery_row(&delivery);
    assert!(binding.is_topic_isolated());
    assert_eq!(binding.outbound_chat_id(), "oc_main");

    // 话题之前 / 普通会话：config 是 `{}` ⇒ 不是隔离行，出站就是键本身。
    let plain = ChatSessionBinding::from_delivery_row(&delivery_row(
        "oc_main",
        "group",
        None,
        None,
        json!({}),
    ));
    assert!(!plain.is_topic_isolated());
    assert_eq!(plain.outbound_chat_id(), "oc_main");
    assert_eq!(outbound_chat_id_of(&plain).as_str(), "oc_main");
}

/// `config.chat_id == 键` ⇒ **不**是隔离行（上游 `cfg.ChatID != b.ChannelChatID` 那个判据）。
#[test]
fn config_chat_id_equal_to_the_key_is_not_isolation() {
    let delivery = delivery_row(
        "oc_main",
        "group",
        None,
        None,
        json!({"chat_id": "oc_main"}),
    );
    assert!(!ChatSessionBinding::from_delivery_row(&delivery).is_topic_isolated());
}

/// 遗留行单独用时 config 是 `Null` ⇒ 键就是真实 chat id；游标来自遗留列。
#[test]
fn legacy_row_projection_carries_the_legacy_cursor() {
    let binding = ChatSessionBinding::from_legacy_row(&legacy_binding_row("oc_main", "group"));
    assert_eq!(binding.chat_session_id, Some(Id(Uuid::from_u128(0x2000))));
    assert_eq!(binding.last_message_id.as_deref(), Some("om_legacy"));
    assert_eq!(binding.last_sender_id, None);
    assert_eq!(binding.config, Json::Null);
    assert_eq!(binding.outbound_chat_id(), "oc_main");
}

/// 认不出的 `chat_type` 失败关闭成 p2p（**不**猜成群聊 —— 那会多 `@` 一个人）。
#[test]
fn unknown_chat_type_falls_back_to_p2p() {
    let delivery = delivery_row("oc_main", "weird", None, None, json!({}));
    assert_eq!(
        ChatSessionBinding::from_delivery_row(&delivery).chat_type,
        ChatType::P2p
    );
}

/// `with_delivery` 让投递行**覆盖**绑定表的游标（上游 `processEvent` 的组装）。
#[test]
fn with_delivery_overrides_the_binding_cursor() {
    let binding = ChatSessionBinding::from_legacy_row(&legacy_binding_row("oc_main", "group"));
    let delivery = delivery_row(
        "oc_main",
        "group",
        Some("om_newer"),
        Some("t_9"),
        json!({"sender_id": "ou_who"}),
    );
    let merged = binding.with_delivery(&delivery);
    assert_eq!(merged.last_message_id.as_deref(), Some("om_newer"));
    assert_eq!(merged.last_thread_id.as_deref(), Some("t_9"));
    assert_eq!(merged.last_sender_id.as_deref(), Some("ou_who"));
    assert_eq!(
        merged.chat_session_id,
        Some(Id(Uuid::from_u128(0x2000))),
        "会话身份不被投递行抹掉"
    );
}

// ---------------------------------------------------------------------
// 发件人（`channel_task_delivery.config` 里那一格）
// ---------------------------------------------------------------------

#[test]
fn sender_id_is_read_and_written_without_clobbering_the_chat_id() {
    assert_eq!(
        channel_sender_id(&json!({"sender_id": "ou_a"})),
        Some("ou_a".to_string())
    );
    assert_eq!(channel_sender_id(&json!({"sender_id": ""})), None);
    assert_eq!(channel_sender_id(&json!({})), None);
    assert_eq!(channel_sender_id(&Json::Null), None);

    let merged = with_channel_sender_id(json!({"chat_id": "oc_main"}), "ou_a");
    assert_eq!(merged, json!({"chat_id": "oc_main", "sender_id": "ou_a"}));
    // 空发件人 ⇒ 原样返回（**不写空键**：那会把"没写"与"写成空"混起来）。
    let untouched = with_channel_sender_id(json!({"chat_id": "oc_main"}), "");
    assert_eq!(untouched, json!({"chat_id": "oc_main"}));
    // 非对象 config ⇒ 重建（不 panic、不丢发件人）。
    assert_eq!(
        with_channel_sender_id(Json::Null, "ou_a"),
        json!({"sender_id": "ou_a"})
    );
}

// ---------------------------------------------------------------------
// 卡片状态词表
// ---------------------------------------------------------------------

#[test]
fn card_status_round_trips_and_knows_terminal() {
    for status in [
        CardStatus::Pending,
        CardStatus::Streaming,
        CardStatus::Final,
        CardStatus::Error,
    ] {
        assert_eq!(CardStatus::from_str_opt(status.as_str()), Some(status));
        assert_eq!(status.to_string(), status.as_str());
    }
    assert!(!CardStatus::Pending.is_terminal());
    assert!(!CardStatus::Streaming.is_terminal());
    assert!(CardStatus::Final.is_terminal());
    assert!(CardStatus::Error.is_terminal());
    // 未知状态**不**回落（库里出现第四种状态是数据问题，不该被静默吞掉）。
    assert_eq!(CardStatus::from_str_opt("weird"), None);
    assert_eq!(CardStatus::default(), CardStatus::Pending);
}

#[test]
fn outbound_card_projection_maps_the_row() {
    let row = ChannelOutboundCardMessageRow {
        id: Uuid::from_u128(0x6000),
        chat_session_id: Uuid::from_u128(0x2000),
        task_id: Some(Uuid::from_u128(0x4000)),
        channel_type: "feishu".to_string(),
        channel_chat_id: "oc_main".to_string(),
        channel_card_message_id: "om_card".to_string(),
        status: "final".to_string(),
        last_patched_at: Some(at(1_700_000_200)),
        created_at: at(1_700_000_100),
    };
    let card = OutboundCardMessage::from(&row);
    assert_eq!(card.id, Id(Uuid::from_u128(0x6000)));
    assert_eq!(card.task_id, Some(Id(Uuid::from_u128(0x4000))));
    assert!(card.is_terminal());
    assert_eq!(card.channel_card_message_id, "om_card");

    let pending = OutboundCardMessage::from(&ChannelOutboundCardMessageRow {
        status: "pending".to_string(),
        task_id: None,
        ..row
    });
    assert!(!pending.is_terminal());
    assert_eq!(pending.task_id, None);
}

// ---------------------------------------------------------------------
// 其余扁平行投影
// ---------------------------------------------------------------------

#[test]
fn user_binding_projection_maps_the_legacy_columns() {
    let row = LarkUserBindingRow {
        id: Uuid::from_u128(0x7000),
        workspace_id: Uuid::from_u128(0x8000),
        multica_user_id: Uuid::from_u128(0x9000),
        installation_id: Uuid::from_u128(0x3000),
        lark_open_id: "ou_a".to_string(),
        union_id: Some("on_a".to_string()),
        bound_at: at(1_700_000_000),
    };
    let binding = UserBinding::from(&row);
    assert_eq!(binding.channel_user_id, "ou_a");
    assert_eq!(binding.union_id.as_deref(), Some("on_a"));
    assert_eq!(binding.multica_user_id, Id(Uuid::from_u128(0x9000)));
}

#[test]
fn binding_token_projection_maps_the_legacy_columns() {
    let row = LarkBindingTokenRow {
        token_hash: "hash".to_string(),
        workspace_id: Uuid::from_u128(0x8000),
        installation_id: Uuid::from_u128(0x3000),
        lark_open_id: "ou_a".to_string(),
        expires_at: at(1_700_000_900),
        consumed_at: None,
        created_at: at(1_700_000_000),
    };
    let token = BindingTokenRow::from(&row);
    assert_eq!(token.channel_user_id, "ou_a");
    assert_eq!(token.consumed_at, None);
    assert_eq!(token.token_hash, "hash");
}

#[test]
fn dedup_projection_is_a_flat_copy_of_five_columns() {
    let row = InboundDedupRow {
        installation_id: Uuid::from_u128(0x3000),
        message_id: "om_1".to_string(),
        received_at: at(1_700_000_000),
        processed_at: Some(at(1_700_000_001)),
        claim_token: Uuid::from_u128(0xa000),
    };
    let dedup = InboundMessageDedup::from(&row);
    assert_eq!(dedup.message_id, "om_1");
    assert_eq!(dedup.claim_token, Id(Uuid::from_u128(0xa000)));
    assert_eq!(dedup.processed_at, Some(at(1_700_000_001)));
}

#[test]
fn audit_projection_maps_the_legacy_column_names() {
    let row = LarkInboundAuditRow {
        id: Uuid::from_u128(0xb000),
        installation_id: Some(Uuid::from_u128(0x3000)),
        lark_chat_id: Some("oc_main".to_string()),
        event_type: "im.message.receive_v1".to_string(),
        lark_event_id: None,
        lark_message_id: Some("om_1".to_string()),
        drop_reason: "duplicate".to_string(),
        received_at: at(1_700_000_000),
    };
    let audit = InboundAuditRow::from(&row);
    assert_eq!(audit.channel_chat_id.as_deref(), Some("oc_main"));
    assert_eq!(audit.channel_message_id.as_deref(), Some("om_1"));
    assert_eq!(audit.channel_event_id, None);
    assert_eq!(audit.drop_reason, "duplicate");
}

// ---------------------------------------------------------------------
// 纯工具
// ---------------------------------------------------------------------

/// 空串 ⇒ `None`（上游逐字：别把"事件没有这个字段"与"这个字段故意为空"混起来）。
#[test]
fn text_if_non_empty_keeps_the_missing_versus_empty_distinction() {
    assert_eq!(text_if_non_empty(""), None);
    assert_eq!(text_if_non_empty("oc_main"), Some("oc_main".to_string()));
}

/// 审计列的字面量与 engine 的词表**取值逐字相同**（两个类型、一份词表）。
#[test]
fn drop_reason_and_chat_type_literals_match_the_engine_vocabulary() {
    for (reason, literal) in [
        (DropReason::UnboundUser, "unbound_user"),
        (DropReason::NonWorkspaceMember, "non_workspace_member"),
        (DropReason::NotAddressedInGroup, "not_addressed_in_group"),
        (DropReason::Duplicate, "duplicate"),
        (DropReason::RevokedInstallation, "revoked_installation"),
        (DropReason::InvalidEvent, "invalid_event"),
    ] {
        assert_eq!(reason.as_str(), literal);
    }
    // 与 engine 的词表逐个对照：**取值逐字相同**。
    let engine_literals = [
        crate::engine::resolvers::DropReason::UnboundUser,
        crate::engine::resolvers::DropReason::NonWorkspaceMember,
        crate::engine::resolvers::DropReason::NotAddressedInGroup,
        crate::engine::resolvers::DropReason::Duplicate,
        crate::engine::resolvers::DropReason::RevokedInstallation,
        crate::engine::resolvers::DropReason::InvalidEvent,
    ];
    for reason in engine_literals {
        assert!(
            [
                "unbound_user",
                "non_workspace_member",
                "not_addressed_in_group",
                "duplicate",
                "revoked_installation",
                "invalid_event"
            ]
            .contains(&reason.as_str()),
            "engine 的取值必须在本 adapter 的词表里: {}",
            reason.as_str()
        );
    }
    assert_eq!(chat_type_str(ChatType::Group), "group");
    assert_eq!(chat_type_str(ChatType::P2p), "p2p");
}

/// `ChatId` 的类型化出口与串形态一致。
#[test]
fn outbound_chat_id_of_wraps_the_string_form() {
    let binding = ChatSessionBinding::from_legacy_row(&legacy_binding_row("oc_main", "group"));
    assert_eq!(outbound_chat_id_of(&binding), ChatId::new("oc_main"));
}
