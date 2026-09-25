//! `telegram::outbound` 的用例（写者 M7-6）。
//!
//! 覆盖上游 `outbound.go` 的**可判定面**：占位消息一个轮次只发一次、编辑阶梯
//! （not-modified / 标记被拒 / 目标没了 / 永久拒绝 / 结果不明）、最终答案的分片推进、
//! 失败告知走**同一条**消息、出站目标的解析。平台替身按脚本应答；投递账用 `delivery` 的
//! 进程内替身 ⇒ **真状态机 + 真记账**，只是不睡真觉（节流间隔压到 0）。

use std::sync::{Arc, Mutex};

use mc_core::channel::ChannelKind;
use serde_json::json;

use super::*;
use crate::telegram::api::{ApiError, ApiResult, WebhookInfo};
use crate::telegram::delivery::testing::{
    ledger as fake_ledger, target as delivery_target, FakeStore,
};
use crate::telegram::delivery::{DeliveryLease, ReplyTurn, PHASE_TERMINAL};
use crate::telegram::inbound::{Message, Update, User};

/// 平台替身：记下每次调用，编辑按脚本应答。
#[derive(Default)]
struct ScriptedApi {
    state: Mutex<Scripted>,
}

#[derive(Default)]
struct Scripted {
    sends: Vec<SendMessage>,
    edits: Vec<EditMessageText>,
    send_script: Vec<ApiResult<Message>>,
    edit_script: Vec<ApiResult<()>>,
    next_send_id: i64,
}

impl ScriptedApi {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn sends(&self) -> Vec<SendMessage> {
        self.state.lock().expect("lock").sends.clone()
    }

    fn edits(&self) -> Vec<EditMessageText> {
        self.state.lock().expect("lock").edits.clone()
    }

    fn push_edit(&self, result: ApiResult<()>) {
        self.state.lock().expect("lock").edit_script.push(result);
    }

    fn push_send(&self, result: ApiResult<Message>) {
        self.state.lock().expect("lock").send_script.push(result);
    }
}

#[async_trait::async_trait]
impl TelegramApi for ScriptedApi {
    async fn get_me(&self, _bot_token: &str) -> ApiResult<User> {
        Ok(User::default())
    }
    async fn get_webhook_info(&self, _bot_token: &str) -> ApiResult<WebhookInfo> {
        Ok(WebhookInfo::default())
    }
    async fn get_updates(&self, _bot_token: &str, _offset: i64) -> ApiResult<Vec<Update>> {
        Ok(Vec::new())
    }
    async fn send_message(&self, _bot_token: &str, params: &SendMessage) -> ApiResult<Message> {
        let mut state = self.state.lock().expect("lock");
        state.sends.push(params.clone());
        if !state.send_script.is_empty() {
            return state.send_script.remove(0);
        }
        state.next_send_id += 1;
        Ok(Message {
            message_id: state.next_send_id,
            ..Message::default()
        })
    }
    async fn send_chat_action(&self, _b: &str, _c: i64, _t: i64) -> ApiResult<()> {
        Ok(())
    }
    async fn edit_message_text(&self, _bot_token: &str, params: &EditMessageText) -> ApiResult<()> {
        let mut state = self.state.lock().expect("lock");
        state.edits.push(params.clone());
        if state.edit_script.is_empty() {
            return Ok(());
        }
        state.edit_script.remove(0)
    }
}

/// 一条出站目标（chat id 用数值，thread / reply 用 0 或真值）。
fn reply_target() -> ReplyTarget {
    let delivery = delivery_target();
    ReplyTarget {
        stream_key: delivery.task_id.to_string(),
        bot_key: delivery.installation_id.to_string(),
        chat_id: 4242,
        thread_id: 7,
        reply_to: 99,
        bot_token: Sensitive::new("123456:not-a-real-bot-token-itest-only"),
        delivery,
    }
}

/// 装配一个 Outbound 与它的两个替身（节流压到 0）。
fn harness() -> (Arc<ScriptedApi>, Arc<FakeStore>, Outbound) {
    let (store, ledger) = fake_ledger();
    let api = ScriptedApi::new();
    let outbound = Outbound::new(api.clone(), Arc::new(ledger)).with_edit_interval(Duration::ZERO);
    (api, store, outbound)
}

/// 在给定阶段取一条真租约。
async fn take_lease(outbound: &Outbound, target: &ReplyTarget, depth: i32) -> DeliveryLease {
    let turn = ReplyTurn {
        id: target.delivery.task_id,
        depth,
    };
    let (lease, status) = outbound
        .ledger()
        .acquire(&target.delivery, turn, PHASE_TERMINAL)
        .await
        .expect("acquire");
    assert_eq!(status, crate::telegram::delivery::DeliveryStatus::Acquired);
    lease.expect("lease")
}

/// 一个 400 的 API 错误。
fn api_error(code: u16, description: &str) -> ApiError {
    ApiError::Api {
        method: "editMessageText",
        code,
        description: description.to_string(),
        retry_after: None,
    }
}

// ---------------------------------------------------------------------------
// 流式占位消息
// ---------------------------------------------------------------------------

/// 一个轮次**恰好**一条占位消息：第一帧发，之后的帧编辑。
#[tokio::test]
async fn the_placeholder_is_sent_once_and_edited_afterwards() {
    let (api, _store, outbound) = harness();
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;

    let first = outbound.push_partial(&target, &lease, "**hello**", 0).await;
    assert_eq!(first, StreamStep::PlaceholderCreated { message_id: 1 });
    let sends = api.sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].parse_mode, "HTML");
    assert_eq!(sends[0].text, "<b>hello</b>");
    assert_eq!(sends[0].message_thread_id, 7, "话题路由");
    assert_eq!(sends[0].reply_to_message_id, 99, "引用触发消息");

    let second = outbound
        .push_partial(&target, &lease, "**hello** world", 1)
        .await;
    assert_eq!(second, StreamStep::Edited);
    assert_eq!(api.sends().len(), 1, "第二帧不许再发一条");
    let edits = api.edits();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].message_id, 1);
    assert_eq!(edits[0].parse_mode, "HTML");
    assert_eq!(edits[0].text, "<b>hello</b> world");
}

/// 第一帧的正文为空时占位文本是 `…`（上游 `firstNonEmpty`）。
#[tokio::test]
async fn an_empty_first_frame_uses_the_placeholder_text() {
    let (api, _store, outbound) = harness();
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;
    outbound.push_partial(&target, &lease, "\n", 0).await;
    assert_eq!(api.sends()[0].text, STREAM_PLACEHOLDER);
}

/// 另一个副本（或自动重试继承的前一次尝试）已经发过占位消息 ⇒ 编辑**它**，不发第二条。
#[tokio::test]
async fn a_row_message_from_another_replica_is_edited_not_re_placed() {
    let (api, store, outbound) = harness();
    let target = reply_target();
    let turn = ReplyTurn {
        id: target.delivery.task_id,
        depth: 0,
    };
    let (first, _) = outbound
        .ledger()
        .acquire(&target.delivery, turn, PHASE_TERMINAL)
        .await
        .expect("acquire");
    let first = first.expect("lease");
    outbound.push_partial(&target, &first, "part one", 0).await;
    assert_eq!(api.sends().len(), 1);

    // 前一个持有者退出（租约过期），接管者拿到的行里**已经**有那条可编辑消息。
    store.expire(target.delivery.task_id);
    let (successor, status) = outbound
        .ledger()
        .acquire(&target.delivery, turn, PHASE_TERMINAL)
        .await
        .expect("takeover");
    assert_eq!(status, crate::telegram::delivery::DeliveryStatus::Acquired);
    let successor = successor.expect("lease");
    assert_eq!(successor.message_id(), 1, "行里的消息是权威");

    let step = outbound
        .push_partial(&target, &successor, "part two", 0)
        .await;
    assert_eq!(step, StreamStep::Edited);
    assert_eq!(api.sends().len(), 1, "行里已有消息 ⇒ 不再新发");
    assert_eq!(api.edits().last().map(|edit| edit.message_id), Some(1));
}

/// 租约丢了 ⇒ 流式帧什么都不做（也不冒泡成错误：它是装饰）。
#[tokio::test]
async fn a_stream_frame_that_lost_the_lease_does_nothing() {
    let (api, store, outbound) = harness();
    let target = reply_target();
    let turn = ReplyTurn {
        id: target.delivery.task_id,
        depth: 0,
    };
    let (first, _) = outbound
        .ledger()
        .acquire(&target.delivery, turn, PHASE_TERMINAL)
        .await
        .expect("acquire");
    let stale = first.expect("lease");
    outbound.push_partial(&target, &stale, "hello", 0).await;
    // 另一个进程接管了这一轮；旧租约的令牌已经写不动东西了。
    store.expire(target.delivery.task_id);
    let (successor, _) = outbound
        .ledger()
        .acquire(&target.delivery, turn, PHASE_TERMINAL)
        .await
        .expect("takeover");
    assert!(successor.is_some());

    let step = outbound
        .push_partial(&target, &stale, "hello again", 1)
        .await;
    assert_eq!(
        step,
        StreamStep::Idle {
            reason: "lease_lost"
        }
    );
    assert_eq!(api.edits().len(), 0, "丢租约的帧不许碰平台");
    assert_eq!(api.sends().len(), 1);
}

/// 流式中途超上限 ⇒ 冻在上限处（完整回复由最终答案分片投递）。
#[test]
fn a_stream_that_overflows_is_frozen_at_the_cap() {
    let long = "a".repeat(50);
    assert_eq!(Outbound::stream_text_cap(&long, 100), long);
    let capped = Outbound::stream_text_cap(&long, 10);
    assert_eq!(capped, "a".repeat(10));
    assert!(utf16_units(&capped) <= 10);
}

// ---------------------------------------------------------------------------
// 最终答案
// ---------------------------------------------------------------------------

/// 最终答案先**编辑**占位消息（同一个目标），多片时按节流一片一片推进。
#[tokio::test]
async fn the_answer_edits_the_placeholder_then_advances_chunk_by_chunk() {
    let (api, store, outbound) = harness();
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;
    outbound.push_partial(&target, &lease, "working…", 0).await;

    // 两片：第一片 3500 码元上限，第二片是剩下的。
    let text = format!("{}\n{}", "a".repeat(3600), "b".repeat(100));
    let chunks = Outbound::plan_chunks(&text);
    assert_eq!(chunks.len(), 2, "应当分成两片");

    let mut progress = AnswerProgress {
        streamed_message_id: 1,
        ..AnswerProgress::default()
    };
    let first = outbound
        .deliver_answer(&target, &lease, &chunks, &mut progress)
        .await;
    assert_eq!(first, Step::RetryAfter(Duration::ZERO), "还有第二片");
    assert!(progress.placeholder_edited);
    assert_eq!(progress.chunk_index, 1);
    assert_eq!(api.sends().len(), 1, "第一片走编辑，不新发");

    let second = outbound
        .deliver_answer(&target, &lease, &chunks, &mut progress)
        .await;
    assert_eq!(second, Step::done("delivered"));
    assert_eq!(api.sends().len(), 2, "第二片新发");
    assert_eq!(
        api.sends()[1].reply_to_message_id,
        0,
        "只有第一片引用触发消息"
    );
    let row = store.row(target.delivery.task_id);
    assert_eq!(row.phase, crate::telegram::delivery::PHASE_SETTLED);
    assert_eq!(row.settled_reason, "delivered");
    assert_eq!(row.chunks_sent, 2, "分片数记在账上");
    assert_eq!(row.message_id, "1", "编辑目标不漂移");
}

/// 空答案 ⇒ 直接收口（`empty_reply`），一片都不发。
#[tokio::test]
async fn an_empty_answer_settles_without_sending() {
    let (api, store, outbound) = harness();
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;
    let mut progress = AnswerProgress::default();
    let step = outbound
        .deliver_answer(&target, &lease, &[], &mut progress)
        .await;
    assert_eq!(step, Step::done("empty_reply"));
    assert!(api.sends().is_empty());
    assert_eq!(
        store.row(target.delivery.task_id).settled_reason,
        "empty_reply"
    );
}

/// 编辑阶梯：not-modified 当成功；标记被拒换纯文本**同一目标**再来一次。
#[tokio::test]
async fn the_edit_ladder_handles_not_modified_and_markup_refusal() {
    // not-modified ⇒ 当成功。
    let (api, _store, outbound) = harness();
    api.push_edit(Err(api_error(400, "Bad Request: message is not modified")));
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;
    let mut progress = AnswerProgress {
        streamed_message_id: 5,
        ..AnswerProgress::default()
    };
    let step = outbound
        .deliver_answer(&target, &lease, &["single".to_string()], &mut progress)
        .await;
    assert_eq!(step, Step::done("delivered"), "not-modified 是良性");

    // 标记被拒 ⇒ 同一个目标换纯文本再试（**不**新发）。
    let (api, _store, outbound) = harness();
    api.push_edit(Err(api_error(
        400,
        "Bad Request: can't parse entities: unsupported start tag",
    )));
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;
    let mut progress = AnswerProgress {
        streamed_message_id: 9,
        ..AnswerProgress::default()
    };
    let step = outbound
        .deliver_answer(&target, &lease, &["**x**".to_string()], &mut progress)
        .await;
    assert_eq!(step, Step::done("delivered"));
    let edits = api.edits();
    assert_eq!(edits.len(), 2, "先 HTML 再纯文本");
    assert_eq!(edits[0].parse_mode, "HTML");
    assert_eq!(edits[1].parse_mode, "", "回落不带 parse mode");
    assert_eq!(edits[1].text, "**x**", "回落发原始 Markdown");
    assert_eq!(edits[0].message_id, edits[1].message_id, "同一个目标");
    assert_eq!(api.sends().len(), 0, "绝不另发一条");
}

/// 目标确认没了 ⇒ 允许另发（**唯一**的例外）；永久拒绝 ⇒ 停手并收口。
#[tokio::test]
async fn target_missing_allows_a_fresh_send_and_a_permanent_rejection_stops() {
    // 目标没了。
    let (api, _store, outbound) = harness();
    api.push_edit(Err(api_error(
        400,
        "Bad Request: message to edit not found",
    )));
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;
    let mut progress = AnswerProgress {
        streamed_message_id: 3,
        ..AnswerProgress::default()
    };
    let step = outbound
        .deliver_answer(&target, &lease, &["answer".to_string()], &mut progress)
        .await;
    assert_eq!(step, Step::RetryAfter(Duration::ZERO));
    assert!(progress.fresh_send);
    assert_eq!(progress.chunk_index, 0);
    let step = outbound
        .deliver_answer(&target, &lease, &["answer".to_string()], &mut progress)
        .await;
    assert_eq!(step, Step::done("delivered"));
    assert_eq!(api.sends().len(), 1, "另发一条新消息");

    // 永久拒绝（封禁 / 失权）。
    let (api, store, outbound) = harness();
    api.push_edit(Err(api_error(
        403,
        "Forbidden: bot was blocked by the user",
    )));
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;
    let mut progress = AnswerProgress {
        streamed_message_id: 3,
        ..AnswerProgress::default()
    };
    let step = outbound
        .deliver_answer(&target, &lease, &["answer".to_string()], &mut progress)
        .await;
    assert_eq!(
        step,
        Step::done("edit_rejected"),
        "停手，不收口成 delivered"
    );
    assert_eq!(
        store.row(target.delivery.task_id).settled_reason,
        "edit_rejected"
    );
    assert!(api.sends().is_empty(), "不重发");
    assert_eq!(api.edits().len(), 1, "也不空转");
}

/// 结果不明的编辑**有界**重试，用完就收口 `edit_failed`（不能永远占着会话队列）。
#[tokio::test]
async fn an_ambiguous_edit_is_retried_on_a_budget() {
    let (api, store, outbound) = harness();
    for _ in 0..MAX_AMBIGUOUS_EDIT_ATTEMPTS {
        api.push_edit(Err(ApiError::Transport {
            method: "editMessageText",
        }));
    }
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;
    let mut progress = AnswerProgress {
        streamed_message_id: 3,
        ..AnswerProgress::default()
    };
    let mut attempts = 0;
    loop {
        let step = outbound
            .deliver_answer(&target, &lease, &["answer".to_string()], &mut progress)
            .await;
        match step {
            Step::RetryAfter(_) => attempts += 1,
            Step::Done { ref reason } => {
                assert_eq!(reason, "edit_failed");
                break;
            }
            Step::Failed { ref message } => panic!("unexpected failure: {message}"),
        }
        assert!(attempts <= MAX_AMBIGUOUS_EDIT_ATTEMPTS + 1, "必须有界");
    }
    assert_eq!(api.edits().len(), MAX_AMBIGUOUS_EDIT_ATTEMPTS as usize);
    assert_eq!(
        store.row(target.delivery.task_id).settled_reason,
        "edit_failed"
    );
}

/// 发送结果**未知** ⇒ 停下并留证据（不重发：平台没有幂等键）。
#[tokio::test]
async fn an_unknown_send_outcome_stops_delivery() {
    let (api, store, outbound) = harness();
    api.push_send(Err(api_error(500, "Internal Server Error")));
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;
    let mut progress = AnswerProgress::default();
    let step = outbound
        .deliver_answer(&target, &lease, &["answer".to_string()], &mut progress)
        .await;
    assert_eq!(step, Step::done("send_result_unknown"));
    assert_eq!(api.sends().len(), 1, "结果未知不得重发");
    let row = store.row(target.delivery.task_id);
    assert!(row.is_send_unknown(), "证据留在行里");
    assert_eq!(row.settled_reason, "send_result_unknown");
}

// ---------------------------------------------------------------------------
// 失败告知
// ---------------------------------------------------------------------------

/// 失败告知把**占位消息**改成告知（同一个目标），不另发一条。
#[tokio::test]
async fn the_failure_notice_reuses_the_placeholder() {
    let (api, store, outbound) = harness();
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;
    outbound.push_partial(&target, &lease, "working", 0).await;
    let mut progress = AnswerProgress {
        streamed_message_id: 1,
        ..AnswerProgress::default()
    };
    let step = outbound
        .deliver_failure_notice(&target, &lease, TASK_FAILED_TEXT, 1, &mut progress)
        .await;
    assert_eq!(step, Step::done("failure_notice"));
    assert_eq!(api.sends().len(), 1, "不再新发");
    assert_eq!(api.edits().len(), 1);
    assert_eq!(api.edits()[0].message_id, 1);
    assert!(api.edits()[0].text.contains("agent run failed"));
    assert_eq!(
        store.row(target.delivery.task_id).settled_reason,
        "failure_notice"
    );
}

/// 没有占位消息 ⇒ 告知新发一条（并成为这一轮的可编辑消息）。
#[tokio::test]
async fn the_failure_notice_sends_fresh_when_the_target_is_gone() {
    let (api, store, outbound) = harness();
    api.push_edit(Err(api_error(
        400,
        "Bad Request: message to edit not found",
    )));
    let target = reply_target();
    let lease = take_lease(&outbound, &target, 0).await;
    let mut progress = AnswerProgress {
        streamed_message_id: 4,
        ..AnswerProgress::default()
    };
    let step = outbound
        .deliver_failure_notice(&target, &lease, TASK_FAILED_TEXT, 4, &mut progress)
        .await;
    assert_eq!(step, Step::RetryAfter(Duration::ZERO));
    let step = outbound
        .deliver_failure_notice(&target, &lease, TASK_FAILED_TEXT, 4, &mut progress)
        .await;
    assert_eq!(step, Step::done("failure_notice"));
    assert_eq!(api.sends().len(), 1);
    assert_eq!(api.sends()[0].text, TASK_FAILED_TEXT);
    assert_eq!(api.sends()[0].message_thread_id, 7);
    assert_eq!(
        store.row(target.delivery.task_id).message_id,
        "1",
        "告知成为这一轮的可编辑消息"
    );
}

// ---------------------------------------------------------------------------
// 编辑阶梯 / 目标解析（纯函数，表驱动）
// ---------------------------------------------------------------------------

/// 编辑阶梯的**顺序**就是语义：三个可恢复的 400 共用状态码，必须按序判定。
#[test]
fn edit_verdicts_are_table_driven() {
    let cases: &[(&str, EditVerdict)] = &[
        (
            "Bad Request: message is not modified",
            EditVerdict::NotModified,
        ),
        (
            "Bad Request: can't parse entities: unsupported start tag",
            EditVerdict::MarkupRefused,
        ),
        (
            "Bad Request: message to edit not found",
            EditVerdict::TargetMissing,
        ),
    ];
    for (description, want) in cases {
        let result: Result<(), ApiError> = Err(api_error(400, description));
        assert_eq!(classify_edit(&result), *want, "{description}");
    }
    assert_eq!(classify_edit(&Ok(())), EditVerdict::Applied);
    assert_eq!(
        classify_edit(&Err(api_error(403, "Forbidden"))),
        EditVerdict::PermanentRejection
    );
    assert_eq!(
        classify_edit(&Err(ApiError::Transport {
            method: "editMessageText"
        })),
        EditVerdict::Ambiguous
    );
    assert_eq!(
        classify_edit(&Err(api_error(500, "Internal Server Error"))),
        EditVerdict::Ambiguous
    );
}

/// 出站目标解析：渠道判别、config 里的 chat id、线程与引用、缺席为零值。
#[test]
fn reply_target_resolution_is_table_driven() {
    use mc_repos::channel::delivery::ChannelTaskDeliveryRow;

    let row = |chat: &str, config: serde_json::Value, thread: Option<&str>, msg: Option<&str>| {
        ChannelTaskDeliveryRow {
            task_id: uuid::Uuid::new_v4(),
            binding_id: uuid::Uuid::new_v4(),
            installation_id: uuid::Uuid::new_v4(),
            channel_type: "telegram".to_string(),
            channel_chat_id: chat.to_string(),
            chat_type: "group".to_string(),
            channel_message_id: msg.map(str::to_owned),
            channel_thread_id: thread.map(str::to_owned),
            route_revision: 1,
            config,
            created_at: chrono::Utc::now(),
        }
    };
    let token = || Sensitive::new("123456:not-a-real-bot-token-itest-only");

    // 复合绑定键 ⇒ 数值 chat id 从 config 里取；线程与引用按原样解析。
    let resolved = ReplyTarget::from_task_delivery(
        &row(
            "chat:thread",
            json!({ "chat_id": "-100123" }),
            Some("7"),
            Some("-100123:55"),
        ),
        token(),
        ChannelKind::Telegram,
    )
    .expect("resolved");
    assert_eq!(resolved.chat_id, -100_123);
    assert_eq!(resolved.thread_id, 7);
    assert_eq!(resolved.reply_to, 55, "复合键取冒号之后");
    assert_eq!(
        resolved.bot_key,
        resolved.delivery.installation_id.to_string()
    );

    // 没有 config / 没有线程 / 没有引用 ⇒ 回落与零值。
    let plain = ReplyTarget::from_task_delivery(
        &row("4242", json!({}), None, None),
        token(),
        ChannelKind::Telegram,
    )
    .expect("resolved");
    assert_eq!(plain.chat_id, 4242);
    assert_eq!(plain.thread_id, 0);
    assert_eq!(plain.reply_to, 0);

    // config 里是空串 ⇒ 视作缺席（上游 `cfg.ChatID != ""`）。
    let empty = ReplyTarget::from_task_delivery(
        &row("4242", json!({ "chat_id": "" }), None, None),
        token(),
        ChannelKind::Telegram,
    )
    .expect("resolved");
    assert_eq!(empty.chat_id, 4242);

    // 别的渠道的任务**不许**被送进外部会话（哪怕这个 chat 曾经有过这条路由）。
    assert!(ReplyTarget::from_task_delivery(
        &row("4242", json!({}), None, None),
        token(),
        ChannelKind::Slack
    )
    .is_none());
}

/// 目标 / 令牌的 `Debug` 不得回显凭据（§2.3 第 2 条）。
#[test]
fn debug_output_never_carries_the_token() {
    let target = reply_target();
    let rendered = format!("{target:?}");
    assert!(rendered.contains("Sensitive(<redacted>)"));
    assert!(!rendered.contains("not-a-real-bot-token"));
}

/// 分片计划：先分片再渲染（代码围栏不跨片）。
#[test]
fn chunk_planning_splits_utf16_units() {
    let chunks = Outbound::plan_chunks("😀".repeat(2000).as_str());
    assert!(chunks.len() > 1, "4000 码元必然超上限");
    for chunk in &chunks {
        assert!(utf16_units(chunk) <= MAX_MESSAGE_UNITS);
    }
    assert_eq!(first_non_empty("", STREAM_PLACEHOLDER), STREAM_PLACEHOLDER);
    assert_eq!(first_non_empty("x", STREAM_PLACEHOLDER), "x");
}
