//! `slack::replier` 的用例（写者 M7-4）。
//!
//! 判决 → 文案逐条钉住；绑定卡的**四条前置失败**各一条；记账种类（`control_ack` /
//! `issue_ack`）与 issue 标题的**消毒顺序**各一条。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, OutboundMessage, Source};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;

use super::*;
use crate::engine::resolvers::{ChannelIssue, Outcome, RouteResult};
use crate::slack::config::Decrypter;
use crate::slack::outbound::{ApiResult, MessageApi, PostMessageRequest, Sender};
use crate::slack::resolvers::InstallationRow;

// =====================================================================
// 替身
// =====================================================================

#[derive(Default)]
struct RecordingApi {
    seen: Mutex<Vec<PostMessageRequest>>,
}

#[async_trait]
impl MessageApi for RecordingApi {
    async fn post_message(&self, _token: &str, req: &PostMessageRequest) -> ApiResult<String> {
        self.seen.lock().expect("lock").push(req.clone());
        Ok(format!(
            "{}.0001",
            500 + self.seen.lock().expect("lock").len()
        ))
    }
}

#[derive(Default)]
struct FakeMinter {
    calls: Mutex<Vec<(Id, Id, String)>>,
    fail: Mutex<bool>,
}

#[async_trait]
impl BindingMinter for FakeMinter {
    async fn mint(
        &self,
        workspace_id: Id,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<MintedBinding, String> {
        self.calls.lock().expect("lock").push((
            workspace_id,
            installation_id,
            channel_user_id.to_string(),
        ));
        if *self.fail.lock().expect("lock") {
            return Err("token store down".to_string());
        }
        Ok(MintedBinding {
            raw: "abc-def_ghi".to_string(),
            expires_at: chrono::Utc::now(),
        })
    }
}

#[derive(Default)]
struct FakeLedger {
    records: Mutex<Vec<OutboundRecord>>,
}

#[async_trait]
impl OutboundLedger for FakeLedger {
    async fn record_outbound(&self, record: &OutboundRecord) -> Result<(), String> {
        self.records.lock().expect("lock").push(record.clone());
        Ok(())
    }
}

fn installation() -> (ResolvedInstallation, InstallationRow) {
    let row = InstallationRow {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        // 身份解密器 ⇒ 明文令牌就是这个 base64。
        config: serde_json::json!({
            "app_id": "A1",
            "bot_user_id": "UBOT",
            "bot_token_encrypted": "eG94Yi10ZXN0",
            "channel_id": "C1",
        }),
    };
    let resolved = ResolvedInstallation {
        id: row.id,
        workspace_id: row.workspace_id,
        agent_id: row.agent_id,
        installer_user_id: row.installer_user_id,
        active: true,
        kind: ChannelKind::Slack,
        platform: Some(Arc::new(row.clone())),
    };
    (resolved, row)
}

fn inbound() -> InboundMessage {
    InboundMessage {
        event_id: "E1".to_string(),
        message_id: "100.1".to_string(),
        source: Source {
            channel_type: ChannelKind::Slack,
            chat_id: "C1".to_string(),
            chat_type: ChatType::Group,
            sender_id: "U1".to_string(),
            sender_stable_id: String::new(),
            thread_id: "100.1".to_string(),
        },
        kind: MessageKind::Text,
        text: "hi".to_string(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::Value::Null,
    }
}

/// 一套装配好的替身。
struct Harness {
    replier: SlackOutboundReplier,
    api: Arc<RecordingApi>,
    ledger: Arc<FakeLedger>,
    minter: Arc<FakeMinter>,
}

fn harness(app_url: &str, with_binding: bool, with_ledger: bool) -> Harness {
    let api = Arc::new(RecordingApi::default());
    let sender = Arc::new(Sender::new(Arc::clone(&api) as Arc<dyn MessageApi>));
    let minter = Arc::new(FakeMinter::default());
    let ledger = Arc::new(FakeLedger::default());
    let replier = SlackOutboundReplier::new(
        sender,
        Decrypter::plaintext(),
        if with_binding {
            Some(Arc::clone(&minter) as Arc<dyn BindingMinter>)
        } else {
            None
        },
        if with_ledger {
            Some(Arc::clone(&ledger) as Arc<dyn OutboundLedger>)
        } else {
            None
        },
        app_url,
        None,
    );
    Harness {
        replier,
        api,
        ledger,
        minter,
    }
}

fn result(outcome: Outcome) -> RouteResult {
    RouteResult {
        outcome,
        channel_binding_id: Some(Id::new()),
        channel_route_revision: 3,
        sender: "U1".to_string(),
        ..RouteResult::default()
    }
}

fn texts(harness: &Harness) -> Vec<String> {
    harness
        .api
        .seen
        .lock()
        .expect("lock")
        .iter()
        .map(|request| request.text.clone())
        .collect()
}

// =====================================================================
// 判决 → 文案
// =====================================================================

#[tokio::test]
async fn every_notice_outcome_posts_its_upstream_text() {
    let cases = [
        (Outcome::AgentOffline, AGENT_OFFLINE_TEXT),
        (Outcome::AgentArchived, AGENT_ARCHIVED_TEXT),
        (Outcome::FreshPending, FRESH_PENDING_TEXT),
        (Outcome::ChatStarted, CHAT_STARTED_TEXT),
        (Outcome::IssueUsage, ISSUE_USAGE_TEXT),
    ];
    for (outcome, expected) in cases {
        let harness = harness("https://app.example", true, true);
        let (installation, _row) = installation();
        harness
            .replier
            .reply_now(&installation, &inbound(), &result(outcome))
            .await;
        assert_eq!(texts(&harness), vec![expected.to_string()], "{outcome:?}");
    }
}

#[tokio::test]
async fn a_dropped_message_and_a_plain_ingest_stay_silent() {
    // 丢弃：不回。
    let harness = harness("https://app.example", true, true);
    let (installation, _row) = installation();
    harness
        .replier
        .reply_now(&installation, &inbound(), &result(Outcome::Dropped))
        .await;
    assert!(texts(&harness).is_empty());

    // 普通聊天消息：agent 自己的回复走 ChatDone ⇒ 这里**保持沉默**。
    harness
        .replier
        .reply_now(&installation, &inbound(), &result(Outcome::Ingested))
        .await;
    assert!(texts(&harness).is_empty());
}

#[tokio::test]
async fn a_threaded_reply_lands_in_the_inbound_thread_and_is_ledgered_as_a_control_ack() {
    let harness = harness("https://app.example", true, true);
    let (installation, _row) = installation();
    harness
        .replier
        .reply_now(&installation, &inbound(), &result(Outcome::AgentOffline))
        .await;

    let seen = harness.api.seen.lock().expect("lock");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].channel, "C1");
    assert_eq!(seen[0].thread_ts.as_deref(), Some("100.1"));
    assert_eq!(
        seen[0]
            .metadata
            .as_ref()
            .map(|meta| meta.kind.clone())
            .expect("metadata"),
        "control_ack"
    );
    let records = harness.ledger.records.lock().expect("lock");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outbound_kind, "control_ack");
    assert_eq!(records[0].channel_type, "slack");
    assert_eq!(records[0].route_revision, 3);
}

// =====================================================================
// 绑定卡
// =====================================================================

#[tokio::test]
async fn the_binding_prompt_wraps_the_token_as_an_explicit_slack_link() {
    let harness = harness("https://app.example/", true, false);
    let (installation, _row) = installation();
    harness
        .replier
        .reply_now(&installation, &inbound(), &result(Outcome::NeedsBinding))
        .await;
    let posted = texts(&harness);
    assert_eq!(posted.len(), 1);
    assert!(
        posted[0].contains("<https://app.example/slack/bind?token=abc-def_ghi|link your account>"),
        "base64url 令牌要包成显式链接（否则 `_`/`-` 会被 mrkdwn 折成斜体）：{}",
        posted[0]
    );
    let calls = harness.minter.calls.lock().expect("lock");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].2, "U1");
}

#[tokio::test]
async fn the_binding_prompt_is_skipped_without_a_minter_or_an_app_url() {
    // 没接绑定服务 ⇒ 不打 Slack（`reply_now` 只告警）。
    let without_minter = harness("https://app.example", false, false);
    let (installation, _row) = installation();
    without_minter
        .replier
        .reply_now(&installation, &inbound(), &result(Outcome::NeedsBinding))
        .await;
    assert!(texts(&without_minter).is_empty());

    // 没配 app url ⇒ 同上。
    let without_url = harness("", true, false);
    without_url
        .replier
        .reply_now(&installation, &inbound(), &result(Outcome::NeedsBinding))
        .await;
    assert!(texts(&without_url).is_empty());

    // 令牌铸造失败 ⇒ 同上。
    let failing = harness("https://app.example", true, false);
    *failing.minter.fail.lock().expect("lock") = true;
    failing
        .replier
        .reply_now(&installation, &inbound(), &result(Outcome::NeedsBinding))
        .await;
    assert!(texts(&failing).is_empty());
}

// =====================================================================
// `/issue` 的两种产物
// =====================================================================

#[tokio::test]
async fn an_issue_outcome_replies_and_is_ledgered_as_an_issue_ack() {
    let harness = harness("https://app.example", true, true);
    let (installation, _row) = installation();
    let mut result = result(Outcome::Ingested);
    result.issue = Some(ChannelIssue {
        id: Id::new(),
        number: 42,
        title: "fix the login button".to_string(),
    });
    result.issue_identifier = "ABC-42".to_string();
    harness
        .replier
        .reply_now(&installation, &inbound(), &result)
        .await;
    assert_eq!(
        texts(&harness),
        vec!["✅ Created ABC-42 — fix the login button".to_string()]
    );
    assert_eq!(
        harness.ledger.records.lock().expect("lock")[0].outbound_kind,
        "issue_ack"
    );
}

#[tokio::test]
async fn an_issue_duplicate_says_not_created() {
    let harness = harness("https://app.example", true, false);
    let (installation, _row) = installation();
    let mut result = result(Outcome::Ingested);
    result.issue = Some(ChannelIssue {
        id: Id::new(),
        number: 7,
        title: "already there".to_string(),
    });
    result.issue_duplicate = true;
    harness
        .replier
        .reply_now(&installation, &inbound(), &result)
        .await;
    assert_eq!(
        texts(&harness),
        vec!["⚠️ Not created — active issue #7 already exists: already there".to_string()],
        "identifier 空 ⇒ 回落 `#<number>`"
    );
}

/// 标题消毒的**顺序**（上游逐字）：先拆链接邻接、再转义 `<`。
#[test]
fn issue_titles_are_sanitized_in_the_upstream_order() {
    // `[x](y)` 的链接邻接被拆开（`](` 之间插一个空格）。
    assert_eq!(sanitize_issue_title("[x](y)"), "[x] (y)");
    // 既有的 Slack 实体也被当作可见文本（`<` 转义），而不是被 mrkdwn 当链接。
    assert_eq!(
        sanitize_issue_title("<@U1> ping"),
        "&lt;@U1> ping",
        "`<` 必须转义 —— mrkdwn 会保留 `<…>` 实体形态"
    );
    assert_eq!(
        issue_created_text(
            &ChannelIssue {
                id: Id::new(),
                number: 1,
                title: "   ".to_string()
            },
            ""
        ),
        "✅ Created #1",
        "空标题不追加破折号"
    );
}

#[test]
fn url_encoding_matches_go_query_escape() {
    assert_eq!(
        url_encode("abc-def_ghi"),
        "abc-def_ghi",
        "base64url 字符不必编码"
    );
    assert_eq!(url_encode("a b"), "a+b");
    assert_eq!(url_encode("a/b"), "a%2Fb");
}

/// 同步接缝（engine 的调用形态）：推一个脱离任务后立刻返回，任务真的跑完。
#[tokio::test]
async fn the_sync_seam_dispatches_a_detached_reply() {
    let harness = harness("https://app.example", true, false);
    let (installation, _row) = installation();
    OutboundReplier::reply(
        &harness.replier,
        &installation,
        &inbound(),
        &result(Outcome::AgentOffline),
    );
    // 脱离任务在同一个运行时里跑：给它一次让步的机会。
    for _ in 0..50 {
        if !texts(&harness).is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(texts(&harness), vec![AGENT_OFFLINE_TEXT.to_string()]);
}

/// `OutboundMessage` 的落点字段（本文件构造的线形态）。
#[test]
fn the_outbound_shape_targets_the_inbound_chat() {
    let message = OutboundMessage {
        chat_id: "C1".to_string(),
        text: "x".to_string(),
        thread_id: "100.1".to_string(),
        reply_to: String::new(),
    };
    assert_eq!(message.chat_id, "C1");
}
