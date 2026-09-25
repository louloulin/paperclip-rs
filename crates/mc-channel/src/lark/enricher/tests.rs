//! [`super`]（富上下文装配器）的用例 —— 上游 `inbound_enricher_test.go` 与
//! `inbound_enricher_recent_test.go` 的等价集合。
//!
//! 六组：**不取回**的三条短路 / 引用回复 / 合并转发 / 群近况（含话题隔离与失败关闭）/
//! 控制指令（`/clear` / `/new`）/ **重试与预算**（注入时钟，不睡真觉）。
//!
//! 最后再加一组「**`union_id` 与 `region` 在入站路径上的可测性**」：上游那两个回填文件的
//! 存在理由是"回填之前的安装仍要能用"，本片用**注入的时钟 + 注入的客户端**把那条路径钉住，
//! **不依赖任何真实回填**（`docs/32` §29 把它登记为交接项：两个回填文件本身归 M7-14）。

use std::sync::Arc;

use mc_core::channel::message::ChatType;

use super::super::client::{ApiClient, ApiError};
use super::super::feishu_channel::LarkInboundMessage;
use super::super::params::{AppSecret, InstallationCredentials};
use super::super::types::{ChatId, LarkMessage, OpenId, Region};
use super::super::ws_frame_decoder::LarkEventMention;
use super::*;

mod fixtures;
use fixtures::*;

// =====================================================================
// 一、三条短路：没有可展开内容 / 传输层未接线 / 近况关掉
// =====================================================================

/// 没有 `parent_id`、不是转发、不是"群里 @ 了 bot"⇒ **一次调用都不发**，正文原样。
#[tokio::test]
async fn nothing_to_expand_makes_no_call() {
    let api = api().build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let payload = p2p_payload("om-1", "你好");

    let result = enricher.enrich(payload.clone(), &credentials()).await;
    assert_eq!(result.body, "你好");
    assert_eq!(api.list_calls(), 0);
    assert_eq!(api.get_message_calls(), 0);
}

/// 传输层没接线（替身客户端）⇒ 跳过而不是给每条回复盖一个取回失败的戳。
#[tokio::test]
async fn unconfigured_client_short_circuits_silently() {
    let api = api().unconfigured().build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "总结一下");
    payload.parent_id = "om-parent".to_string();

    let result = enricher.enrich(payload.clone(), &credentials()).await;
    assert_eq!(result.body, payload.body);
    assert_eq!(api.get_message_calls(), 0);
}

/// `recent_context_size == 0`（配置显式关掉）⇒ 群里 @ 了 bot 也不预取近况，
/// 但**显式**附上的引用照旧展开。
#[tokio::test]
async fn zero_recent_context_size_disables_the_prefetch_only() {
    let api = api()
        .get_message(
            "om-parent",
            Ok(vec![quoted_parent("om-parent", "ou_a", "被引用的话")]),
        )
        .build();
    let mut config = config();
    config.recent_context_size = 0;
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config);
    let mut payload = group_payload("om-1", "看看这个");
    payload.parent_id = "om-parent".to_string();

    let result = enricher.enrich(payload, &credentials()).await;
    assert_eq!(api.list_calls(), 0, "关掉预取就不该列消息");
    assert!(result.body.contains("<quoted_message"), "{}", result.body);
}

// =====================================================================
// 二、引用回复
// =====================================================================

/// 引用父消息 ⇒ `<quoted_message>` 在前、用户自己的消息在后，且 `has_selected_context` 置位。
#[tokio::test]
async fn quoted_parent_is_prepended_and_marks_selected_context() {
    let api = api()
        .get_message(
            "om-parent",
            Ok(vec![quoted_parent("om-parent", "ou_alice", "被引用的话")]),
        )
        .users(&[("ou_alice", "Alice")])
        .user(&[("ou_bob", "Bob")])
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "回一下这句");
    payload.parent_id = "om-parent".to_string();
    payload.sender_open_id = OpenId::new("ou_bob");

    let result = enricher.enrich(payload, &credentials()).await;
    assert!(result.has_selected_context, "显式引用应当置位");
    assert!(
        result.body.starts_with(
            "<quoted_message message_id=\"om-parent\" sender=\"Alice\" type=\"text\">"
        ),
        "{}",
        result.body
    );
    assert!(result.body.contains("被引用的话"), "{}", result.body);
    assert!(
        result.body.ends_with("[Bob]: 回一下这句"),
        "{}",
        result.body
    );
}

/// 取不到父消息 ⇒ 降级成错误块（**不**上抛），用户正文照旧带名字标签。
#[tokio::test]
async fn missing_parent_degrades_to_the_error_block() {
    let api = api()
        .get_message(
            "om-parent",
            Err(ApiError::Http {
                op: "get_message",
                status: 403,
            }),
        )
        .users(&[("ou_alice", "Alice")])
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "回一下这句");
    payload.parent_id = "om-parent".to_string();
    payload.sender_open_id = OpenId::new("ou_alice");

    let result = enricher.enrich(payload, &credentials()).await;
    assert!(result.body.contains("<quoted_message message_id=\"om-parent\" type=\"error\">[unable to fetch]</quoted_message>"), "{}", result.body);
    assert!(
        result.body.ends_with("[Alice]: 回一下这句"),
        "{}",
        result.body
    );
}

/// p2p 保留**位置标签**（1:1 里身份没有歧义 ⇒ 不加 `[User 1]:` 前缀）。
#[tokio::test]
async fn direct_chats_keep_the_body_unlabelled() {
    let api = api()
        .get_message(
            "om-parent",
            Ok(vec![quoted_parent("om-parent", "ou_alice", "被引用的话")]),
        )
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = p2p_payload("om-1", "回一下这句");
    payload.parent_id = "om-parent".to_string();

    let result = enricher.enrich(payload, &credentials()).await;
    assert!(result.body.ends_with("回一下这句"), "{}", result.body);
    assert!(!result.body.contains("[User 1]"), "{}", result.body);
}

// =====================================================================
// 三、合并转发
// =====================================================================

/// 转发 ⇒ `<forwarded_messages>` 记录（子消息最旧在前）；父消息**不**取。
#[tokio::test]
async fn forward_is_expanded_from_its_own_id() {
    let children = vec![
        // 转发哨兵的 `message_id` **就是**触发消息自己（Lark 的形态：`GetMessage(id)` 的首项）。
        text_message("om-1", "ou_app", "app", "哨兵"),
        text_message("om-c1", "ou_bob", "user", "第一句"),
        text_message("om-c2", "ou_alice", "user", "第二句"),
    ];
    let api = api()
        .get_message("om-1", Ok(children))
        .users(&[("ou_alice", "Alice"), ("ou_bob", "Bob")])
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "");
    payload.message_type = "merge_forward".to_string();
    payload.body = String::new();

    let result = enricher.enrich(payload, &credentials()).await;
    assert!(result.has_selected_context);
    assert!(
        result.body.starts_with("<forwarded_messages count=\"2\">"),
        "{}",
        result.body
    );
    // 顺序：最旧在前（`om-c1` 的 create_time 更小）。
    let first = result.body.find("[Bob]: 第一句").expect("第一句");
    let second = result.body.find("[Alice]: 第二句").expect("第二句");
    assert!(first < second, "{}", result.body);
}

/// 取不到转发 ⇒ 错误块 + 一条 warn（**不**上抛）。
#[tokio::test]
async fn unreadable_forward_degrades_to_the_error_block() {
    let api = api()
        .get_message(
            "om-fwd",
            Err(ApiError::Refused {
                op: "get_message",
                status: None,
                code: 230_110,
            }),
        )
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-fwd", "");
    payload.message_type = "merge_forward".to_string();
    payload.body = String::new();

    let result = enricher.enrich(payload, &credentials()).await;
    assert_eq!(
        result.body,
        "<forwarded_messages type=\"error\">[unable to fetch]</forwarded_messages>"
    );
}

// =====================================================================
// 四、群近况
// =====================================================================

/// 群聊里 @ 了 bot ⇒ 预取一次近况，渲染成块，发言人用**一次**批量查名解析成真名。
#[tokio::test]
async fn recent_context_is_prefetched_for_addressed_group_messages() {
    let items = vec![
        text_message_at("om-a", "ou_alice", "user", "上一句", "1700000001000"),
        text_message_at("om-b", "ou_bob", "user", "更早一句", "1700000000000"),
    ];
    let api = api()
        .list(Ok(items))
        .users(&[("ou_alice", "Alice"), ("ou_bob", "Bob")])
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "总结一下");
    payload.addressed_to_bot = true;
    payload.sender_open_id = OpenId::new("ou_alice");

    let result = enricher.enrich(payload, &credentials()).await;
    assert_eq!(api.list_calls(), 1);
    // 最旧在前。
    assert!(
        result.body.starts_with(
            "<recent_context count=\"2\">\n[Bob]: 更早一句\n[Alice]: 上一句\n</recent_context>"
        ),
        "{}",
        result.body
    );
    assert!(
        result.body.ends_with("[Alice]: 总结一下"),
        "{}",
        result.body
    );
    // 窗口的上界锚在触发时刻（秒）。
    assert_eq!(api.list_end_time(0), 1_700_000_000);
}

/// 触发消息自己与它引用的父消息都从近况窗口里滤掉；bot 的互动卡回复也被滤掉。
#[tokio::test]
async fn recent_context_filters_self_parent_and_bot_cards() {
    let items = vec![
        text_message_at("om-1", "ou_alice", "user", "触发句自己", "1700000002000"),
        text_message_at("om-parent", "ou_bob", "user", "被引用那句", "1700000001500"),
        LarkMessage {
            message_id: "om-card".to_string(),
            message_type: "interactive".to_string(),
            sender_id: "cli_app".to_string(),
            sender_type: "app".to_string(),
            create_time: "1700000001000".to_string(),
            ..LarkMessage::default()
        },
        text_message_at("om-keep", "ou_carol", "user", "留下这句", "1700000000000"),
    ];
    let api = api().list(Ok(items)).build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "总结一下");
    payload.addressed_to_bot = true;
    payload.parent_id = "om-parent".to_string();

    // 父消息的取回也指向同一个会话（这里只需要它成功）。
    let result = enricher.enrich(payload, &credentials()).await;
    assert!(
        result
            .body
            .contains("<recent_context count=\"1\">\n[User 1]: 留下这句\n</recent_context>"),
        "{}",
        result.body
    );
    assert!(!result.body.contains("触发句自己"), "{}", result.body);
}

/// 话题里的 `@` ⇒ 窗口收窄到话题、**不用** `end_time`、客户端侧锚定到触发时刻，
/// 且**失败关闭**：`thread_id` 缺失 / 不匹配 / 晚于触发时刻的返回项一律丢掉（#5835）。
#[tokio::test]
async fn thread_scoped_recent_context_is_fail_closed() {
    let items = vec![
        LarkMessage {
            thread_id: "omt-1".to_string(),
            ..text_message_at("om-keep", "ou_a", "user", "同话题且更早", "1700000000000")
        },
        // 兄弟话题：thread_id 不匹配 ⇒ 丢。
        LarkMessage {
            thread_id: "omt-other".to_string(),
            ..text_message_at("om-sibling", "ou_b", "user", "兄弟话题", "1700000000500")
        },
        // 同话题但晚于触发时刻 ⇒ 丢（thread 容器忽略 end_time，靠客户端锚）。
        LarkMessage {
            thread_id: "omt-1".to_string(),
            ..text_message_at("om-later", "ou_c", "user", "晚于触发", "1700000009000")
        },
        // thread_id 缺失 ⇒ 丢（失败关闭）。
        text_message_at(
            "om-nothread",
            "ou_d",
            "user",
            "没有 thread_id",
            "1700000000600",
        ),
    ];
    let api = api().list(Ok(items)).build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "总结一下");
    payload.addressed_to_bot = true;
    payload.thread_id = "omt-1".to_string();
    payload.create_time = "1700000002000".to_string();

    let result = enricher.enrich(payload, &credentials()).await;
    assert!(api.list_used_thread(0), "话题路径应当带 thread_id");
    assert_eq!(api.list_end_time(0), 0, "thread 容器不接受 end_time");
    assert!(
        result
            .body
            .contains("<recent_context count=\"1\">\n[User 1]: 同话题且更早\n</recent_context>"),
        "{}",
        result.body
    );
}

/// 话题取回失败 ⇒ 与会话路径**一样**降级，且**永不**回落到会话级取回（那会把泄露重新打开）。
#[tokio::test]
async fn thread_fetch_failure_never_falls_back_to_the_chat_container() {
    let api = api()
        .list(Err(ApiError::Refused {
            op: "list_chat_messages",
            status: None,
            code: 99_991_002,
        }))
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "总结一下");
    payload.addressed_to_bot = true;
    payload.thread_id = "omt-1".to_string();

    let result = enricher.enrich(payload, &credentials()).await;
    assert_eq!(api.list_calls(), 1, "应当只试一次话题取回，不回落");
    assert!(
        result
            .body
            .contains("[Recent Lark context unavailable: the bot cannot read this chat history."),
        "{}",
        result.body
    );
}

/// 没有 `chat_id` ⇒ 立刻用 `channel_unbound` 降级（本地就拒，不发请求）。
#[tokio::test]
async fn missing_chat_id_degrades_with_the_channel_unbound_line() {
    let api = api().build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "总结一下");
    payload.addressed_to_bot = true;
    payload.chat_id = ChatId::new("");

    let result = enricher.enrich(payload, &credentials()).await;
    assert_eq!(api.list_calls(), 0, "本地就该拒掉");
    assert!(
        result
            .body
            .contains("[Recent Lark context unavailable: chat binding is missing."),
        "{}",
        result.body
    );
}

/// 查名失败 ⇒ 位置标签（**不**阻塞摄取）。
#[tokio::test]
async fn name_resolution_failure_degrades_to_positional_labels() {
    let api = api()
        .list(Ok(vec![text_message_at(
            "om-a",
            "ou_alice",
            "user",
            "上一句",
            "1700000001000",
        )]))
        .users_fail(ApiError::Http {
            op: "batch_get_users",
            status: 403,
        })
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "总结一下");
    payload.addressed_to_bot = true;
    payload.sender_open_id = OpenId::new("ou_alice");

    let result = enricher.enrich(payload, &credentials()).await;
    assert!(result.body.contains("[User 1]: 上一句"), "{}", result.body);
    // 上游逐字：用户自己那条**只在名字解析出来时**才加标签 ⇒ 查名失败就原样透传。
    assert!(result.body.ends_with("总结一下"), "{}", result.body);
    assert!(
        !result.body.contains("[User 1]: 总结一下"),
        "{}",
        result.body
    );
}

// =====================================================================
// 五、控制指令
// =====================================================================

/// 裸 `/clear` ⇒ 正文被剥空 + `force_fresh_session` 置位。
#[tokio::test]
async fn bare_clear_sets_force_fresh_and_keeps_the_body_empty() {
    let api = api().build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let payload = p2p_payload("om-1", "/clear");

    let result = enricher.enrich(payload, &credentials()).await;
    assert!(result.force_fresh_session);
    assert_eq!(result.body, "");
}

/// `/new` ⇒ 本回合**不**继承近况（`start_chat`），但显式附上的引用仍要展开。
#[tokio::test]
async fn new_chat_skips_recent_context_but_still_expands_a_quote() {
    let api = api()
        .list(Ok(vec![text_message_at(
            "om-a",
            "ou_a",
            "user",
            "上一条路由的近况",
            "1700000001000",
        )]))
        .get_message(
            "om-parent",
            Ok(vec![quoted_parent("om-parent", "ou_a", "被引用的话")]),
        )
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "/new 重开一下");
    payload.addressed_to_bot = true;
    payload.parent_id = "om-parent".to_string();

    let result = enricher.enrich(payload, &credentials()).await;
    assert_eq!(api.list_calls(), 0, "新 Chat 不得继承上一条路由的近况");
    assert!(result.body.contains("<quoted_message"), "{}", result.body);
    assert!(!result.body.contains("上一条路由的近况"), "{}", result.body);
}

// =====================================================================
// 六、重试与预算（注入时钟，不睡真觉）
// =====================================================================

/// 可重试失败 + 预算还有 ⇒ 重试**一次**，第二次成功 ⇒ 恢复。
#[tokio::test]
async fn retryable_failure_is_retried_once_while_budget_remains() {
    let api = api()
        .list(Err(ApiError::Http {
            op: "list_chat_messages",
            status: 503,
        }))
        .then_list(Ok(vec![text_message_at(
            "om-a",
            "ou_a",
            "user",
            "恢复后的近况",
            "1700000001000",
        )]))
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "总结一下");
    payload.addressed_to_bot = true;

    let result = enricher.enrich(payload, &credentials()).await;
    assert_eq!(api.list_calls(), 2, "第二次尝试应当发生");
    assert!(result.body.contains("恢复后的近况"), "{}", result.body);
}

/// **预算耗尽 ⇒ 不重试**（上游那条"第一次尝试超时 ⇒ 永不恢复"的判决）。
/// 时钟由用例推进（`ManualClock`）⇒ 不睡真觉、不依赖调度器时序。
#[tokio::test]
async fn exhausted_budget_means_no_second_attempt() {
    let clock = Arc::new(ManualClock::new(0));
    let mut config = config();
    config.budget = Duration::from_millis(50);
    config.clock = Arc::clone(&clock) as Arc<dyn Clock>;
    // 第一次调用就把时钟推过预算（替身模拟"这次调用吃光了预算"）。
    config.clock = Arc::clone(&clock) as Arc<dyn Clock>;
    let api = api()
        .list(Err(ApiError::Http {
            op: "list_chat_messages",
            status: 503,
        }))
        .advance_clock_on_list(Arc::clone(&clock), 60)
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config);
    let mut payload = group_payload("om-1", "总结一下");
    payload.addressed_to_bot = true;

    let result = enricher.enrich(payload, &credentials()).await;
    assert_eq!(api.list_calls(), 1, "预算已空 ⇒ 不再重试");
    assert!(
        result
            .body
            .contains("[Recent Lark context temporarily unavailable"),
        "{}",
        result.body
    );
}

/// 限流**不**重试（上游那一条刻意的判决：同预算内必然再撞，且让被限流的租户多挨一次）。
#[tokio::test]
async fn rate_limited_is_not_retried() {
    let api = api()
        .list(Err(ApiError::Refused {
            op: "list_chat_messages",
            status: None,
            code: 230_020,
        }))
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    let mut payload = group_payload("om-1", "总结一下");
    payload.addressed_to_bot = true;

    let result = enricher.enrich(payload, &credentials()).await;
    assert_eq!(api.list_calls(), 1);
    assert!(
        result
            .body
            .contains("[Recent Lark context temporarily unavailable"),
        "{}",
        result.body
    );
}

/// 预算为 0 ⇒ 连第一次都不发（确定的"预算耗尽"错误，不等调度器）。
#[tokio::test]
async fn zero_budget_fails_before_any_call() {
    let mut config = config();
    config.budget = Duration::from_millis(0);
    let api = api().build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config);
    let mut payload = group_payload("om-1", "总结一下");
    payload.addressed_to_bot = true;

    let result = enricher.enrich(payload, &credentials()).await;
    assert_eq!(api.list_calls(), 0);
    assert!(
        result
            .body
            .contains("[Recent Lark context temporarily unavailable"),
        "{}",
        result.body
    );
}

// =====================================================================
// 七、union_id 与 region 在**入站路径**上的可测性（不依赖真实回填）
// =====================================================================

/// **`union_id` 回填的可观测面就在入站路径上**：`bot_union_id` 已知 ⇒ 按 `union_id` 判 `@bot`；
/// 未知（**回填之前**的安装、或通讯录范围受限）⇒ 回落到 `open_id`，安装**仍然可用**。
///
/// 这条判决是 `union_id_backfill.go`（**M7-14 的写集**）的存在理由，本片把它钉在
/// "回填前 / 回填后"两种安装状态上，**不依赖回填真的跑过**。
#[tokio::test]
async fn union_id_backfill_state_is_testable_on_the_inbound_path() {
    let bot_mention = LarkEventMention {
        key: "@_user_1".to_string(),
        id: super::super::ws_frame_decoder::LarkSenderId {
            open_id: "ou_bot".to_string(),
            union_id: "on_bot".to_string(),
            user_id: String::new(),
        },
        name: "Bot".to_string(),
    };
    let api = api().build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());

    // ① 回填之后（`union_id` 已知）：多 bot 群里只有 `union_id` 一致才算 @ 到自己。
    let mut installed = fixtures::installation(Some("on_bot"));
    let mut event = fixtures::event(
        "om-1",
        "text",
        r#"{"text":"@_user_1 总结一下"}"#,
        ChatType::Group,
    );
    event.mentions = vec![bot_mention.clone()];
    let payload = LarkInboundMessage::from_event(event, &installed);
    assert_eq!(payload.body, "总结一下", "bot 自己的提及被剥掉");

    // ② 回填之前（`union_id` 缺席）：回落到 `open_id`（结构上对多 bot 群是反的，
    //    但 p2p / 单 bot 够用）⇒ **安装仍然可用**，不硬失败。
    installed.bot_union_id = None;
    let mut event = fixtures::event(
        "om-2",
        "text",
        r#"{"text":"@_user_1 总结一下"}"#,
        ChatType::Group,
    );
    event.mentions = vec![bot_mention];
    let payload = LarkInboundMessage::from_event(event, &installed);
    assert_eq!(payload.body, "总结一下", "回填之前仍要可用");
    let _ = enricher;

    // ③ 两个标识都空（一行坏数据）⇒ **不**匹配任何提及，`@_user_1` 原样留在正文里。
    let mut broken = installed.clone();
    broken.bot_open_id = OpenId::new("");
    let mut event = fixtures::event(
        "om-3",
        "text",
        r#"{"text":"@_user_1 总结一下"}"#,
        ChatType::Group,
    );
    event.mentions = vec![LarkEventMention {
        key: "@_user_1".to_string(),
        id: super::super::ws_frame_decoder::LarkSenderId {
            open_id: "ou_someone".to_string(),
            union_id: "on_someone".to_string(),
            user_id: String::new(),
        },
        name: "Someone".to_string(),
    }];
    let payload = LarkInboundMessage::from_event(event, &broken);
    assert_eq!(payload.body, "@Someone 总结一下");
}

/// **`region` 的可观测面也在入站路径上**：`region` 只出现在凭据里（`InstallationCredentials`），
/// 由注入的 [`ApiClient`] 观测 —— 所以"这条安装走哪个云"可以**不依赖 `region_backfill`**
/// 就钉住（那两个回填文件归 M7-14）。
#[tokio::test]
async fn region_travels_with_the_inbound_credentials() {
    let api = api()
        .list(Ok(vec![text_message_at(
            "om-a",
            "ou_a",
            "user",
            "近况",
            "1700000001000",
        )]))
        .build();
    let enricher = InboundEnricher::new(Arc::clone(&api) as Arc<dyn ApiClient>, config());
    for (raw, expected) in [("feishu", Region::Feishu), ("lark", Region::Lark)] {
        let credentials = InstallationCredentials::new("cli_x", AppSecret::new("s"))
            .with_region(Region::or_default(raw));
        let mut payload = group_payload("om-1", "总结一下");
        payload.addressed_to_bot = true;
        let _ = enricher.enrich(payload, &credentials).await;
        assert_eq!(
            api.last_region().expect("至少一次调用"),
            expected,
            "region={raw}"
        );
    }
}
