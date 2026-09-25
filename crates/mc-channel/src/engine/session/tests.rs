//! 会话状态机的**纯**用例（代际窗口 / 失败关闭的恢复计划 / 隔离键 / 三个端口的判决）。
//!
//! 只有真 PostgreSQL 能判的那一半（行锁顺序、CAS、事务内落定）在
//! `mc-repos/src/channel/session/tests.rs`（`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）。
//! 这里钉的是**判决**：它们在任何 DB 上都必须成立，而且必须在没有 DB 时也跑得动。

use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, Source};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::session::ChannelChatSessionBindingRow;

use super::*;
use crate::engine::resolvers::{Auditor, DropReason, Outcome};

// ---------------------------------------------------------------------
// 造一条入站消息
// ---------------------------------------------------------------------

fn message(channel_type: ChannelKind, chat_id: &str, thread_id: &str) -> InboundMessage {
    InboundMessage {
        event_id: "ev-1".to_string(),
        message_id: "msg-1".to_string(),
        source: Source {
            channel_type,
            chat_id: chat_id.to_string(),
            chat_type: ChatType::Group,
            sender_id: "sender-native-1".to_string(),
            sender_stable_id: String::new(),
            thread_id: thread_id.to_string(),
        },
        kind: MessageKind::Text,
        text: "hello".to_string(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::json!({}),
    }
}

fn binding_row(context_revision: i64, retired: bool) -> ChannelChatSessionBindingRow {
    ChannelChatSessionBindingRow {
        id: Id::new().0,
        chat_session_id: Id::new().0,
        installation_id: Id::new().0,
        channel_type: "slack".to_string(),
        channel_chat_id: "C1".to_string(),
        chat_type: "group".to_string(),
        last_message_id: Some("msg-1".to_string()),
        last_thread_id: None,
        config: serde_json::json!({}),
        created_at: mc_core::timestamp::Timestamp::now().as_datetime(),
        pending_fresh: false,
        context_revision,
        route_revision: 1,
        retired_at: retired.then(|| mc_core::timestamp::Timestamp::now().as_datetime()),
        history_start_message_id: None,
        history_end_message_id: None,
        history_boundary_pending: false,
    }
}

// ---------------------------------------------------------------------
// 代际窗口
// ---------------------------------------------------------------------

/// **跨代围栏**：一个窗口只认自己那一代的 revision。
#[test]
fn a_context_window_only_accepts_its_own_generation() {
    let window = ContextWindow::opened(1);
    assert!(window.accepts(1));
    assert!(!window.accepts(2), "老代际不得读到新代际上下文");
    assert!(!window.accepts(0));

    let next = window.advance(Some("msg-2"), true);
    assert_eq!(next.revision(), 2);
    assert_eq!(next.history_start_message_id(), Some("msg-2"));
    assert!(!next.boundary_pending());
    assert!(next.pending_fresh(), "新代带着 fresh 意图");
    assert!(next.accepts(2));
    assert!(!next.accepts(1), "换代之后老 revision 一律被拒");
}

/// 收口：老代的 `history_end` 钉在触发它的那条消息上。
#[test]
fn closing_a_window_pins_the_boundary() {
    let window = ContextWindow::opened(1);
    let closed = window.closed(Some("msg-2"));
    assert_eq!(closed.history_end_message_id(), Some("msg-2"));
    assert_eq!(closed.revision(), 1, "收口不改代际");
    // 裸 /clear 没有平台游标 ⇒ 边界保持待定。
    assert_eq!(
        ContextWindow::opened(1)
            .closed(None)
            .history_end_message_id(),
        None
    );
}

/// 没有正文的换代（裸 `/clear` / 原生斜杠命令）：起点待定，等下一个真实游标钉下来。
#[test]
fn a_body_less_advance_keeps_the_start_pending() {
    let next = ContextWindow::opened(1).advance(Some("cmd-1"), false);
    assert_eq!(next.revision(), 2);
    assert_eq!(next.history_start_message_id(), None);
    assert!(next.boundary_pending());

    let resolved = next.resolve_history_start("msg-9");
    assert_eq!(resolved.history_start_message_id(), Some("msg-9"));
    assert!(!resolved.boundary_pending());
    assert_eq!(resolved.revision(), 2, "钉起点不改代际");
    // 已经钉过再钉一次是幂等的（不会被后来的消息改写）。
    assert_eq!(resolved.resolve_history_start("msg-10"), resolved);
    // 非待定的窗口同样不受影响。
    let steady = ContextWindow::opened(3);
    assert_eq!(steady.resolve_history_start("msg-11"), steady);
}

// ---------------------------------------------------------------------
// 崩溃恢复计划
// ---------------------------------------------------------------------

/// 恢复计划按代际升序；**发起人缺失的老代际失败关闭**（绝不冒充后来的发件人）。
#[test]
fn the_recovery_plan_fails_closed_without_an_initiator_snapshot() {
    let rows = vec![
        PendingContextRow {
            revision: 2,
            initiator_user_id: Some(Id::new().0),
        },
        PendingContextRow {
            revision: 1,
            initiator_user_id: None,
        },
    ];
    let plans = plan_pending_contexts(&rows);
    assert_eq!(
        plans.iter().map(|plan| plan.revision).collect::<Vec<_>>(),
        vec![1, 2],
        "按代际升序"
    );
    assert!(!plans[0].is_recoverable(), "没有发起人快照 ⇒ 不恢复");
    assert!(plans[1].is_recoverable());

    let contexts = pending_contexts(&plans);
    assert_eq!(contexts.len(), 2);
    assert_eq!(contexts[0].revision, 1);
    assert_eq!(contexts[0].initiator_user_id, None);
    assert_eq!(contexts[1].initiator_user_id, plans[1].initiator_user_id);
}

/// 一个带 revision 的迟到回调：只有代际仍是当前代、且 revision 对得上才能落。
#[test]
fn a_late_callback_is_rejected_once_its_generation_moved() {
    assert!(ChannelSessionBinder::generation_is_current(
        &binding_row(2, false),
        2
    ));
    assert!(!ChannelSessionBinder::generation_is_current(
        &binding_row(3, false),
        2
    ));
    assert!(
        !ChannelSessionBinder::generation_is_current(&binding_row(2, true), 2),
        "路由被退休之后，任何老回调都不许再落"
    );
}

// ---------------------------------------------------------------------
// 隔离键
// ---------------------------------------------------------------------

/// 隔离键：默认就是平台 chat id；线程化平台把线程根拼进去（一个频道两个线程 = 两个会话）。
#[test]
fn the_binding_key_policy_isolates_threads_only_when_asked() {
    let channel = message(ChannelKind::Slack, "C123", "1700000000.000100");
    assert_eq!(BindingKeyPolicy::ChatId.compose(&channel), "C123");
    assert_eq!(
        BindingKeyPolicy::ChatIdPlusThreadRoot.compose(&channel),
        "C123#1700000000.000100"
    );
    // 顶层消息（无线程）退回 chat id —— 线程化平台也不该造出空线程键。
    let top_level = message(ChannelKind::Slack, "C123", "");
    assert_eq!(
        BindingKeyPolicy::ChatIdPlusThreadRoot.compose(&top_level),
        "C123"
    );
    assert_eq!(BindingKeyPolicy::default(), BindingKeyPolicy::ChatId);
}

// ---------------------------------------------------------------------
// 去重端口：命中 ≠ 错误
// ---------------------------------------------------------------------

/// 可注入的去重替身（判两个分支：命中 / DB 故障）。
#[derive(Default)]
struct FakeDedup {
    outcome: std::sync::Mutex<Option<&'static str>>,
    claims: std::sync::Mutex<Vec<String>>,
    marks: std::sync::Mutex<Vec<Id>>,
    releases: std::sync::Mutex<Vec<Id>>,
}

#[async_trait]
impl DedupStore for FakeDedup {
    async fn claim(&self, _installation_id: Id, message_id: &str) -> EngineResult<Option<Id>> {
        self.claims
            .lock()
            .expect("claims")
            .push(message_id.to_string());
        match *self.outcome.lock().expect("outcome") {
            Some("duplicate") => Ok(None),
            Some("infra") => Err(EngineError::infra("connection reset by peer")),
            _ => Ok(Some(Id::new())),
        }
    }

    async fn mark(
        &self,
        _installation_id: Id,
        _message_id: &str,
        claim_token: Id,
    ) -> EngineResult<bool> {
        self.marks.lock().expect("marks").push(claim_token);
        Ok(*self.outcome.lock().expect("outcome") != Some("claim-lost"))
    }

    async fn release(
        &self,
        _installation_id: Id,
        _message_id: &str,
        claim_token: Id,
    ) -> EngineResult<bool> {
        self.releases.lock().expect("releases").push(claim_token);
        Ok(true)
    }
}

fn deduper(outcome: &'static str) -> (Arc<FakeDedup>, ChannelDeduper) {
    let fake = Arc::new(FakeDedup::default());
    *fake.outcome.lock().expect("outcome") = Some(outcome);
    let store: Arc<dyn DedupStore> = Arc::clone(&fake) as Arc<dyn DedupStore>;
    (fake, ChannelDeduper::with_store(store, ChannelKind::Slack))
}

/// **去重命中 ⇒ 丢弃且不报错**：端口把"命中"报成产品性判决 `Duplicate`（Router 消费成 `dropped`），
/// 而**基础设施失败**报成 `Infra`（adapter 上报）。两个分支各一条断言。
#[tokio::test]
async fn a_dedup_hit_is_a_product_verdict_while_a_store_failure_is_not() {
    let installation_id = Id::new();

    let (_fake, hit) = deduper("duplicate");
    let error = hit
        .claim(installation_id, "msg-1")
        .await
        .expect_err("命中不是成功");
    assert!(
        matches!(error, EngineError::Pipeline(PipelineError::Duplicate)),
        "命中必须是产品性判决，got {error:?}"
    );
    assert_eq!(error.code_hint(), "duplicate");
    assert_eq!(
        PipelineError::Duplicate.drop_reason(),
        Some(DropReason::Duplicate),
        "Router 靠它把这次认领变成 dropped 结论（而**不是**错误）"
    );
    assert_eq!(Outcome::Dropped.as_str(), "dropped");

    let (_fake, broken) = deduper("infra");
    let error = broken
        .claim(installation_id, "msg-1")
        .await
        .expect_err("DB 故障不是成功");
    assert!(
        matches!(error, EngineError::Infra { .. }),
        "SQL 失败必须是基础设施失败，got {error:?}"
    );
    assert_eq!(error.code_hint(), "engine_infra_error");
    assert_eq!(error.into_channel_error().code(), "channel_storage_error");
}

/// 落定：令牌被抢走 ⇒ `ClaimLost`（等价于 duplicate，调用方回滚）；释放是 fenced no-op ⇒ 不报错。
#[tokio::test]
async fn mark_and_release_are_fenced_but_only_mark_can_fail() {
    let installation_id = Id::new();
    let token = Id::new();

    let (_fake, lost) = deduper("claim-lost");
    let error = lost
        .mark(installation_id, "msg-1", token)
        .await
        .expect_err("令牌被抢走");
    assert!(matches!(
        error,
        EngineError::Pipeline(PipelineError::ClaimLost)
    ));
    assert_eq!(
        PipelineError::ClaimLost.drop_reason(),
        Some(DropReason::Duplicate),
        "认领丢失等价于 duplicate"
    );

    let (fake, healthy) = deduper("ok");
    healthy
        .release(installation_id, "msg-1", token)
        .await
        .expect("释放不是错误");
    assert_eq!(fake.releases.lock().expect("releases").len(), 1);
    healthy
        .mark(installation_id, "msg-1", token)
        .await
        .expect("正常落定");
    assert_eq!(fake.marks.lock().expect("marks").len(), 1);
}

// ---------------------------------------------------------------------
// 审计端口：非内容口径
// ---------------------------------------------------------------------

#[derive(Default)]
struct FakeAudit {
    drops: std::sync::Mutex<Vec<mc_repos::channel::inbound_audit::NewChannelInboundDrop>>,
}

#[async_trait]
impl AuditStore for FakeAudit {
    async fn record_drop(
        &self,
        drop: &mc_repos::channel::inbound_audit::NewChannelInboundDrop,
    ) -> EngineResult<()> {
        self.drops.lock().expect("drops").push(drop.clone());
        Ok(())
    }
}

/// 审计行只带路由 / 身份 / 原因 / 时刻 —— **没有**正文列，转发出去的入参同样没有。
#[tokio::test]
async fn the_audit_row_never_carries_the_message_body() {
    let fake = Arc::new(FakeAudit::default());
    let store: Arc<dyn AuditStore> = Arc::clone(&fake) as Arc<dyn AuditStore>;
    let auditor = ChannelAuditor::with_store(store, ChannelKind::Slack);

    let mut inbound = message(ChannelKind::Slack, "C1", "");
    inbound.text = "SENSITIVE-BODY-CONTENT".to_string();
    inbound.command_text = "/issue SENSITIVE-BODY-CONTENT".to_string();
    let installation_id = Id::new();
    auditor
        .record_drop(
            Some(installation_id),
            &inbound,
            DropReason::NotAddressedInGroup,
        )
        .await
        .expect("record drop");

    let drops = fake.drops.lock().expect("drops");
    assert_eq!(drops.len(), 1);
    let drop = &drops[0];
    assert_eq!(drop.installation_id, Some(installation_id));
    assert_eq!(drop.kind, ChannelKind::Slack);
    assert_eq!(drop.channel_chat_id.as_deref(), Some("C1"));
    assert_eq!(drop.event_type, "text", "归一化后的消息种类（见函数文档）");
    assert_eq!(drop.channel_event_id.as_deref(), Some("ev-1"));
    assert_eq!(drop.channel_message_id.as_deref(), Some("msg-1"));
    assert_eq!(drop.drop_reason, "not_addressed_in_group");
    // 唯一可证明"正文没进审计"的机器判据：整份入参的 `Debug` 里没有正文。
    assert!(!format!("{drop:?}").contains("SENSITIVE-BODY-CONTENT"));
    assert!(!format!("{auditor:?}").contains("SENSITIVE-BODY-CONTENT"));
}

/// 空 id 落成 `NULL` 而不是空串（审计表的 `channel_event_id` 是可空列）。
#[test]
fn blank_platform_ids_become_nulls() {
    let mut inbound = message(ChannelKind::Telegram, "chat-1", "");
    inbound.event_id = String::new();
    inbound.message_id = String::new();
    let drop = drop_from_message(None, &inbound, DropReason::InvalidEvent);
    assert_eq!(drop.installation_id, None);
    assert_eq!(drop.channel_event_id, None);
    assert_eq!(drop.channel_message_id, None);
    assert_eq!(drop.drop_reason, "invalid_event");
    assert_eq!(drop.kind.storage_str(), "telegram");
}

/// 两个适配器的 `Debug` 只列平台与**存在性**，不含接缝内部状态。
#[test]
fn the_adapters_do_not_leak_store_state_in_debug() {
    let (_fake, deduper) = deduper("ok");
    assert!(format!("{deduper:?}").contains("<dyn DedupStore>"));
    let auditor = ChannelAuditor::with_store(Arc::new(FakeAudit::default()), ChannelKind::Lark);
    assert!(format!("{auditor:?}").contains("Lark"));
    assert!(format!("{auditor:?}").contains("<dyn AuditStore>"));
}

/// 会话绑定器的配置与接缝存在性（装配断言；语义在真库用例里）。
#[test]
fn the_binder_reports_its_configuration_without_touching_the_database() {
    let binder_config = SessionBinderConfig {
        binding_key: BindingKeyPolicy::ChatIdPlusThreadRoot,
    };
    assert_eq!(
        binder_config.binding_key,
        BindingKeyPolicy::ChatIdPlusThreadRoot
    );
    assert_eq!(
        SessionBinderConfig::default().binding_key,
        BindingKeyPolicy::ChatId
    );
}
