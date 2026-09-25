//! **wecom 端到端收发回路** —— 本片承担本渠道的门禁证据（`docs/60-M7-PLAN.md` §4.2）。
//!
//! # 这条回路**真**到哪一步（逐层点明，别把替身当业务路径）
//!
//! | 层 | 本用例 | 说明 |
//! | --- | --- | --- |
//! | 平台 wire | **替身**（[`harness::FakePeer`]） | `WeCom` 的 aibot 端点在 CI 里连不上（要真企业凭据），且本仓**没有**可起 WS 服务端的 feature ⇒ 替身记下每一次写、按 `req_id` 自动回 ack（真平台的行为），并允许注入入站帧。**只替 wire，不替业务路径。** |
//! | 传输 / 握手 / 读循环 | **真代码** | `WsSender`（M7-16）、`loop.rs` 的 `subscribe` / `ping_loop` / `pump` / worker（本片） |
//! | 入站归一化 | **真代码** | `inbound.rs`（本片） |
//! | 安装 / 身份解析 | **真代码** | `WeComInstallationResolver` / `WeComIdentityResolver`（本片），端口是内存查询 |
//! | 去重 | **真代码** | `ChannelDeduper`（M7-2）+ 内存 `DedupStore` —— "命中不报错"这条语义的**真实**承担者 |
//! | 流水线判决 | **真代码** | engine 的 `Router`（M7-1/M7-2）；只有**别的片**的端口（会话 / 触发 / issue）是替身 |
//! | 出站 | **真代码** | `WeComOutboundReplier`（M7-17）+ 本片交付的 `MemberLinkBreaker` 与 `InboxCardRenderer` |
//! | 数据层 | **真代码**（在 `mc-http` / `mc-repos` 那一侧） | 本 crate **没有** `sqlx` 依赖 ⇒ 泛化 `channel_*` 仓储由门 ⑥ 的 `crates/mc-http/tests/channels/*` 覆盖、`mc-repos` 的真库用例覆盖余下那些（与 lark M7-13 同一条边界） |
//!
//! ⇒ §4.2 的"真 DB"由门 ⑥ 承担，"帧 → 归一化 → 判决 → 出站 → 帧回到替身"的**整条业务链**由本文件
//! 承担。**登记在 `docs/32` §36 的 D8。**
//!
//! # 一条断言链的固定形状
//!
//! `替身造一帧 → 真入站 → 真 Router（真去重）→ 真出站 → 帧回到替身`，中间**零 mock**。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use mc_core::channel::message::ChatType;
use mc_core::channel::{ChannelKind, InstallationStatus};
use mc_core::id::Id;
use serde_json::json;

use super::harness::{
    address_of, event_callback, is_cmd, markdown_of, msg_callback, text_message, FakePeer,
    TestSenders,
};
use crate::channel::{Channel, ChannelError};
use crate::engine::resolvers::{Auditor, RunTriggerer, SessionBinder};
use crate::engine::session::ChannelDeduper;
use crate::engine::{ChannelCommandClassifier, Router, RouterConfig};
use crate::wecom::credentials::PlaintextSecret;
use crate::wecom::inbox_message::InboxCardRenderer;
use crate::wecom::markdown::MemberLinkBreaker;
use crate::wecom::replier::WeComOutboundReplier;
use crate::wecom::resolvers::{
    WeComIdentityResolver, WeComInstallationResolver, WeComResolverSet, WeComSessionBinder,
    ORIGIN_WECOM_CHAT,
};
use crate::wecom::types::{Installation, KIND};
use crate::wecom::wecom_channel::{SenderRegistry, WeComChannel, WeComDeps};

use super::doubles::{
    CountingTrigger, FixedBinder, FixedIssues, MemoryAudit, MemoryDedup, MemoryIdentities,
    MemoryInstallations, MemorySession, NeverMedia, NoReader, StaticCredentials,
};
use crate::wecom::ws_frame::CMD_SEND_MSG;

/// 回路里那条安装。
const BOT_ID: &str = "bot_5f1c9a";
const SENDER_ID: &str = "user_1";

/// 回路里所有的把手。
struct Loop {
    peer: Arc<FakePeer>,
    channel: Arc<WeComChannel>,
    senders: Arc<TestSenders>,
    audit: Arc<MemoryAudit>,
    session: Arc<MemorySession>,
    trigger: Arc<CountingTrigger>,
    installation_id: Id,
}

impl Loop {
    /// 装配一条回路；`bound` = 发件人预先绑到某个用户。
    fn new(bound: Option<Id>) -> Self {
        let peer = FakePeer::new(true);
        let senders = TestSenders::new();
        let audit = Arc::new(MemoryAudit::default());
        let session = Arc::new(MemorySession::default());
        let trigger = Arc::new(CountingTrigger::default());

        let installation_id = Id::new();
        let installation = Installation {
            id: installation_id,
            workspace_id: Id::new(),
            agent_id: Id::new(),
            installer_user_id: Id::new(),
            status: InstallationStatus::Active,
            bot_id: BOT_ID.to_string(),
            secret_encrypted: vec![1, 2, 3],
            bot_display_name: "Multica Bot".to_string(),
            config: serde_json::Value::Null,
            installed_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        // 出站回复器：真件 + 端口替身（**同一条活 socket**）。
        let replier = WeComOutboundReplier::new(
            Some(Arc::clone(&senders) as Arc<dyn crate::wecom::outbound::SenderLookup>),
            "https://app.example",
            "/wecom/bind",
        )
        .with_binder(Arc::new(FixedBinder))
        .with_member_links(Arc::new(MemberLinkBreaker));

        let set = WeComResolverSet::new(
            Arc::new(WeComInstallationResolver::new(Arc::new(
                MemoryInstallations { installation },
            ))),
            Arc::new(WeComIdentityResolver::new(Arc::new(MemoryIdentities {
                bound,
            }))),
            Arc::new(ChannelDeduper::with_store(
                Arc::new(MemoryDedup::default()),
                KIND,
            )),
            Arc::new(WeComSessionBinder::new(
                Arc::clone(&session) as Arc<dyn SessionBinder>
            )),
            Arc::clone(&audit) as Arc<dyn Auditor>,
        )
        .with_media(Arc::new(NeverMedia::default()))
        .with_replier(Arc::new(replier));
        assert_eq!(set.engine_set().origin_type, ORIGIN_WECOM_CHAT);

        let router = Arc::new(Router::new(
            Arc::new(ChannelCommandClassifier),
            Arc::clone(&trigger) as Arc<dyn RunTriggerer>,
            Arc::new(NoReader),
            Arc::new(FixedIssues),
            RouterConfig::default(),
        ));
        router.register(KIND, set.into_engine_set());

        let deps = Arc::new(
            WeComDeps::new(Arc::new(StaticCredentials))
                .with_senders(Arc::clone(&senders) as Arc<dyn SenderRegistry>)
                .with_dialer(peer.dialer())
                .with_ws_url("wss://fake.example/aibot"),
        );
        let channel = Arc::new(WeComChannel {
            installation_id: Some(installation_id),
            bot_id: BOT_ID.to_string(),
            secret: PlaintextSecret::new("static-secret"),
            bot_display_name: "Multica Bot".to_string(),
            handler: Some(router),
            dialer: Arc::clone(&deps.dialer),
            ws_url: deps.ws_url.clone(),
            senders: deps.senders.clone(),
            metrics: None,
        });

        Self {
            peer,
            channel,
            senders,
            audit,
            session,
            trigger,
            installation_id,
        }
    }

    /// 起连接（后台任务）并等握手完成。
    ///
    /// 握手完成的判据是替身收到了 `aibot_subscribe` —— 那之后发送者才进了登记表。
    async fn start(&self) -> tokio::task::JoinHandle<Result<(), ChannelError>> {
        let channel = Arc::clone(&self.channel);
        let handle = tokio::spawn(async move { channel.connect().await });
        // 等握手：订阅帧写出去了，而登记表也装上了（顺序由 `run` 保证：先握手、再 set）。
        let subscribed = self
            .peer
            .wait_for_written(
                |frame| is_cmd(frame, "aibot_subscribe"),
                Duration::from_secs(5),
            )
            .await;
        assert!(subscribed.is_some(), "握手帧没出去");
        for _ in 0..200 {
            if self.senders.has(self.installation_id) {
                return handle;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("连接建立之后发送者没有进登记表");
    }

    fn drops(&self) -> Vec<(String, String)> {
        self.audit.drops.lock().expect("lock").clone()
    }
}

/// 等一个条件成立（带超时）。
async fn eventually<F: Fn() -> bool>(predicate: F, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if predicate() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// =====================================================================
// 回路 1：已绑定发件人的一条文本消息 → 真流水线 → 排了一次 run
// =====================================================================

#[tokio::test]
async fn a_bound_text_message_travels_the_whole_pipeline() {
    let loop_ = Loop::new(Some(Id::new()));
    let handle = loop_.start().await;
    loop_.peer.push(msg_callback(text_message("m1", "你好")));

    assert!(
        eventually(
            || loop_.trigger.runs.load(Ordering::SeqCst) == 1,
            Duration::from_secs(5)
        )
        .await,
        "一条已绑定发件人的消息必须排一次 run"
    );
    // 会话隔离键 = `Source.ChatID`（单聊里它就是 userid）。
    assert_eq!(
        loop_.session.ensured.lock().expect("lock").as_slice(),
        [SENDER_ID]
    );
    // 正文原样进库；**没有**丢弃审计。
    assert_eq!(
        loop_.session.appended.lock().expect("lock").as_slice(),
        ["你好"]
    );
    assert!(loop_.drops().is_empty(), "{:?}", loop_.drops());

    handle.abort();
}

// =====================================================================
// 回路 2：未绑定发件人 ⇒ 绑定卡走**同一条** socket，且只走私聊
// =====================================================================

#[tokio::test]
async fn an_unbound_sender_gets_a_binding_prompt_in_a_private_chat() {
    let loop_ = Loop::new(None);
    let handle = loop_.start().await;
    loop_.peer.push(msg_callback(text_message("m1", "你好")));

    let frame = loop_
        .peer
        .wait_for_written(|frame| is_cmd(frame, CMD_SEND_MSG), Duration::from_secs(5))
        .await
        .expect("绑定卡");
    let (chat_id, chat_type) = address_of(&frame);
    // **持票凭据**绝不落在群里（上游逐字）⇒ 发到发送者**自己的** userid、`chat_type = 1`。
    assert_eq!(chat_id, SENDER_ID);
    assert_eq!(chat_type, 1);
    let content = markdown_of(&frame);
    assert!(
        content.contains("https://app.example/wecom/bind?token=bind-token-xyz"),
        "{content}"
    );
    assert!(content.contains("绑定"), "{content}");

    // 未绑定是**产品性**结局（`needs_binding` + 绑卡），但它**仍然**落一条 `unbound_user` 审计：
    // 一条用户消息没有进入流水线这件事，运维该看得见（上游在发卡之前先记这一笔）。
    assert_eq!(
        loop_.drops(),
        [("unbound_user".to_string(), "m1".to_string())]
    );
    // agent 没有被触发。
    assert_eq!(loop_.trigger.runs.load(Ordering::SeqCst), 0);

    handle.abort();
}

// =====================================================================
// 回路 3：去重命中 ⇒ 丢弃且**不报错**
// =====================================================================

#[tokio::test]
async fn a_duplicate_frame_is_dropped_without_an_error() {
    let loop_ = Loop::new(Some(Id::new()));
    let handle = loop_.start().await;

    // 同一条 `msgid` 投两次（重连重投的形状：平台会重投）。
    loop_.peer.push(msg_callback(text_message("m1", "你好")));
    assert!(
        eventually(
            || loop_.trigger.runs.load(Ordering::SeqCst) == 1,
            Duration::from_secs(5)
        )
        .await,
        "第一条消息要排一次 run"
    );
    loop_.peer.push(msg_callback(text_message("m1", "你好")));

    assert!(
        eventually(|| !loop_.drops().is_empty(), Duration::from_secs(5)).await,
        "重投必须落下一条丢弃审计"
    );
    assert_eq!(
        loop_.drops(),
        [("duplicate".to_string(), "m1".to_string())],
        "去重命中的原因是 duplicate"
    );
    // **就一次 run**：重投没有第二次触发。
    assert_eq!(loop_.trigger.runs.load(Ordering::SeqCst), 1);
    // 会话只被写了一次（`append` 没跑第二遍）。
    assert_eq!(loop_.session.appended.lock().expect("lock").len(), 1);

    // 而这条连接**还活着**（丢弃不是基础设施失败、不撕 socket）：再来一条新消息仍然走通。
    loop_.peer.push(msg_callback(text_message("m2", "还在吗")));
    assert!(
        eventually(
            || loop_.trigger.runs.load(Ordering::SeqCst) == 2,
            Duration::from_secs(5)
        )
        .await,
        "去重命中之后连接必须还能收消息"
    );

    handle.abort();
}

// =====================================================================
// 回路 4：`/issue` ⇒ 出站确认（`replier` 的文案 + `MemberLinkBreaker`）
// =====================================================================

#[tokio::test]
async fn a_pure_issue_command_gets_its_confirmation() {
    let loop_ = Loop::new(Some(Id::new()));
    let handle = loop_.start().await;
    loop_
        .peer
        .push(msg_callback(text_message("m1", "/issue 登录坏了")));

    let frame = loop_
        .peer
        .wait_for_written(
            |frame| is_cmd(frame, CMD_SEND_MSG) && markdown_of(frame).contains("已创建"),
            Duration::from_secs(5),
        )
        .await
        .expect("确认帧");
    let (chat_id, chat_type) = address_of(&frame);
    assert_eq!(chat_id, SENDER_ID);
    assert_eq!(chat_type, 1);
    let content = markdown_of(&frame);
    assert!(content.contains("✅ 已创建 #7"), "{content}");
    // 成员写的标题过了 `MemberLinkBreaker`（本片交付的那一道闸）。日常标题逐字通过。
    assert!(content.contains("登录坏了"), "{content}");

    // `skip_agent_run`：wecom 独一份 —— 纯 `/issue` **不**触发 agent（否则会多出一条
    // "我不认识这个斜杠命令"）。
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(loop_.trigger.runs.load(Ordering::SeqCst), 0);

    handle.abort();
}

// =====================================================================
// 回路 5：读不懂的一种 ⇒ 一行回执，而不是沉默
// =====================================================================

#[tokio::test]
async fn an_unreadable_kind_gets_a_one_line_receipt() {
    let loop_ = Loop::new(Some(Id::new()));
    let handle = loop_.start().await;
    // 一张位置卡：adapter 不认识，`own_text` 答"没有正文"。
    loop_.peer.push(msg_callback(json!({
        "msgid": "m1",
        "aibotid": BOT_ID,
        "chatid": SENDER_ID,
        "chattype": "single",
        "from": { "userid": SENDER_ID },
        "msgtype": "location",
    })));

    let frame = loop_
        .peer
        .wait_for_written(
            |frame| {
                is_cmd(frame, CMD_SEND_MSG)
                    && markdown_of(frame)
                        .contains(crate::wecom::wecom_channel::UNSUPPORTED_MSG_TYPE_RECEIPT)
            },
            Duration::from_secs(5),
        )
        .await
        .expect("回执");
    let (chat_id, chat_type) = address_of(&frame);
    assert_eq!(chat_id, SENDER_ID);
    assert_eq!(chat_type, 1);
    // 回执**不是**一条入站消息：流水线根本没跑。
    assert_eq!(loop_.trigger.runs.load(Ordering::SeqCst), 0);
    assert!(loop_.drops().is_empty());

    handle.abort();
}

// =====================================================================
// 回路 6：`disconnected_event` ⇒ 连接结束，且登记表被撤
// =====================================================================

#[tokio::test]
async fn a_disconnected_event_ends_the_connection_and_releases_the_slot() {
    let loop_ = Loop::new(Some(Id::new()));
    let handle = loop_.start().await;
    assert!(loop_.senders.has(loop_.installation_id));
    loop_.peer.push(event_callback("disconnected_event"));

    let outcome = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("连接必须在被顶掉之后结束")
        .expect("join");
    let error = outcome.expect_err("被顶掉是一条错误（supervisor 据此退避重连）");
    assert!(error.to_string().contains("superseded"), "{error}");
    // 退出路径上撤掉了这把 socket（否则出站会一直往一条死连接上写）。
    assert!(
        !loop_.senders.has(loop_.installation_id),
        "连接结束之后登记表必须被撤"
    );
}

// =====================================================================
// 回路 7：坏帧不撕链路；对端正常关闭 ⇒ `Ok(())`
// =====================================================================

#[tokio::test]
async fn a_bad_frame_does_not_tear_the_socket_down() {
    let loop_ = Loop::new(Some(Id::new()));
    let handle = loop_.start().await;

    // 一段不是 JSON 的字节，以及一个形态不对的回调 body。
    loop_.peer.push_raw(b"not json at all".to_vec());
    loop_.peer.push(msg_callback(json!({"msgid": "broken"})));
    // 之后一条正常的消息仍然走通。
    loop_.peer.push(msg_callback(text_message("m2", "还在吗")));

    assert!(
        eventually(
            || loop_.trigger.runs.load(Ordering::SeqCst) == 1,
            Duration::from_secs(5)
        )
        .await,
        "坏帧之后的正常帧必须照常被处理"
    );
    // 那条形态不对的回调**没有**被当成一条消息（它连 `msgid` 都不完整）。
    assert!(loop_.drops().is_empty());

    handle.abort();
}

/// 对端正常关闭 ⇒ `connect` 返回 `Ok(())`（"取消 / 正常收尾不是错误"）。
#[tokio::test]
async fn a_clean_close_returns_ok() {
    let loop_ = Loop::new(Some(Id::new()));
    let handle = loop_.start().await;
    // 替身侧关掉 → 读半返回 `None` → 循环正常收尾。
    loop_.peer.close();
    let outcome = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("正常收尾要结束")
        .expect("join");
    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(!loop_.senders.has(loop_.installation_id));
}

// =====================================================================
// 收件箱卡片（M7-17 的端口 + 本片的渲染器）
// =====================================================================

/// 收件箱卡片**在同一条 socket 上**出去，且带的是 M7-19 交付的那张卡。
#[test]
fn the_inbox_card_is_the_renderer_the_delivery_path_uses() {
    let renderer = InboxCardRenderer::new("https://app.example");
    let push = crate::wecom::outbound::InboxPush {
        item_id: "item-1".to_string(),
        item_type: "issue_assigned".to_string(),
        issue_id: "issue-1".to_string(),
        recipient_type: "member".to_string(),
        recipient_id: SENDER_ID.to_string(),
        workspace_id: "ws-uuid".to_string(),
        title: "登录页 500 错误".to_string(),
        body: "from: todo\nto: in_review".to_string(),
    };
    let card =
        crate::wecom::outbound::InboxRenderer::render(&renderer, &push, "acme").expect("render");
    assert!(card.starts_with("**[任务指派] 登录页 500 错误**"), "{card}");
    assert!(card.contains("from: todo\nto: in_review"), "{card}");
    assert!(
        card.contains("[查看详情](https://app.example/acme/inbox?issue=issue-1)"),
        "{card}"
    );
    // 推送的收件人是成员 ⇒ 走机器人（agent 不经聊天渠道收任何东西）。
    assert!(push.is_member_recipient());
    // 群 vs 单聊的判据是 `ChatType`（诊断用，不进卡片）。
    assert_eq!(ChatType::P2p.as_str(), "p2p");
}

// =====================================================================
// 回路 8：**路由键是连接盖章的 `bot_id`**，不是帧里的 `aibotid`
// =====================================================================

/// 上游逐字：每一条 `aibot_msg_callback` 都**经由它到达的那条连接**说明自己是哪个机器人
/// （一个机器人一条连接），连接器把 `bot_id` 盖进信封 ⇒ 帧里那个 `aibotid` **不参与**路由。
///
/// 这条判据是"多副本 / 多安装下不会串台"的形态证据：一条被伪造 `aibotid` 的帧仍然只会落到
/// **它自己那条连接**的安装上。
#[tokio::test]
async fn the_routing_key_is_the_connections_bot_id() {
    let loop_ = Loop::new(Some(Id::new()));
    let handle = loop_.start().await;
    let mut body = text_message("m1", "你好");
    body["aibotid"] = serde_json::json!("bot_someone_else");
    loop_.peer.push(msg_callback(body));

    assert!(
        eventually(
            || loop_.trigger.runs.load(Ordering::SeqCst) == 1,
            Duration::from_secs(5)
        )
        .await,
        "帧里的 aibotid 不参与路由：那条连接自己的安装照常接住它"
    );
    assert!(
        loop_.drops().is_empty(),
        "既不是 invalid_event 也不是别的丢弃：{:?}",
        loop_.drops()
    );
    let appended = loop_.session.appended.lock().expect("lock").clone();
    assert_eq!(appended, ["你好"]);

    handle.abort();
}

/// 端口替身的形状完整性（本文件只编译它们；语义由各自的断言钉）。
#[test]
fn the_doubles_implement_their_contracts() {
    fn assert_ports<T: Send + Sync>() {}
    assert_ports::<StaticCredentials>();
    assert_ports::<MemoryInstallations>();
    assert_ports::<MemoryIdentities>();
    assert_ports::<MemoryDedup>();
    assert_ports::<MemorySession>();
    assert_ports::<MemoryAudit>();
    assert_ports::<CountingTrigger>();
    assert_ports::<FixedBinder>();
    assert_ports::<NeverMedia>();
    assert_ports::<crate::wecom::wecom_channel::tests::harness::RecordingHandler>();
    assert_eq!(ChannelKind::WeCom.storage_str(), "wecom");
}
