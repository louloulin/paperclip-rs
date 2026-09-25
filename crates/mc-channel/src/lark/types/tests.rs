//! `types.rs` 的用例：**纯函数**（region 映射、标识符、丢弃原因、wire 归一化、资源头解析）。
//!
//! 上游口径逐条钉：`RegionOrDefault` 的回落、`ChatType` / `InstallationStatus` 的字面量与
//! `mc-core` **逐字相同**（本片不重复定义领域类型）、`larkRESTMessageItem.normalize` 的字段搬
//! 运、以及 `Content-Disposition` 的受限解析。

use super::*;

// =====================================================================
// region（上游 `types.go` 的 `Region` / `OpenPlatformBaseURL` / `RegionOrDefault`）
// =====================================================================

#[test]
fn region_maps_to_exactly_two_open_platform_hosts() {
    assert_eq!(
        Region::Feishu.open_platform_base_url(),
        "https://open.feishu.cn"
    );
    assert_eq!(
        Region::Lark.open_platform_base_url(),
        "https://open.larksuite.com"
    );
    // 两个云**必须**是不同的主机（否则"一个部署服务两个云"这条就不成立）。
    assert_ne!(
        Region::Feishu.open_platform_base_url(),
        Region::Lark.open_platform_base_url()
    );
    assert_eq!(
        Region::Feishu.open_platform_base_url(),
        DEFAULT_LARK_BASE_URL
    );
    assert_eq!(
        Region::Lark.open_platform_base_url(),
        LARK_INTERNATIONAL_OPEN_BASE_URL
    );
}

#[test]
fn region_defaults_to_feishu_for_empty_and_unknown_values() {
    // 上游 `RegionOrDefault`：**只**认 `lark`，其余一律飞书。
    assert_eq!(Region::or_default(""), Region::Feishu);
    assert_eq!(Region::or_default("feishu"), Region::Feishu);
    assert_eq!(Region::or_default("lark"), Region::Lark);
    assert_eq!(Region::or_default("LARK"), Region::Feishu);
    assert_eq!(Region::or_default("larksuite"), Region::Feishu);
    // 结构默认值也是飞书（`lark_installation.region` 之前落的历史行）。
    assert_eq!(Region::default(), Region::Feishu);
}

#[test]
fn region_from_str_opt_keeps_the_problem_visible() {
    // 与 `or_default` 的**分工**：本函数保留"这一行是坏的"这一事实。
    assert_eq!(Region::from_str_opt("feishu"), Some(Region::Feishu));
    assert_eq!(Region::from_str_opt("lark"), Some(Region::Lark));
    assert_eq!(Region::from_str_opt(""), None);
    assert_eq!(Region::from_str_opt("wecom"), None);
    for region in [Region::Feishu, Region::Lark] {
        assert_eq!(Region::from_str_opt(region.as_str()), Some(region));
    }
}

// =====================================================================
// mc-core 的领域枚举**不重复定义**（上游 `types.go` 的两个）
// =====================================================================

#[test]
fn chat_type_literals_match_upstream() {
    // 上游 `ChatTypeP2P` / `ChatTypeGroup` 与 `lark_chat_session_binding.lark_chat_type`
    // 的 `CHECK` 逐字一致。
    assert_eq!(ChatType::P2p.as_str(), "p2p");
    assert_eq!(ChatType::Group.as_str(), "group");
    assert_eq!(ChatType::from_str_opt("p2p"), Some(ChatType::P2p));
    assert_eq!(ChatType::from_str_opt("group"), Some(ChatType::Group));
    assert_eq!(ChatType::from_str_opt("channel"), None);
}

#[test]
fn installation_status_literals_match_upstream() {
    assert_eq!(InstallationStatus::Active.as_str(), "active");
    assert_eq!(InstallationStatus::Revoked.as_str(), "revoked");
    assert_eq!(
        InstallationStatus::from_str_opt("revoked"),
        Some(InstallationStatus::Revoked)
    );
    assert_eq!(InstallationStatus::from_str_opt("deleted"), None);
    assert!(InstallationStatus::Active.is_live());
    assert!(!InstallationStatus::Revoked.is_live());
}

// =====================================================================
// 标识符（上游 `types.go` 的两个 string alias）
// =====================================================================

#[test]
fn open_id_and_chat_id_are_opaque_strings() {
    let open_id = OpenId::new("ou_abc");
    assert_eq!(open_id.as_str(), "ou_abc");
    assert_eq!(open_id.to_string(), "ou_abc");
    assert!(!open_id.is_empty());
    assert!(OpenId::new("").is_empty());

    let chat_id = ChatId::new("oc_xyz");
    assert_eq!(chat_id.as_str(), "oc_xyz");
    assert_eq!(chat_id.to_string(), "oc_xyz");
    assert!(!chat_id.is_empty());
    assert!(ChatId::default().is_empty());
}

// =====================================================================
// 丢弃原因（上游 `types.go` 的 `DropReason`，六个）
// =====================================================================

#[test]
fn every_drop_reason_has_the_upstream_literal() {
    let pairs = [
        (DropReason::UnboundUser, "unbound_user"),
        (DropReason::NonWorkspaceMember, "non_workspace_member"),
        (DropReason::NotAddressedInGroup, "not_addressed_in_group"),
        (DropReason::Duplicate, "duplicate"),
        (DropReason::RevokedInstallation, "revoked_installation"),
        (DropReason::InvalidEvent, "invalid_event"),
    ];
    for (reason, literal) in pairs {
        assert_eq!(reason.as_str(), literal);
        // serde 口径与列口径**必须**一致（两条写入路径不能漂移）。
        assert_eq!(
            serde_json::to_string(&reason).expect("serialize"),
            format!("\"{literal}\"")
        );
    }
}

#[test]
fn binding_token_ttl_matches_the_storage_check() {
    // `channel_binding_token` 的 `CHECK` 是 `INTERVAL '15 minutes'`。
    assert_eq!(BINDING_TOKEN_TTL, Duration::from_mins(15));
}

// =====================================================================
// 领域值对象
// =====================================================================

#[test]
fn bot_info_reports_a_missing_union_id_as_unresolved() {
    // 空串 = 未解析（软失败路径），**不是**错误。
    let unresolved = BotInfo {
        open_id: OpenId::new("ou_bot"),
        union_id: String::new(),
    };
    assert!(!unresolved.has_union_id());

    let resolved = BotInfo {
        open_id: OpenId::new("ou_bot"),
        union_id: "on_bot".to_string(),
    };
    assert!(resolved.has_union_id());
}

// =====================================================================
// 资源体（`io.ReadCloser` 的等价物）
// =====================================================================

#[test]
fn rest_message_item_normalizes_every_field() {
    let raw = serde_json::json!({
        "message_id": "om_1",
        "root_id": "om_root",
        "parent_id": "om_parent",
        "thread_id": "omt_1",
        "upper_message_id": "om_upper",
        "msg_type": "post",
        "create_time": "1700000000000",
        "deleted": true,
        "sender": { "id": "ou_sender", "id_type": "open_id", "sender_type": "user" },
        "body": { "content": "{\"text\":\"hi\"}" },
        "mentions": [{ "key": "@_user_1", "id": "ou_1", "name": "A" }]
    });
    let item: LarkRestMessageItem = serde_json::from_value(raw).expect("decode");
    let message = item.normalize();
    assert_eq!(message.message_id, "om_1");
    assert_eq!(message.root_id, "om_root");
    assert_eq!(message.parent_id, "om_parent");
    assert_eq!(message.thread_id, "omt_1");
    assert_eq!(message.upper_message_id, "om_upper");
    assert_eq!(message.message_type, "post");
    assert_eq!(message.create_time, "1700000000000");
    assert!(message.deleted);
    assert_eq!(message.sender_id, "ou_sender");
    assert_eq!(message.sender_type, "user");
    // `body.content` **原样透传**（解释它是 flattener 的事）。
    assert_eq!(message.content, "{\"text\":\"hi\"}");
    assert_eq!(message.mentions.len(), 1);
    assert_eq!(message.mentions[0].key, "@_user_1");
    assert_eq!(message.mentions[0].id, "ou_1");
    assert_eq!(message.mentions[0].name, "A");
}

#[test]
fn rest_message_item_tolerates_the_optional_fields_being_absent() {
    let raw = serde_json::json!({ "message_id": "om_2" });
    let item: LarkRestMessageItem = serde_json::from_value(raw).expect("decode");
    let message = item.normalize();
    assert_eq!(message.message_id, "om_2");
    assert_eq!(message.message_type, "");
    assert!(message.mentions.is_empty());
    assert!(!message.deleted);
}

#[test]
fn message_items_envelope_turns_data_into_a_message_list() {
    let raw = serde_json::json!({
        "code": 0,
        "data": { "items": [{ "message_id": "om_1" }, { "message_id": "om_2" }] }
    });
    let envelope: MessageItemsEnvelope = serde_json::from_value(raw).expect("decode");
    let messages = envelope.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].message_id, "om_2");

    // 缺 `data` / 空 `items` 都合法（上游只判 `code`）。
    let empty: MessageItemsEnvelope =
        serde_json::from_value(serde_json::json!({})).expect("decode");
    assert!(empty.messages().is_empty());
}

#[test]
fn message_id_envelope_rejects_a_missing_or_empty_id() {
    let ok: MessageIdEnvelope =
        serde_json::from_value(serde_json::json!({ "data": { "message_id": "om_9" } }))
            .expect("decode");
    assert_eq!(ok.message_id("send").expect("id"), "om_9");

    let empty: MessageIdEnvelope =
        serde_json::from_value(serde_json::json!({ "data": { "message_id": "" } }))
            .expect("decode");
    assert_eq!(
        empty.message_id("send").expect_err("empty id"),
        ApiError::Malformed { op: "send" }
    );

    let missing: MessageIdEnvelope =
        serde_json::from_value(serde_json::json!({ "code": 0 })).expect("decode");
    assert!(missing.message_id("send").is_err());
}

#[test]
fn reaction_id_envelope_rejects_a_missing_or_empty_id() {
    let ok: ReactionIdEnvelope =
        serde_json::from_value(serde_json::json!({ "data": { "reaction_id": "r_1" } }))
            .expect("decode");
    assert_eq!(ok.reaction_id("react").expect("id"), "r_1");
    let bad: ReactionIdEnvelope = serde_json::from_value(serde_json::json!({})).expect("decode");
    assert!(bad.reaction_id("react").is_err());
}

#[test]
fn bot_info_envelope_reads_the_top_level_bot_object() {
    // ⚠️ `bot` 在**顶层**（上游 `botResp` 逐字），不在 `data` 下。
    let envelope: BotInfoEnvelope =
        serde_json::from_value(serde_json::json!({ "code": 0, "bot": { "open_id": "ou_bot" } }))
            .expect("decode");
    assert_eq!(
        envelope.open_id("bot info").expect("open_id").as_str(),
        "ou_bot"
    );

    let missing: BotInfoEnvelope =
        serde_json::from_value(serde_json::json!({ "code": 0, "bot": { "open_id": "" } }))
            .expect("decode");
    assert_eq!(
        missing.open_id("bot info").expect_err("empty"),
        ApiError::Malformed { op: "bot info" }
    );
}

#[test]
fn union_id_envelope_treats_a_missing_field_as_unresolved() {
    let present: UnionIdEnvelope =
        serde_json::from_value(serde_json::json!({ "data": { "user": { "union_id": "on_1" } } }))
            .expect("decode");
    assert_eq!(present.union_id(), "on_1");

    // 范围受限时 Lark 回 `code = 0` 而不带 `union_id` ⇒ 空串 + Ok（软失败）。
    let absent: UnionIdEnvelope =
        serde_json::from_value(serde_json::json!({ "code": 0 })).expect("decode");
    assert_eq!(absent.union_id(), "");
}

#[test]
fn user_batch_envelope_drops_items_missing_an_id_or_a_name() {
    let envelope: UserBatchEnvelope = serde_json::from_value(serde_json::json!({
        "code": 0,
        "data": { "items": [
            { "open_id": "ou_1", "name": "A" },
            { "open_id": "ou_2", "name": "" },
            { "open_id": "", "name": "C" },
            { "open_id": "ou_4", "name": "D" }
        ] }
    }))
    .expect("decode");
    let names = envelope.names();
    assert_eq!(names.len(), 2);
    assert_eq!(names.get("ou_1").map(String::as_str), Some("A"));
    assert_eq!(names.get("ou_4").map(String::as_str), Some("D"));
}

#[test]
fn tenant_token_response_tolerates_a_missing_expire() {
    let response: TenantTokenResponse = serde_json::from_value(serde_json::json!({
        "code": 0,
        "tenant_access_token": "t-abc"
    }))
    .expect("decode");
    assert_eq!(response.tenant_access_token, "t-abc");
    assert_eq!(response.expire, 0);
}

// =====================================================================
// 资源头解析（`Content-Disposition` 的受限解析）
// =====================================================================
