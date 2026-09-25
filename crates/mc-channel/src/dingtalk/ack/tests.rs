//! `dingtalk::ack` 的用例（写者 M7-8）。
//!
//! 三个层次：
//!
//! 1. **批次判据**（`sameReactionBatch`）与已终态围栏：纯函数 / 纯状态；
//! 2. **状态机**：受理 ⇒ 贴「收到」；终态 ⇒ 撤回；Done 只认本地记下的提供方坐标；
//!    重复消息不重复贴；被取代的旧批次立刻撤回；
//! 3. **接缝纪律**：同步 `TypingNotifier::on_ingested` 在没有运行时时**不** panic（只 warn），
//!    表情失败**不**影响受理（输入仍然留在活动表里）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, Source};
use mc_core::id::Id;
use mc_repos::chat_message::ChatMessageRow;

use super::{AckNotifier, NoReactionInputs, ReactionInputQueries, ReactionSender};
use crate::dingtalk::emotion::{Emotion, EmotionError};
use crate::dingtalk::inbound::TYPE_DINGTALK;
use crate::dingtalk::outbound::{ReplySource, SendTarget};
use crate::engine::resolvers::{EngineError, EngineResult, ResolvedInstallation, TypingNotifier};

// =====================================================================
// 替身
// =====================================================================

/// 一条被记下的表情调用。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReactionCall {
    message_id: String,
    emotion: Emotion,
    recall: bool,
}

#[derive(Default)]
struct RecordingReaction {
    calls: Mutex<Vec<ReactionCall>>,
    fail: Mutex<bool>,
}

impl RecordingReaction {
    fn failing() -> Self {
        let this = Self::default();
        *this.fail.lock().expect("lock") = true;
        this
    }

    fn calls(&self) -> Vec<ReactionCall> {
        self.calls.lock().expect("lock").clone()
    }
}

#[async_trait]
impl ReactionSender for RecordingReaction {
    async fn react(
        &self,
        _installation: &ResolvedInstallation,
        target: &SendTarget,
        emotion: Emotion,
        recall: bool,
    ) -> Result<(), EmotionError> {
        if *self.fail.lock().expect("lock") {
            return Err(EmotionError::Transport {
                message: "scripted failure".to_string(),
            });
        }
        self.calls.lock().expect("lock").push(ReactionCall {
            message_id: target.source_message_id.clone(),
            emotion,
            recall,
        });
        Ok(())
    }
}

/// 内存里的 `chat_message` 读面。
#[derive(Default)]
struct MemoryInputs {
    rows: Mutex<HashMap<Id, ChatMessageRow>>,
    fail: Mutex<bool>,
}

impl MemoryInputs {
    fn with(rows: &[ChatMessageRow]) -> Arc<Self> {
        let this = Arc::new(Self::default());
        for row in rows {
            this.rows
                .lock()
                .expect("lock")
                .insert(Id(row.id), row.clone());
        }
        this
    }
}

#[async_trait]
impl ReactionInputQueries for MemoryInputs {
    async fn get_chat_message(&self, id: Id) -> EngineResult<Option<ChatMessageRow>> {
        if *self.fail.lock().expect("lock") {
            return Err(EngineError::infra("scripted lookup failure"));
        }
        Ok(self.rows.lock().expect("lock").get(&id).cloned())
    }
}

// =====================================================================
// 夹具
// =====================================================================

fn message_row(
    id: Id,
    session: Id,
    ingested: bool,
    role: &str,
    task: Option<Id>,
    revision: Option<i64>,
    created_at: i64,
) -> ChatMessageRow {
    ChatMessageRow {
        id: id.0,
        chat_session_id: session.0,
        role: role.to_string(),
        content: "hi".to_string(),
        task_id: task.map(|task| task.0),
        created_at: DateTime::<Utc>::from_timestamp(created_at, 0).expect("timestamp"),
        failure_reason: None,
        elapsed_ms: None,
        message_kind: "user".to_string(),
        channel_media_pending_until: None,
        channel_ingested: ingested,
        quick_actions: serde_json::json!([]),
        channel_context_revision: revision,
        channel_outbound_type: None,
        channel_outbound_installation_id: None,
        channel_outbound_chat_id: None,
        channel_outbound_message_ids: None,
    }
}

fn installation_with() -> ResolvedInstallation {
    ResolvedInstallation::new(
        Id::new(),
        Id::new(),
        Id::new(),
        Id::new(),
        TYPE_DINGTALK,
        true,
    )
}

fn inbound(message_id: &str, chat_type: ChatType) -> InboundMessage {
    InboundMessage {
        event_id: "ev".to_string(),
        message_id: message_id.to_string(),
        source: Source {
            channel_type: TYPE_DINGTALK,
            chat_id: "chat".to_string(),
            chat_type,
            sender_id: "staff".to_string(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
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
        raw: serde_json::json!({ "app_id": "app-key" }),
    }
}

/// 一条"已受理"的输入：缓存里记下坐标（宿主在 append 成功后做的同一件事）。
fn accepted(
    notifier: &AckNotifier,
    installation: &ResolvedInstallation,
    session: Id,
    input: Id,
    message_id: &str,
) {
    notifier.remember_source(ReplySource::new(
        installation.id,
        input,
        session,
        &inbound(message_id, ChatType::Group),
    ));
}

// =====================================================================
// 批次判据
// =====================================================================

/// `sameReactionBatch` 的四条分支。
#[test]
fn the_batch_predicate_matches_upstream() {
    let session = Id::new();
    let task = Id::new();
    let unsealed = message_row(Id::new(), session, true, "user", None, Some(7), 1);
    let mut same = unsealed.clone();
    same.id = Id::new().0;
    assert!(super::same_reaction_batch(&unsealed, &same), "同会话同代际");

    let mut other_revision = unsealed.clone();
    other_revision.channel_context_revision = Some(8);
    assert!(!super::same_reaction_batch(&unsealed, &other_revision));

    let sealed = message_row(Id::new(), session, true, "user", Some(task), Some(7), 1);
    let mut sealed_same_task = sealed.clone();
    sealed_same_task.id = Id::new().0;
    assert!(
        super::same_reaction_batch(&sealed, &sealed_same_task),
        "同任务"
    );
    let mut sealed_other_task = sealed.clone();
    sealed_other_task.task_id = Some(Id::new().0);
    assert!(!super::same_reaction_batch(&sealed, &sealed_other_task));
    assert!(
        !super::same_reaction_batch(&unsealed, &sealed),
        "一边密封一边没密封 ⇒ 不是同一批"
    );

    let mut other_session = unsealed.clone();
    other_session.chat_session_id = Id::new().0;
    assert!(!super::same_reaction_batch(&unsealed, &other_session));

    let mut not_ingested = unsealed.clone();
    not_ingested.channel_ingested = false;
    assert!(!super::same_reaction_batch(&unsealed, &not_ingested));

    let mut assistant = unsealed.clone();
    assistant.role = "assistant".to_string();
    assert!(!super::same_reaction_batch(&unsealed, &assistant));
}

/// 没有输入读面时也能受理（跳过批次分类）。
#[tokio::test]
async fn without_an_input_port_the_batch_classification_is_skipped() {
    let reaction = Arc::new(RecordingReaction::default());
    let notifier = AckNotifier::new(Arc::clone(&reaction) as Arc<dyn ReactionSender>, None);
    let installation = installation_with();
    let session = Id::new();
    let message = inbound("m1", ChatType::Group);
    notifier.ingest_now(&installation, &message, session).await;
    let calls = reaction.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].emotion, Emotion::Acknowledged);
    assert!(!calls[0].recall, "受理时是**贴**不是撤");
    assert_eq!(calls[0].message_id, "m1");
    assert_eq!(notifier.active_count(session), 1);
    assert!(notifier.has_session(session));

    // `NoReactionInputs` 是显式给出的缺省（错误是基础设施类，不是 panic）。
    let no_inputs = NoReactionInputs;
    assert!(no_inputs.get_chat_message(Id::new()).await.is_err());
}

/// 受理 ⇒ 贴「收到」；终态 ⇒ **撤**「收到」并摘掉句柄。
#[tokio::test]
async fn a_terminal_event_recalls_the_acknowledged_reaction() {
    let reaction = Arc::new(RecordingReaction::default());
    let notifier = AckNotifier::new(Arc::clone(&reaction) as Arc<dyn ReactionSender>, None);
    let installation = installation_with();
    let session = Id::new();
    notifier
        .ingest_now(&installation, &inbound("m1", ChatType::Group), session)
        .await;
    notifier.settled_now(session).await;

    let calls = reaction.calls();
    assert_eq!(calls.len(), 2);
    assert!(!calls[0].recall);
    assert!(calls[1].recall, "终态撤的是同一条表情");
    assert_eq!(calls[1].emotion, Emotion::Acknowledged);
    assert_eq!(calls[1].message_id, "m1");
    assert_eq!(notifier.active_count(session), 0);
    assert!(!notifier.has_session(session));

    // 幂等：再来一次没有可撤的。
    notifier.settled_now(session).await;
    assert_eq!(reaction.calls().len(), 2);
}

/// 同一个源消息重复受理 ⇒ 只贴一次（上游的 duplicate 短路）。
#[tokio::test]
async fn a_duplicate_message_is_not_acknowledged_twice() {
    let reaction = Arc::new(RecordingReaction::default());
    let notifier = AckNotifier::new(Arc::clone(&reaction) as Arc<dyn ReactionSender>, None);
    let installation = installation_with();
    let session = Id::new();
    let message = inbound("m1", ChatType::Group);
    notifier.ingest_now(&installation, &message, session).await;
    notifier.ingest_now(&installation, &message, session).await;
    assert_eq!(reaction.calls().len(), 1);
    assert_eq!(notifier.active_count(session), 1);
}

/// 缺消息 id / 缺会话 id ⇒ 什么都不做。
#[tokio::test]
async fn incomplete_messages_never_reach_the_reaction_port() {
    let reaction = Arc::new(RecordingReaction::default());
    let notifier = AckNotifier::new(Arc::clone(&reaction) as Arc<dyn ReactionSender>, None);
    let installation = installation_with();
    let mut no_message = inbound("", ChatType::Group);
    notifier
        .ingest_now(&installation, &no_message, Id::new())
        .await;
    no_message.message_id = "m1".to_string();
    no_message.source.chat_id = String::new();
    notifier
        .ingest_now(&installation, &no_message, Id::new())
        .await;
    assert!(reaction.calls().is_empty());
}

/// 表情**失败**不拒收输入，也不挡后续（句柄仍然在活动表里）。
#[tokio::test]
async fn a_reaction_failure_never_rejects_the_input() {
    let reaction = Arc::new(RecordingReaction::failing());
    let notifier = AckNotifier::new(Arc::clone(&reaction) as Arc<dyn ReactionSender>, None);
    let installation = installation_with();
    let session = Id::new();
    notifier
        .ingest_now(&installation, &inbound("m1", ChatType::Group), session)
        .await;
    assert_eq!(notifier.active_count(session), 1, "输入仍然被受理");
    // 失败 ⇒ 撤回时把句柄摘掉（上游 `recallStates` 的 `attempted` 分支）。
    notifier.settled_now(session).await;
    assert_eq!(notifier.active_count(session), 0);
}

// =====================================================================
// 批次：取代与围栏
// =====================================================================

/// 同批的旧输入被新输入**取代**：旧的立刻撤回，新的留下并贴出自己的「收到」。
#[tokio::test]
async fn a_newer_input_in_the_same_batch_supersedes_the_older_one() {
    let reaction = Arc::new(RecordingReaction::default());
    let session = Id::new();
    let installation = installation_with();
    let older_input = Id::new();
    let newer_input = Id::new();
    let rows = vec![
        message_row(older_input, session, true, "user", None, Some(1), 10),
        message_row(newer_input, session, true, "user", None, Some(1), 20),
    ];
    let notifier = AckNotifier::new(
        Arc::clone(&reaction) as Arc<dyn ReactionSender>,
        Some(MemoryInputs::with(&rows)),
    );
    accepted(&notifier, &installation, session, older_input, "m-older");
    accepted(&notifier, &installation, session, newer_input, "m-newer");

    notifier
        .ingest_now(&installation, &inbound("m-older", ChatType::Group), session)
        .await;
    notifier
        .ingest_now(&installation, &inbound("m-newer", ChatType::Group), session)
        .await;

    let calls = reaction.calls();
    // 旧的那条：贴 + 撤；新的那条：只有贴。
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[0].message_id, "m-older");
    assert!(!calls[0].recall);
    assert_eq!(calls[1].message_id, "m-older");
    assert!(calls[1].recall, "被取代的旧批次立刻撤回");
    assert_eq!(calls[2].message_id, "m-newer");
    assert!(!calls[2].recall);
    assert_eq!(notifier.active_count(session), 1, "只留最新那条");
}

/// 反过来的顺序（旧的在读面里更晚）⇒ 新来的被拒，**不**贴表情。
#[tokio::test]
async fn an_older_arrival_after_a_newer_one_is_rejected() {
    let reaction = Arc::new(RecordingReaction::default());
    let session = Id::new();
    let installation = installation_with();
    let older_input = Id::new();
    let newer_input = Id::new();
    let rows = vec![
        message_row(older_input, session, true, "user", None, Some(1), 10),
        message_row(newer_input, session, true, "user", None, Some(1), 20),
    ];
    let notifier = AckNotifier::new(
        Arc::clone(&reaction) as Arc<dyn ReactionSender>,
        Some(MemoryInputs::with(&rows)),
    );
    accepted(&notifier, &installation, session, newer_input, "m-newer");
    accepted(&notifier, &installation, session, older_input, "m-older");
    notifier
        .ingest_now(&installation, &inbound("m-newer", ChatType::Group), session)
        .await;
    notifier
        .ingest_now(&installation, &inbound("m-older", ChatType::Group), session)
        .await;
    let calls = reaction.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].message_id, "m-newer");
}

/// 已终态围栏：终态事件之后，同一输入 id 的迟到受理**不**再贴表情。
#[tokio::test]
async fn a_settled_input_cannot_be_re_acknowledged() {
    let reaction = Arc::new(RecordingReaction::default());
    let session = Id::new();
    let installation = installation_with();
    let input = Id::new();
    let rows = vec![message_row(input, session, true, "user", None, Some(1), 10)];
    let notifier = AckNotifier::new(
        Arc::clone(&reaction) as Arc<dyn ReactionSender>,
        Some(MemoryInputs::with(&rows)),
    );
    accepted(&notifier, &installation, session, input, "m1");
    notifier
        .ingest_now(&installation, &inbound("m1", ChatType::Group), session)
        .await;
    let after_first = reaction.calls().len();

    // 终态事件只退休它自己那一批。
    let own = rows[0].clone();
    notifier.inputs_settled_now(session, &[own]).await;
    assert_eq!(notifier.active_count(session), 0);

    // 迟到的受理：同一个输入 id ⇒ 拒绝（不再贴）。
    notifier
        .ingest_now(&installation, &inbound("m1", ChatType::Group), session)
        .await;
    assert_eq!(
        reaction.calls().len(),
        after_first + 1,
        "只多了一次撤，没有新的贴"
    );
    assert!(reaction.calls().last().expect("有调用").recall);
}

/// 输入读面失败 ⇒ 保留现有回执、不贴新的（上游 `TestAckBatchLookupFailuresKeepExistingReceipt`）。
#[tokio::test]
async fn an_input_lookup_failure_keeps_existing_receipts() {
    let reaction = Arc::new(RecordingReaction::default());
    let session = Id::new();
    let installation = installation_with();
    let input = Id::new();
    let rows = vec![message_row(input, session, true, "user", None, Some(1), 10)];
    let inputs = MemoryInputs::with(&rows);
    let notifier = AckNotifier::new(
        Arc::clone(&reaction) as Arc<dyn ReactionSender>,
        Some(Arc::clone(&inputs) as Arc<dyn ReactionInputQueries>),
    );
    accepted(&notifier, &installation, session, input, "m1");
    notifier
        .ingest_now(&installation, &inbound("m1", ChatType::Group), session)
        .await;
    let calls_after_first = reaction.calls().len();
    assert_eq!(notifier.active_count(session), 1);

    *inputs.fail.lock().expect("lock") = true;
    accepted(&notifier, &installation, session, Id::new(), "m2");
    notifier
        .ingest_now(&installation, &inbound("m2", ChatType::Group), session)
        .await;
    assert_eq!(
        reaction.calls().len(),
        calls_after_first,
        "读面挂了 ⇒ 不贴新的、也不撤已有的"
    );
    assert_eq!(notifier.active_count(session), 1);
}

// =====================================================================
// Done / 归档
// =====================================================================

/// Done 只认本地记下的坐标：没记过 ⇒ 什么都不发；记过 ⇒ 贴 Done。
#[tokio::test]
async fn done_requires_a_locally_recorded_source() {
    let reaction = Arc::new(RecordingReaction::default());
    let installation = installation_with();
    let session = Id::new();
    let input = Id::new();
    let notifier = AckNotifier::new(Arc::clone(&reaction) as Arc<dyn ReactionSender>, None);
    notifier.on_reply_delivered(&installation, input).await;
    assert!(reaction.calls().is_empty(), "缓存未命中 ⇒ 跳过 Done");

    accepted(&notifier, &installation, session, input, "m1");
    notifier.on_reply_delivered(&installation, input).await;
    let calls = reaction.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].emotion, Emotion::Done);
    assert!(!calls[0].recall);
    assert_eq!(calls[0].message_id, "m1");

    // 别的安装 ⇒ 认不出（安装必须逐字相符）。
    let other = installation_with();
    notifier.on_reply_delivered(&other, input).await;
    assert_eq!(reaction.calls().len(), 1, "安装不符 ⇒ 不贴");
}

/// agent 归档：只退休**本 agent** 的回执，并撤回它们。
#[tokio::test]
async fn archiving_an_agent_retires_and_recalls_only_its_receipts() {
    let reaction = Arc::new(RecordingReaction::default());
    let notifier = AckNotifier::new(Arc::clone(&reaction) as Arc<dyn ReactionSender>, None);
    let archived_agent = Id::new();
    let mut archived_installation = installation_with();
    archived_installation.agent_id = archived_agent;
    let other_installation = installation_with();
    let (session_a, session_b) = (Id::new(), Id::new());
    notifier
        .ingest_now(
            &archived_installation,
            &inbound("m-a", ChatType::Group),
            session_a,
        )
        .await;
    notifier
        .ingest_now(
            &other_installation,
            &inbound("m-b", ChatType::Group),
            session_b,
        )
        .await;
    assert_eq!(reaction.calls().len(), 2);

    notifier.agent_archived_now(archived_agent).await;
    let calls = reaction.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[2].message_id, "m-a", "只撤归档 agent 的那条");
    assert!(calls[2].recall);
    assert_eq!(notifier.active_count(session_a), 0);
    assert_eq!(notifier.active_count(session_b), 1, "另一个 agent 不动");
}

/// `with_sources` 让出站侧与回执侧**共享**同一个缓存（Done 的唯一判据）。
#[tokio::test]
async fn the_notifier_shares_the_reply_source_cache() {
    let reaction = Arc::new(RecordingReaction::default());
    let notifier = AckNotifier::new(Arc::clone(&reaction) as Arc<dyn ReactionSender>, None);
    let installation = installation_with();
    let session = Id::new();
    let input = Id::new();
    // 从**共享**缓存里取回同一个实例：宿主在装配时把出站侧的那个交进来。
    let shared = notifier.sources().clone();
    let notifier = notifier.with_sources(shared.clone());
    shared.remember(ReplySource::new(
        installation.id,
        input,
        session,
        &inbound("m1", ChatType::Group),
    ));
    assert!(notifier
        .sources()
        .source_for(installation.id, input)
        .is_some());
    notifier.on_reply_delivered(&installation, input).await;
    assert_eq!(reaction.calls().len(), 1, "共享缓存 ⇒ Done 贴得出来");
}

// =====================================================================
// 同步接缝
// =====================================================================

/// `TypingNotifier` 的同步入口在没有运行时时**不** panic（只记 warn）。
#[test]
fn the_sync_seam_tolerates_a_missing_runtime() {
    let reaction = Arc::new(RecordingReaction::default());
    let notifier = AckNotifier::new(Arc::clone(&reaction) as Arc<dyn ReactionSender>, None);
    let installation = installation_with();
    notifier.on_ingested(&installation, &inbound("m1", ChatType::Group), Id::new());
    notifier.on_settled(Id::new());
    assert!(reaction.calls().is_empty(), "没有运行时 ⇒ 什么都没做");
}

/// 有运行时 ⇒ 同步接缝真的把工作推出了（`on_settled` 之后活动表清空）。
#[tokio::test]
async fn the_sync_seam_dispatches_the_detached_work() {
    let reaction = Arc::new(RecordingReaction::default());
    let notifier = AckNotifier::new(Arc::clone(&reaction) as Arc<dyn ReactionSender>, None);
    let installation = installation_with();
    let session = Id::new();
    notifier.on_ingested(&installation, &inbound("m1", ChatType::Group), session);
    // 脱离任务的完成时间不可观测 ⇒ 用"最终一致"的断言（等它把两步都做完）。
    for _ in 0..100 {
        if notifier.active_count(session) == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(notifier.active_count(session), 1, "脱离任务贴上了「收到」");
    assert!(notifier.has_session(session));

    notifier.on_settled(session);
    for _ in 0..100 {
        if notifier.active_count(session) == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(notifier.active_count(session), 0);
    assert!(reaction.calls().iter().any(|call| call.recall));
}
