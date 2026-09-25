//! `channel_store.rs` 的用例（M7-13）：桥的**逐方法**行为 + 两族表的读写落点。
//!
//! 数据的替身是 [`crate::lark::tests::support::MemoryStore`]（两族表各一张），桥本身是**真代码**。

use std::sync::Arc;

use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::RepoError;
use serde_json::json;

use super::*;
use crate::lark::outbound::PatcherQueries;
use crate::lark::store::CardStatus;
use crate::lark::tests::support::{delivery_row, installation, legacy_binding_row, MemoryStore};
use crate::lark::types::ChatType;

fn ids() -> (Id, Id, Id, Id) {
    (
        Id(uuid::Uuid::from_u128(0x3000)),
        Id(uuid::Uuid::from_u128(0x2000)),
        Id(uuid::Uuid::from_u128(0x4000)),
        Id(uuid::Uuid::from_u128(0x5000)),
    )
}

// ---------------------------------------------------------------------
// 安装行（**遗留**表）
// ---------------------------------------------------------------------

#[tokio::test]
async fn installation_is_read_from_the_legacy_table() {
    let store = MemoryStore::new();
    let inst = installation(0x3000, "cli_a");
    store.put_installation(&inst);
    let bridged = store.store();

    let found = bridged
        .installation(inst.id)
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(found.app_id, "cli_a");
    assert_eq!(found.status, "active");
    assert!(found.is_active());
}

#[tokio::test]
async fn missing_installation_is_none_and_a_gone_row_is_also_none() {
    let store = MemoryStore::new();
    let bridged = store.store();
    assert!(bridged
        .installation(Id(uuid::Uuid::from_u128(0x9999)))
        .await
        .expect("lookup")
        .is_none());

    let inst = installation(0x3000, "cli_a");
    store.put_installation(&inst);
    *store.installation_gone.lock().expect("poisoned") = true;
    assert!(
        bridged
            .installation(inst.id)
            .await
            .expect("lookup")
            .is_none(),
        "行被删（运行时拆除）⇒ 查询答 None"
    );
}

/// 表族常量把"两套并存、逐行按族落"写在断言里。
#[test]
fn table_family_constants_pin_the_two_generations() {
    assert_eq!(LEGACY_INSTALLATION_TABLE, "lark_installation");
    assert_eq!(LEGACY_SESSION_BINDING_TABLE, "lark_chat_session_binding");
    assert_eq!(
        GENERIC_SESSION_BINDING_TABLE,
        "channel_chat_session_binding"
    );
    assert_eq!(TASK_DELIVERY_TABLE, "channel_task_delivery");
    assert_eq!(OUTBOUND_CARD_TABLE, "channel_outbound_card_message");
    assert_eq!(LEGACY_USER_BINDING_TABLE, "lark_user_binding");
    // lark 的安装行**不**落泛化表；另外三格是别的渠道与入站审计面。
    assert!(!NOT_USED_BY_LARK.contains(&LEGACY_INSTALLATION_TABLE));
    assert!(NOT_USED_BY_LARK.contains(&"channel_installation"));
    assert!(NOT_USED_BY_LARK.contains(&"channel_inbound_audit"));
}

// ---------------------------------------------------------------------
// 会话绑定（两族）
// ---------------------------------------------------------------------

#[tokio::test]
async fn legacy_session_binding_is_read_by_session() {
    let store = MemoryStore::new();
    let (inst_id, session_id, _, _) = ids();
    store.put_legacy_binding(legacy_binding_row(
        session_id,
        inst_id,
        "oc_main",
        ChatType::Group,
    ));
    let bridged = store.store();

    let binding = bridged
        .session_binding(session_id)
        .await
        .expect("read")
        .expect("present");
    assert_eq!(binding.chat_session_id, Some(session_id));
    assert_eq!(binding.channel_chat_id, "oc_main");
    assert_eq!(binding.chat_type, ChatType::Group);
}

/// **D11 的落地**：`bridge_session_binding` 第一次建行，第二次返回既有行（**不**重写游标）。
#[tokio::test]
async fn bridging_a_session_binding_is_idempotent_and_never_clobbers_the_cursor() {
    let store = MemoryStore::new();
    let (inst_id, session_id, _, _) = ids();
    store.put_installation(&installation(0x3000, "cli_a"));
    let bridged = store.store();

    let created = bridged
        .bridge_session_binding(session_id, inst_id, "oc_main", ChatType::Group)
        .await
        .expect("create");
    assert_eq!(created.channel_chat_id, "oc_main");

    // 游标被推进过之后，再桥一次**不得**把它抹掉。
    let advanced = bridged
        .remember_reply_target(session_id, Some("om_new"), Some("t_1"))
        .await
        .expect("advance");
    assert_eq!(advanced, 1);

    let again = bridged
        .bridge_session_binding(session_id, inst_id, "oc_main", ChatType::Group)
        .await
        .expect("idempotent");
    assert_eq!(again.last_message_id.as_deref(), Some("om_new"));
    assert_eq!(again.last_thread_id.as_deref(), Some("t_1"));
}

/// 外键（`REFERENCES lark_installation(id)`）不存在 ⇒ 冲突错误，**不**静默写脏行。
#[tokio::test]
async fn bridging_fails_closed_when_the_installation_row_does_not_exist() {
    let store = MemoryStore::new();
    let (inst_id, session_id, _, _) = ids();
    let bridged = store.store();
    let error = bridged
        .bridge_session_binding(session_id, inst_id, "oc_main", ChatType::Group)
        .await
        .expect_err("must fail");
    assert!(error
        .to_string()
        .contains("lark store (session binding insert)"));
}

/// 会话绑定面**同时**跨两族：遗留写 / 泛化读，且两个表名都在诊断里。
#[test]
fn session_binding_surface_spans_both_generations() {
    assert_ne!(LEGACY_SESSION_BINDING_TABLE, GENERIC_SESSION_BINDING_TABLE);
    assert!(LEGACY_SESSION_BINDING_TABLE.starts_with("lark_"));
    assert!(GENERIC_SESSION_BINDING_TABLE.starts_with("channel_"));
}

/// **两族并存的读侧证据**：入站把绑定写在泛化表上（M7-12 的通用 binder），本片补写遗留表
/// （D11）——两行**同时**在，且桥读的是遗留那一族。
#[tokio::test]
async fn both_generations_can_hold_a_row_for_the_same_session() {
    let store = MemoryStore::new();
    let (inst_id, session_id, _, _) = ids();
    store.put_installation(&installation(0x3000, "cli_a"));
    store.put_generic_binding(ChannelChatSessionBindingRow {
        id: uuid::Uuid::new_v4(),
        chat_session_id: session_id.0,
        installation_id: inst_id.0,
        channel_type: "feishu".to_string(),
        channel_chat_id: "oc_main".to_string(),
        chat_type: "group".to_string(),
        last_message_id: Some("om_generic".to_string()),
        last_thread_id: None,
        config: serde_json::json!({}),
        created_at: chrono::Utc::now(),
        pending_fresh: false,
        context_revision: 1,
        route_revision: 2,
        retired_at: None,
        history_start_message_id: None,
        history_end_message_id: None,
        history_boundary_pending: false,
    });

    // 泛化行在（入站写的那一行）。
    let generic = SessionBindingStore::generic_by_session(&*store, session_id)
        .await
        .expect("read")
        .expect("present");
    assert_eq!(generic.channel_chat_id, "oc_main");
    assert_eq!(generic.route_revision, 2);

    // 遗留行为空 ⇒ 桥读不到（这正是 D11 要补的那一半）。
    let bridged = store.store();
    assert!(bridged
        .session_binding(session_id)
        .await
        .expect("read")
        .is_none());

    // 补写之后两族**同时**有行，且桥读的是遗留那一族。
    let bridged_row = bridged
        .bridge_session_binding(session_id, inst_id, "oc_main", ChatType::Group)
        .await
        .expect("bridge");
    assert_eq!(bridged_row.chat_session_id, Some(session_id));
    assert_eq!(bridged_row.last_message_id, None, "**不**从泛化行拷游标");
    let generic_after = SessionBindingStore::generic_by_session(&*store, session_id)
        .await
        .expect("read")
        .expect("present");
    assert_eq!(
        generic_after.last_message_id.as_deref(),
        Some("om_generic"),
        "泛化行不被本片改写"
    );
}

// ---------------------------------------------------------------------
// 投递行 → 会话绑定的组装
// ---------------------------------------------------------------------

#[tokio::test]
async fn binding_for_task_needs_a_delivery_row() {
    let store = MemoryStore::new();
    let (_, _, task_id, _) = ids();
    let bridged = store.store();
    assert!(
        bridged
            .binding_for_task(task_id)
            .await
            .expect("read")
            .is_none(),
        "直接任务 ⇒ 失败关闭"
    );
}

/// 投递行的渠道判别式不是 feishu ⇒ 失败关闭（**不**回退到"猜"）。
#[tokio::test]
async fn binding_for_task_ignores_another_channels_delivery_row() {
    let store = MemoryStore::new();
    let (inst_id, _, task_id, binding_id) = ids();
    let mut row = delivery_row(
        task_id,
        binding_id,
        inst_id,
        "oc_main",
        ChatType::Group,
        Some("om_1"),
        None,
        json!({}),
    );
    row.channel_type = "slack".to_string();
    store.put_delivery(row);
    let bridged = store.store();
    assert!(bridged
        .binding_for_task(task_id)
        .await
        .expect("read")
        .is_none());
}

/// 有投递行 ⇒ 投递行的游标与发件人；有遗留绑定行 ⇒ 补上会话身份。
#[tokio::test]
async fn binding_for_task_composes_the_delivery_row_with_the_legacy_identity() {
    let store = MemoryStore::new();
    let (inst_id, session_id, task_id, binding_id) = ids();
    store.put_legacy_binding(legacy_binding_row(
        session_id,
        inst_id,
        "oc_main#t_1",
        ChatType::Group,
    ));
    store.put_delivery(delivery_row(
        task_id,
        binding_id,
        inst_id,
        "oc_main#t_1",
        ChatType::Group,
        Some("om_trigger"),
        Some("t_1"),
        json!({"chat_id": "oc_main", "sender_id": "ou_sender"}),
    ));
    let bridged = store.store();

    let binding = bridged
        .binding_for_task(task_id)
        .await
        .expect("read")
        .expect("present");
    assert_eq!(binding.id, binding_id, "投递行给绑定 id");
    assert_eq!(
        binding.chat_session_id,
        Some(session_id),
        "遗留行给会话身份"
    );
    assert_eq!(binding.last_message_id.as_deref(), Some("om_trigger"));
    assert_eq!(binding.last_sender_id.as_deref(), Some("ou_sender"));
    assert!(binding.is_topic_isolated());
    assert_eq!(binding.outbound_chat_id(), "oc_main");
}

// ---------------------------------------------------------------------
// 卡片行（**泛化**表）
// ---------------------------------------------------------------------

#[tokio::test]
async fn cards_are_created_patched_and_settled() {
    let store = MemoryStore::new();
    let (_, session_id, task_id, _) = ids();
    let bridged = store.store();

    let created = bridged
        .upsert_card(&NewOutboundCard {
            chat_session_id: session_id,
            task_id,
            channel_chat_id: "oc_main".to_string(),
            channel_card_message_id: "om_card".to_string(),
            status: CardStatus::Pending,
        })
        .await
        .expect("create");
    assert_eq!(created.status, CardStatus::Pending.as_str());
    assert!(!created.is_terminal());

    // 重复 upsert：冲突保留既有行（上游 `ON CONFLICT (task_id) DO UPDATE SET … = 既有值`）。
    let again = bridged
        .upsert_card(&NewOutboundCard {
            chat_session_id: session_id,
            task_id,
            channel_chat_id: "oc_other".to_string(),
            channel_card_message_id: "om_other".to_string(),
            status: CardStatus::Pending,
        })
        .await
        .expect("upsert again");
    assert_eq!(again.id, created.id);
    assert_eq!(again.channel_card_message_id, "om_card");

    assert!(bridged
        .mark_card_status(created.id, CardStatus::Streaming)
        .await
        .expect("streaming"));
    let streaming = bridged
        .card_by_task(task_id)
        .await
        .expect("read")
        .expect("present");
    assert_eq!(streaming.status, CardStatus::Streaming.as_str());
    assert!(streaming.last_patched_at.is_some());

    assert!(bridged
        .mark_card_status(created.id, CardStatus::Final)
        .await
        .expect("final"));
    // 终态之后不再改（上游 SQL 的 `WHERE … status NOT IN ('final','error')` ⇒ 0 行）。
    assert!(
        !bridged
            .mark_card_status(created.id, CardStatus::Error)
            .await
            .expect("after terminal"),
        "终态行不得被再翻状态"
    );

    assert!(
        !bridged
            .mark_card_status(Id(uuid::Uuid::from_u128(0xdead)), CardStatus::Error)
            .await
            .expect("unknown card"),
        "未知卡片 ⇒ 0 行，不是错误"
    );
}

// ---------------------------------------------------------------------
// 成员绑定（**遗留**表）
// ---------------------------------------------------------------------

#[tokio::test]
async fn user_binding_is_read_from_the_legacy_table() {
    let store = MemoryStore::new();
    let (inst_id, _, _, _) = ids();
    store.user_bindings.lock().expect("poisoned").insert(
        (inst_id.0, "ou_sender".to_string()),
        crate::lark::store::UserBinding {
            id: Id(uuid::Uuid::from_u128(0x7000)),
            workspace_id: Id(uuid::Uuid::from_u128(0x9000)),
            multica_user_id: Id(uuid::Uuid::from_u128(0x9100)),
            installation_id: inst_id,
            channel_user_id: "ou_sender".to_string(),
            union_id: Some("on_sender".to_string()),
            bound_at: chrono::Utc::now(),
        },
    );
    let bridged = store.store();

    let binding = bridged
        .user_binding(inst_id, "ou_sender")
        .await
        .expect("read")
        .expect("present");
    assert_eq!(binding.union_id.as_deref(), Some("on_sender"));
    assert!(bridged
        .user_binding(inst_id, "ou_other")
        .await
        .expect("read")
        .is_none());
}

// ---------------------------------------------------------------------
// 端口实现（出站 / 打字 / 回复器 共用同一个桥）
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_bridge_satisfies_the_patcher_port() {
    let store = MemoryStore::new();
    let (inst_id, session_id, task_id, binding_id) = ids();
    let inst = installation(0x3000, "cli_a");
    store.put_installation(&inst);
    store.put_agent_name(inst.agent_id, "Bot");
    store.put_delivery(delivery_row(
        task_id,
        binding_id,
        inst_id,
        "oc_main",
        ChatType::P2p,
        Some("om_1"),
        None,
        json!({}),
    ));
    store.put_legacy_binding(legacy_binding_row(
        session_id,
        inst_id,
        "oc_main",
        ChatType::P2p,
    ));
    let bridged = store.store();

    let queries: Arc<dyn PatcherQueries> = Arc::clone(&bridged) as Arc<dyn PatcherQueries>;
    assert!(queries
        .task_delivery(task_id)
        .await
        .expect("read")
        .is_some());
    let origin = queries.task_origin(task_id).await.expect("origin");
    assert!(origin.deliverable());
    assert_eq!(origin.chat_input_task_id, None, "D2：任务面给不出这一半");
    assert_eq!(
        queries.agent_name(inst.agent_id).await.expect("name"),
        Some("Bot".to_string())
    );
    assert!(queries
        .installation(inst_id)
        .await
        .expect("installation")
        .is_some());
    assert!(queries
        .binding_for_task(task_id)
        .await
        .expect("binding")
        .is_some());
    assert!(queries.card_by_task(task_id).await.expect("card").is_none());

    // 游标推进走遗留表。
    assert_eq!(
        queries
            .update_reply_target(session_id, Some("om_2"), None)
            .await
            .expect("cursor"),
        1
    );
}

/// **D2 的落地**：生产默认值是"有投递行 ⇒ 批次戳为真"，且 `chat_input_task_id` 那一半
/// 在任务面补齐之前只能给 `None` ⇒ [`crate::lark::outbound::TaskOrigin::deliverable`] 恒真。
/// 端口形状允许**收紧**（注入两半的完整形态即可）。
#[tokio::test]
async fn task_origin_defaults_to_channel_owned_when_a_delivery_row_exists() {
    use crate::lark::outbound::TaskOrigin;

    let store = MemoryStore::new();
    let (inst_id, _, task_id, binding_id) = ids();
    let bridged = store.store();
    let queries: Arc<dyn PatcherQueries> = Arc::clone(&bridged) as Arc<dyn PatcherQueries>;

    let none_yet = queries.task_origin(task_id).await.expect("origin");
    assert!(
        !none_yet.batch_has_channel_ingested_messages,
        "没有投递行 ⇒ 批次戳为假"
    );
    assert_eq!(none_yet.chat_input_task_id, None, "任务面给不出这一半");
    // ⚠️ D2 的**放宽**就在这里：`chat_input_task_id = None` ⇒ 判决恒真（上游 #5645 的默认投递）。
    assert!(none_yet.deliverable());

    store.put_delivery(delivery_row(
        task_id,
        binding_id,
        inst_id,
        "oc_main",
        ChatType::P2p,
        None,
        None,
        json!({}),
    ));
    let with_delivery = queries.task_origin(task_id).await.expect("origin");
    assert!(with_delivery.batch_has_channel_ingested_messages);
    assert!(with_delivery.deliverable());

    // 收紧：给出任务行的 `chat_input_task_id` + 批次戳的**完整**两半。
    let tightened = TaskOrigin {
        chat_input_task_id: Some(Id(uuid::Uuid::from_u128(0x7777))),
        batch_has_channel_ingested_messages: false,
    };
    assert!(
        !tightened.deliverable(),
        "有输入批次且批次里没有渠道消息 ⇒ 这条回复属于 Multica"
    );
    let tightened_true = TaskOrigin {
        chat_input_task_id: Some(Id(uuid::Uuid::from_u128(0x7777))),
        batch_has_channel_ingested_messages: true,
    };
    assert!(tightened_true.deliverable());
    // `chat_input_task_id` 缺省（密封之前的渠道任务）⇒ 照上游 #5645 的"默认投递"。
    assert!(TaskOrigin::from_batch_flag(false).deliverable());

    // 脚本注入：替换批次戳那一半即可让生产默认值变严。
    *store.channel_ingested.lock().expect("poisoned") = Some(false);
    let scripted = queries.task_origin(task_id).await.expect("origin");
    assert!(!scripted.batch_has_channel_ingested_messages);
}

/// 桥的 `Debug` 把**两族表名**都写出来，且**没有**凭据字段。
#[test]
fn the_bridge_debug_names_both_table_families_without_credentials() {
    let store = MemoryStore::new();
    let rendered = format!("{:?}", store.store());
    assert!(rendered.contains(LEGACY_INSTALLATION_TABLE));
    assert!(rendered.contains(&format!(
        "{LEGACY_SESSION_BINDING_TABLE} + {GENERIC_SESSION_BINDING_TABLE}"
    )));
    assert!(rendered.contains(TASK_DELIVERY_TABLE));
    assert!(rendered.contains(OUTBOUND_CARD_TABLE));
    assert!(rendered.contains(LEGACY_USER_BINDING_TABLE));
    for forbidden in ["app_secret", "secret", "token"] {
        assert!(
            !rendered.contains(forbidden),
            "桥的 Debug 不得出现 {forbidden}: {rendered}"
        );
    }
}

/// 存储口径的判别式（诊断出口）。
#[test]
fn storage_kind_is_the_lark_variant() {
    assert_eq!(storage_kind(), ChannelKind::Lark);
    assert_eq!(storage_kind().storage_str(), "feishu");
}

/// `store_error` 只带端口名与 `RepoError` 的分类文案（**不**带任何行内容）。
#[test]
fn store_error_keeps_only_the_port_and_the_class() {
    let engine = store_error("card")(RepoError::Conflict);
    assert_eq!(
        engine.to_string(),
        "engine: infrastructure failure: lark store (card): conflict"
    );
}
