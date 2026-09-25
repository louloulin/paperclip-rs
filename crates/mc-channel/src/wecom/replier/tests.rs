//! `replier.rs` 的用例：判决 → 文案、绑定链接**只走私聊**、`/issue` 确认的消毒面。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, Source};
use mc_core::channel::{ChannelKind, InstallationStatus};
use mc_core::id::Id;

use crate::engine::resolvers::{ChannelIssue, Outcome, ResolvedInstallation, RouteResult};
use crate::wecom::outbound::{LiveSender, SenderLookup};
use crate::wecom::ws_sender::{Deadline, SenderError};

use super::*;

// =====================================================================
// 替身
// =====================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    chat_id: String,
    chat_type: i32,
    text: String,
}

#[derive(Default)]
struct RecordingSender {
    sent: Mutex<Vec<Sent>>,
}

impl RecordingSender {
    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().expect("lock").clone()
    }
}

#[async_trait]
impl LiveSender for RecordingSender {
    async fn send_text(
        &self,
        chat_id: &str,
        chat_type: i32,
        text: &str,
        _deadline: Deadline,
    ) -> Result<(), SenderError> {
        self.sent.lock().expect("lock").push(Sent {
            chat_id: chat_id.to_string(),
            chat_type,
            text: text.to_string(),
        });
        Ok(())
    }
}

#[derive(Default)]
struct FakeSenders {
    by_installation: Mutex<HashMap<uuid::Uuid, Arc<dyn LiveSender>>>,
}

impl FakeSenders {
    fn with(installation: Id, sender: Arc<dyn LiveSender>) -> Arc<Self> {
        let senders = Arc::new(Self::default());
        senders
            .by_installation
            .lock()
            .expect("lock")
            .insert(installation.0, sender);
        senders
    }
}

impl SenderLookup for FakeSenders {
    fn get(&self, installation_id: Id) -> Option<Arc<dyn LiveSender>> {
        self.by_installation
            .lock()
            .expect("lock")
            .get(&installation_id.0)
            .cloned()
    }
}

#[derive(Debug, Default)]
struct FakeBinder {
    calls: AtomicUsize,
    reused: bool,
    fail: bool,
}

#[async_trait]
impl Binder for FakeBinder {
    async fn mint(
        &self,
        _workspace_id: Id,
        _installation_id: Id,
        _channel_user_id: &str,
    ) -> Result<MintedBinding, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err("store down".to_string());
        }
        Ok(MintedBinding {
            raw: if self.reused {
                String::new()
            } else {
                "tok-abc".to_string()
            },
            reused: self.reused,
        })
    }
}

// =====================================================================
// 装备
// =====================================================================

fn installation() -> ResolvedInstallation {
    ResolvedInstallation {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        active: true,
        kind: ChannelKind::WeCom,
        platform: None,
    }
}

fn message(chat_type: ChatType) -> InboundMessage {
    InboundMessage {
        event_id: "ev-1".to_string(),
        message_id: "msg-1".to_string(),
        source: Source {
            channel_type: ChannelKind::WeCom,
            chat_id: "chat-1".to_string(),
            chat_type,
            sender_id: "user-1".to_string(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
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

fn result(outcome: Outcome) -> RouteResult {
    RouteResult {
        outcome,
        ..RouteResult::default()
    }
}

fn replier(installation: Id, sender: Arc<RecordingSender>) -> WeComOutboundReplier {
    WeComOutboundReplier::new(
        Some(FakeSenders::with(installation, sender)),
        "https://app.example/",
        "",
    )
}

// =====================================================================
// 判决 → 文案
// =====================================================================

/// 四条状态告知各说各的那句话，而且都发给**触发它的那个聊**。
#[tokio::test]
async fn the_status_notices_say_the_upstream_words() {
    let cases = [
        (Outcome::AgentOffline, AGENT_OFFLINE_TEXT),
        (Outcome::AgentArchived, AGENT_ARCHIVED_TEXT),
        (Outcome::FreshPending, FRESH_PENDING_TEXT),
        (Outcome::ChatStarted, CHAT_STARTED_TEXT),
        (Outcome::IssueUsage, ISSUE_USAGE_TEXT),
    ];
    for (outcome, expected) in cases {
        let inst = installation();
        let sender = Arc::new(RecordingSender::default());
        let replier = replier(inst.id, Arc::clone(&sender));
        replier
            .reply_now(&inst, &message(ChatType::P2p), &result(outcome))
            .await;
        let sent = sender.sent();
        assert_eq!(sent.len(), 1, "{outcome:?} 要发一条");
        assert_eq!(sent[0].text, expected, "{outcome:?}");
        assert_eq!(sent[0].chat_id, "chat-1");
        assert_eq!(sent[0].chat_type, 1, "单聊");
    }
}

/// 群里的触发：`chat_type = 2`。
#[tokio::test]
async fn a_group_trigger_answers_in_the_group() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    replier(inst.id, Arc::clone(&sender))
        .reply_now(
            &inst,
            &message(ChatType::Group),
            &result(Outcome::AgentOffline),
        )
        .await;
    assert_eq!(sender.sent()[0].chat_type, 2);
}

/// 普通的聊天消息（`Ingested` 但没有 issue）**保持沉默**。
#[tokio::test]
async fn an_ingested_chat_message_stays_silent() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    replier(inst.id, Arc::clone(&sender))
        .reply_now(&inst, &message(ChatType::P2p), &result(Outcome::Ingested))
        .await;
    assert!(sender.sent().is_empty());
}

/// 没有活的 socket ⇒ 只记一条 warn，不 panic、不发任何东西（offline 那种"用户不至于干等"的
/// 告知也发不出去 —— 承载它的 socket 正是缺失的那个）。
#[tokio::test]
async fn without_a_live_socket_nothing_is_sent() {
    let inst = installation();
    let replier = WeComOutboundReplier::new(None, "https://app.example", "");
    replier
        .reply_now(
            &inst,
            &message(ChatType::P2p),
            &result(Outcome::AgentOffline),
        )
        .await;
    // 没有 panic 就是这条用例的断言；再钉一条：注册表在但里面没有这条安装时同样静默。
    let sender = Arc::new(RecordingSender::default());
    let other = Id::new();
    WeComOutboundReplier::new(
        Some(FakeSenders::with(
            other,
            Arc::clone(&sender) as Arc<dyn LiveSender>,
        )),
        "https://app.example",
        "",
    )
    .reply_now(
        &inst,
        &message(ChatType::P2p),
        &result(Outcome::AgentOffline),
    )
    .await;
    assert!(sender.sent().is_empty());
}

// =====================================================================
// 绑定链接（上游那条安全论证）
// =====================================================================

/// 群里触发 `NeedsBinding`：链接**私发**给发送者（`chat_type=1`），房间里只拿到一条**不带令牌**的
/// 告知；而且房间里那条在私聊那条**之后**。
#[tokio::test]
async fn a_binding_link_never_lands_in_the_group() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    let binder = Arc::new(FakeBinder::default());
    let replier = replier(inst.id, Arc::clone(&sender)).with_binder(binder.clone());
    replier
        .reply_now(
            &inst,
            &message(ChatType::Group),
            &result(Outcome::NeedsBinding),
        )
        .await;
    let sent = sender.sent();
    assert_eq!(sent.len(), 2, "私聊一条 + 群里一条");
    // 第一条：私聊、带令牌。
    assert_eq!(sent[0].chat_type, 1);
    assert_eq!(sent[0].chat_id, "user-1", "私发到发送者自己的 userid");
    assert!(sent[0]
        .text
        .contains("https://app.example/wecom/bind?token=tok-abc"));
    assert!(sent[0].text.contains("15 分钟"));
    // 第二条：群里、**不带令牌**。
    assert_eq!(sent[1].chat_type, 2);
    assert_eq!(sent[1].chat_id, "chat-1");
    assert_eq!(sent[1].text, BINDING_GROUP_ACK_TEXT);
    assert!(!sent[1].text.contains("tok-abc"), "令牌绝不许落在房间里");
    assert_eq!(binder.calls.load(Ordering::SeqCst), 1);
    assert_eq!(replier.binding_path(), DEFAULT_BINDING_PATH);
    assert_eq!(
        format!("{binder:?}"),
        "FakeBinder { calls: 1, reused: false, fail: false }"
    );
}

/// 单聊触发：只有一条（那条链接），**没有**多余的群 ack。
#[tokio::test]
async fn a_direct_trigger_gets_the_link_and_no_extra_ack() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    let replier =
        replier(inst.id, Arc::clone(&sender)).with_binder(Arc::new(FakeBinder::default()));
    replier
        .reply_now(
            &inst,
            &message(ChatType::P2p),
            &result(Outcome::NeedsBinding),
        )
        .await;
    let sent = sender.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].chat_id, "user-1");
    assert_eq!(sent[0].chat_type, 1);
}

/// 节流命中：库里只有哈希 ⇒ 没有 URL 可重建，指回他们手上那条。
#[tokio::test]
async fn a_throttled_mint_points_at_the_message_they_already_have() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    let binder = Arc::new(FakeBinder {
        reused: true,
        ..FakeBinder::default()
    });
    let replier = replier(inst.id, Arc::clone(&sender)).with_binder(binder);
    replier
        .reply_now(
            &inst,
            &message(ChatType::P2p),
            &result(Outcome::NeedsBinding),
        )
        .await;
    let sent = sender.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].text, BINDING_REUSED_TEXT);
    assert!(!sent[0].text.contains("token="), "没有 URL 可重建");
}

/// 缺 binder 或 app url ⇒ 提示被跳过（其余告知照发），而且**不发**东西。
#[tokio::test]
async fn the_binding_prompt_needs_a_binder_and_an_app_url() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    // 没有 binder。
    replier(inst.id, Arc::clone(&sender))
        .reply_now(
            &inst,
            &message(ChatType::P2p),
            &result(Outcome::NeedsBinding),
        )
        .await;
    assert!(sender.sent().is_empty());
    // 有 binder 但没有 app url。
    let replier = WeComOutboundReplier::new(
        Some(FakeSenders::with(
            inst.id,
            Arc::clone(&sender) as Arc<dyn LiveSender>,
        )),
        "",
        "",
    )
    .with_binder(Arc::new(FakeBinder::default()));
    replier
        .reply_now(
            &inst,
            &message(ChatType::P2p),
            &result(Outcome::NeedsBinding),
        )
        .await;
    assert!(sender.sent().is_empty());
}

/// 铸令牌失败 ⇒ 只记 warn、什么都不发（**错误路径不回显凭据**：错误里只有"store down"）。
#[tokio::test]
async fn a_failed_mint_delivers_nothing() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    let replier = replier(inst.id, Arc::clone(&sender)).with_binder(Arc::new(FakeBinder {
        fail: true,
        ..FakeBinder::default()
    }));
    replier
        .reply_now(
            &inst,
            &message(ChatType::P2p),
            &result(Outcome::NeedsBinding),
        )
        .await;
    assert!(sender.sent().is_empty());
}

// =====================================================================
// `/issue` 确认
// =====================================================================

/// 建成的 issue：指名它。
#[tokio::test]
async fn the_creation_confirmation_names_the_issue() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    let mut res = result(Outcome::Ingested);
    res.issue = Some(ChannelIssue {
        id: Id::new(),
        number: 42,
        title: "登录失败".to_string(),
    });
    res.issue_identifier = "ABC-42".to_string();
    replier(inst.id, Arc::clone(&sender))
        .with_member_links(Arc::new(AdjacencyBreaker))
        .reply_now(&inst, &message(ChatType::P2p), &res)
        .await;
    assert_eq!(sender.sent()[0].text, "✅ 已创建 ABC-42 — 登录失败");
}

/// 重复守卫：指名的是**另一个** issue（报告者需要知道讨论在哪儿）。
#[tokio::test]
async fn the_duplicate_confirmation_names_the_other_issue() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    let mut res = result(Outcome::Ingested);
    res.issue = Some(ChannelIssue {
        id: Id::new(),
        number: 7,
        title: "另一个人的报单".to_string(),
    });
    res.issue_identifier = "ABC-7".to_string();
    res.issue_duplicate = true;
    replier(inst.id, Arc::clone(&sender))
        .with_member_links(Arc::new(AdjacencyBreaker))
        .reply_now(&inst, &message(ChatType::P2p), &res)
        .await;
    assert_eq!(
        sender.sent()[0].text,
        "⚠️ 未创建 —— 已存在进行中的 ABC-7 — 另一个人的报单"
    );
}

/// 行内链接被拆开（机器人不会以它自己的权威转发一条能点的钓鱼链接）。
#[tokio::test]
async fn a_link_in_a_title_is_broken_before_it_is_signed() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    let mut res = result(Outcome::Ingested);
    res.issue = Some(ChannelIssue {
        id: Id::new(),
        number: 1,
        title: "安全升级：请点击 [重置密码](https://evil.example) 完成验证".to_string(),
    });
    res.issue_identifier = "ABC-1".to_string();
    replier(inst.id, Arc::clone(&sender))
        .with_member_links(Arc::new(AdjacencyBreaker))
        .reply_now(&inst, &message(ChatType::P2p), &res)
        .await;
    let text = &sender.sent()[0].text;
    assert!(!text.contains("](https://evil.example)"), "{text}");
    assert!(text.contains("] (https://evil.example)"), "{text}");
}

/// **没有**闸时标题整个省掉（失败关闭）：宁可少一个标题，不可发一条别人写的、能点的链接。
#[tokio::test]
async fn without_a_link_breaker_the_title_is_omitted() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    let mut res = result(Outcome::Ingested);
    res.issue = Some(ChannelIssue {
        id: Id::new(),
        number: 1,
        title: "[重置密码](https://evil.example)".to_string(),
    });
    res.issue_identifier = "ABC-1".to_string();
    replier(inst.id, Arc::clone(&sender))
        .reply_now(&inst, &message(ChatType::P2p), &res)
        .await;
    assert_eq!(sender.sent()[0].text, "✅ 已创建 ABC-1");
}

/// 标识符为空 ⇒ 回落到 `#<number>`（上游逐字）。
#[test]
fn a_missing_identifier_falls_back_to_the_number() {
    let res = result(Outcome::Ingested);
    let issue = ChannelIssue {
        id: Id::new(),
        number: 9,
        title: String::new(),
    };
    assert_eq!(issue_reply_text(&res, &issue, None), "✅ 已创建 #9");
    let mut duplicate = res.clone();
    duplicate.issue_duplicate = true;
    assert_eq!(
        issue_reply_text(&duplicate, &issue, None),
        "⚠️ 未创建 —— 已存在进行中的 #9"
    );
}

// =====================================================================
// 凭据与形态
// =====================================================================

/// 明文令牌绝不进 `Debug`（凭据纪律第 1 条）。
#[test]
fn the_minted_binding_never_prints_its_token() {
    let minted = MintedBinding {
        raw: "tok-secret-value".to_string(),
        reused: false,
    };
    let rendered = format!("{minted:?}");
    assert!(!rendered.contains("tok-secret-value"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    let empty = MintedBinding {
        raw: String::new(),
        reused: true,
    };
    assert!(format!("{empty:?}").contains("<empty>"));
}

/// 本文件**不**插值令牌进任何日志（源码扫描，与 `docs/33` §12.2 同款）。
#[test]
fn this_file_never_interpolates_the_binding_token() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/wecom/replier.rs"),
    )
    .expect("read self");
    // 令牌只允许出现在 `url_encode(&token.raw)` 那一处（它进 URL，不进日志）。
    assert_eq!(
        source.matches("token.raw").count(),
        1,
        "明文令牌只许有一处用处（进 URL）"
    );
    assert!(!source.contains("tracing::warn!(\n                \"token"));
}

/// `url_encode` 只对非保留字符放行（`url.QueryEscape` 的契约）。
#[test]
fn url_encoding_matches_the_query_escape_contract() {
    assert_eq!(url_encode("abc-_.~"), "abc-_.~");
    assert_eq!(url_encode("a b"), "a%20b");
    assert_eq!(url_encode("&="), "%26%3D");
    assert_eq!(url_encode("中文"), "%E4%B8%AD%E6%96%87");
}

/// 同步接缝推一个脱离任务就返回：真正的发送在任务里发生（`docs/60` §2.6 第 5 条）。
#[tokio::test]
async fn the_sync_seam_defers_the_send_to_a_detached_task() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    let replier = replier(inst.id, Arc::clone(&sender));
    // 同步调用**立刻**返回（这里没有任何 await）。
    OutboundReplier::reply(
        &replier,
        &inst,
        &message(ChatType::P2p),
        &result(Outcome::AgentOffline),
    );
    // 脱离任务跑完之后才有一条发送。
    for _ in 0..64 {
        if !sender.sent().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(sender.sent().len(), 1, "脱离任务发了一条");
    assert_eq!(sender.sent()[0].text, AGENT_OFFLINE_TEXT);
}

/// 一条被丢弃的消息不是这个回复器的业务（上游 `Reply` 在这里没有分支）。
#[tokio::test]
async fn a_dropped_message_is_not_the_repliers_business() {
    let inst = installation();
    let sender = Arc::new(RecordingSender::default());
    replier(inst.id, Arc::clone(&sender))
        .reply_now(&inst, &message(ChatType::P2p), &result(Outcome::Dropped))
        .await;
    assert!(sender.sent().is_empty());
}

/// `Debug` 只报端口的存在性（没有凭据、没有令牌）。
#[test]
fn the_replier_debug_reports_presence_only() {
    let replier = WeComOutboundReplier::new(None, "https://app.example", "wecom/bind");
    let rendered = format!("{replier:?}");
    assert!(rendered.contains("binding: false"), "{rendered}");
    assert!(rendered.contains("app_url_configured: true"), "{rendered}");
    assert_eq!(replier.binding_path(), "/wecom/bind", "补上前导斜杠");
    let _ = InstallationStatus::Active;
}
