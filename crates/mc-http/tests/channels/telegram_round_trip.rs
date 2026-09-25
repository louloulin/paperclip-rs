//! telegram **端到端收发回路**（M7-6 / `LUM-1771`）—— `docs/60-M7-PLAN.md` §4.2 的渠道门禁证据。
//!
//! 本文件是 telegram 这一渠道的**真实收发回路**，替身纪律逐条照 §4.2：
//!
//! 1. **只替平台 wire，不替业务路径**：替身是一个本地 axum 服务端，按 Bot API 的**真实方法名**
//!    应答（`getMe` / `getWebhookInfo` / `getUpdates` / `sendMessage` / `editMessageText`），
//!    两端都跑**真 HTTP**（`JsonBotApi` / `Sender`），中间零 mock；
//! 2. **帧逐字段比对**：出站方向断言替身收到的**原始 JSON 字段**；入站方向断言归一化之后的
//!    真实落库行；
//! 3. **真库**：入站经 engine 的**真 Router**（`TelegramResolverSet::from_repos` 把五个必填端口
//!    接在真 PG 仓储上），出站经 `PgDeliveryStore` 写 `channel_reply_delivery`。
//!
//! 全链路是：`替身造一帧 getUpdates → 真入站（长轮询）→ 真 DB（审计 / 去重 / 会话绑定）→
//! 真出站（sender + 流式占位 + 终态编辑）→ 帧回到替身`。
//!
//! 全部 `#[ignore]`：需要真库（门 ⑥ 用 `-- --ignored` 拉起）。

#![cfg(feature = "test-util")]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::Uri;
use axum::routing::post;
use axum::{Json, Router};
use mc_channel::channel::Channel;
use mc_channel::engine::{
    ChannelIssueOutcome, ChannelIssueParams, ChatRunParams, EngineError, EngineResult, NoCommands,
    Router as EngineRouter, RouterConfig, SessionReader, WorkspaceIdentity,
};
use mc_channel::engine::{IssueCreator, RunTriggerer};
use mc_channel::message::SharedInboundHandler;
use mc_channel::telegram::api::{reset_api_base, set_api_base, JsonBotApi};
use mc_channel::telegram::config::Sensitive;
use mc_channel::telegram::delivery::{
    DeliveryLedger, PgDeliveryStore, PHASE_STREAMING, PHASE_TERMINAL,
};
use mc_channel::telegram::outbound::{AnswerProgress, Outbound, ReplyTarget};
use mc_channel::telegram::resolvers::TelegramResolverSet;
use mc_channel::telegram::{TelegramApi, TelegramChannel};
use mc_core::channel::ChannelKind;
use mc_repos::channel::binding::ChannelBindingRepo;
use mc_repos::channel::dedup::ChannelInboundDedupRepo;
use mc_repos::channel::delivery::ChannelDeliveryRepo;
use mc_repos::channel::inbound_audit::ChannelInboundAuditRepo;
use mc_repos::channel::installation::ChannelInstallationRepo;
use mc_repos::channel::session::ChannelChatSessionRepo;
use mc_repos::member::MemberRepo;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::{cleanup, connect, seed, serve_stub, Seed, STUB_LOCK};

/// bot 的数值 id 后缀：**每次运行都不同** —— 安装行的唯一索引建在
/// `(channel_type, config->>'app_id')` 上，固定值会让上一轮失败留下的死主挡住这一轮。
/// （上一次 `--with-db` 失败留下的行不会被清场删掉，这是真库用例的常态。）
fn fresh_bot_id() -> i64 {
    let raw = Uuid::new_v4().as_u128();
    i64::try_from(200_000_000 + (raw % 700_000_000)).unwrap_or(200_000_001)
}
/// 落库密钥（base64 的 32 字节）。
const SECRET_KEY_BASE64: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";
/// 发件人的 Telegram 用户 id（绑定行按它查）。
const SENDER_ID: i64 = 555;
/// 会话的 Telegram chat id。
const CHAT_ID: i64 = 777;

// ---------------------------------------------------------------------------
// Bot API 替身（真方法名；逐字段捕获）
// ---------------------------------------------------------------------------

/// 替身捕获到的一次出站请求。
#[derive(Debug, Clone)]
struct Captured {
    method: String,
    body: Value,
}

/// 替身的共享状态。
#[derive(Default)]
struct BotState {
    captured: Mutex<Vec<Captured>>,
    updates: Mutex<VecDeque<Value>>,
    next_message_id: Mutex<i64>,
}

impl BotState {
    fn captured(&self, method: &str) -> Vec<Value> {
        self.captured
            .lock()
            .expect("lock")
            .iter()
            .filter(|entry| entry.method == method)
            .map(|entry| entry.body.clone())
            .collect()
    }
}

/// 起一个 Bot API 替身（路由就是真实方法名；路径里的 token 只作形状）。
fn bot_api_stub(state: Arc<BotState>) -> Router {
    Router::new()
        .route("/health", post(|| async { "ok" }))
        .fallback(bot_api_dispatch)
        .with_state(state)
}

async fn bot_api_dispatch(
    State(state): State<Arc<BotState>>,
    uri: Uri,
    body: Option<Json<Value>>,
) -> Json<Value> {
    let method = uri
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    state.captured.lock().expect("lock").push(Captured {
        method: method.clone(),
        body: body.map_or(Value::Null, |Json(value)| value),
    });
    match method.as_str() {
        "getMe" => Json(json!({
            "ok": true,
            "result": { "id": 123_456, "is_bot": true, "first_name": "Acme", "username": "acme_bot" }
        })),
        "getWebhookInfo" => {
            Json(json!({ "ok": true, "result": { "url": "", "pending_update_count": 0 } }))
        }
        "getUpdates" => {
            if let Some(update) = state.updates.lock().expect("lock").pop_front() {
                return Json(json!({ "ok": true, "result": [update] }));
            }
            // "没有新消息"：服务端挂起（Telegram 的真实形态是 50 秒）。
            std::future::pending::<()>().await;
            unreachable!()
        }
        "sendMessage" | "editMessageText" => {
            let mut next = state.next_message_id.lock().expect("lock");
            *next += 1;
            Json(json!({ "ok": true, "result": { "message_id": *next } }))
        }
        _ => Json(json!({ "ok": true, "result": true })),
    }
}

// ---------------------------------------------------------------------------
// engine 侧的最小端口替身（**不是**平台替身：这三个端口不进渠道面）
// ---------------------------------------------------------------------------

/// run 触发：本用例只关心"消息进了库"，不真的起 agent run。
struct RecordingTrigger;

#[async_trait::async_trait]
impl RunTriggerer for RecordingTrigger {
    async fn schedule_chat_run(&self, _params: ChatRunParams) -> EngineResult<()> {
        Ok(())
    }
    async fn drain(&self) -> EngineResult<()> {
        Ok(())
    }
}

/// `/issue` 要的 workspace 身份（本用例不建单，给一个形状合法的值）。
struct FixedReader;

#[async_trait::async_trait]
impl SessionReader for FixedReader {
    async fn workspace_identity(
        &self,
        _workspace_id: mc_core::Id,
    ) -> EngineResult<WorkspaceIdentity> {
        Ok(WorkspaceIdentity {
            issue_prefix: "DEL".to_string(),
            slug: "delivery".to_string(),
        })
    }
}

/// `/issue` 建单端口（本用例不建单）。
struct RefusingIssues;

#[async_trait::async_trait]
impl IssueCreator for RefusingIssues {
    async fn create_issue(&self, _params: ChannelIssueParams) -> EngineResult<ChannelIssueOutcome> {
        Err(EngineError::infra(
            "the round-trip test does not create issues",
        ))
    }
}

// ---------------------------------------------------------------------------
// 种子
// ---------------------------------------------------------------------------

/// 插一条已经绑定好的发送人（`channel_user_binding`）。
async fn bind_sender(pool: &PgPool, workspace_id: Uuid, installation_id: Uuid, user_id: Uuid) {
    sqlx::query(
        "INSERT INTO channel_user_binding \
         (workspace_id, multica_user_id, installation_id, channel_type, channel_user_id) \
         VALUES ($1, $2, $3, 'telegram', $4)",
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(installation_id)
    .bind(SENDER_ID.to_string())
    .execute(pool)
    .await
    .expect("bind sender");
}

/// 插一条安装行（`config` 的密文与安装路由写进去的**同一个形状**）。
async fn insert_installation(
    pool: &PgPool,
    seed: &Seed,
    secret_key: &mc_secrets::secretbox::SecretBox,
    bot_id: i64,
    bot_token: &str,
) -> Uuid {
    let sealed = secret_key.seal(bot_token.as_bytes()).expect("seal");
    let encrypted = mc_channel::telegram::config::encode_ciphertext(&sealed);
    let config = json!({
        "app_id": bot_id.to_string(),
        "bot_username": "acme_bot",
        "bot_token_encrypted": encrypted,
    });
    sqlx::query_scalar(
        "INSERT INTO channel_installation \
         (workspace_id, agent_id, channel_type, status, config, installer_user_id) \
         VALUES ($1, $2, 'telegram', 'active', $3, $4) RETURNING id",
    )
    .bind(seed.workspace_id)
    .bind(seed.agent_id)
    .bind(config)
    .bind(seed.admin)
    .fetch_one(pool)
    .await
    .expect("insert installation")
}

/// 插一条任务投递快照（**出站目标的来源**）。
#[allow(clippy::too_many_arguments)]
async fn insert_task_delivery(
    pool: &PgPool,
    task_id: Uuid,
    binding_id: Uuid,
    installation_id: Uuid,
    message_id: &str,
    thread_id: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO channel_task_delivery \
         (task_id, binding_id, installation_id, channel_type, channel_chat_id, chat_type, \
          channel_message_id, channel_thread_id, route_revision, config) \
         VALUES ($1, $2, $3, 'telegram', $4, 'p2p', $5, $6, 1, $7)",
    )
    .bind(task_id)
    .bind(binding_id)
    .bind(installation_id)
    .bind(CHAT_ID.to_string())
    .bind(message_id)
    .bind(thread_id)
    .bind(json!({ "chat_id": CHAT_ID.to_string() }))
    .execute(pool)
    .await
    .expect("insert task delivery");
}

/// 清场：渠道面的表（审计 / 去重 / 投递 / 任务投递 / 绑定）+ support 的那几张。
async fn teardown(pool: &PgPool, seed: &Seed, installation_id: Uuid) {
    for sql in [
        "DELETE FROM channel_reply_delivery WHERE installation_id = $1",
        "DELETE FROM channel_task_delivery WHERE installation_id = $1",
        "DELETE FROM channel_inbound_audit WHERE installation_id = $1",
        "DELETE FROM channel_inbound_message_dedup WHERE installation_id = $1",
        "DELETE FROM channel_chat_session_binding WHERE installation_id = $1",
        "DELETE FROM channel_user_binding WHERE installation_id = $1",
        "DELETE FROM chat_session WHERE workspace_id = $1",
    ] {
        let _ = sqlx::query(sql).bind(installation_id).execute(pool).await;
    }
    cleanup(pool, seed).await;
}

/// 轮询等一个条件成立（真库是异步落地的；不超过 `timeout`）。
async fn wait_for<F, Fut>(mut probe: F, timeout: Duration) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if probe().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    probe().await
}

// ---------------------------------------------------------------------------
// 端到端
// ---------------------------------------------------------------------------

/// 一条安装的发送人身份（`ResolvedInstallation` 的接线在 engine 里；本用例只需要它落库）。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
async fn the_round_trip_carries_one_frame_in_and_one_reply_back_out() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let _guard = STUB_LOCK.lock().await;
    let seed = seed(&pool).await;

    let secret_key = mc_secrets::secretbox::SecretBox::new(
        &mc_secrets::secretbox::decode_key(SECRET_KEY_BASE64).expect("key"),
    )
    .expect("secret box");
    let bot_id = fresh_bot_id();
    let bot_token = format!("{bot_id}:not-a-real-bot-token-itest-only");
    let installation_id = insert_installation(&pool, &seed, &secret_key, bot_id, &bot_token).await;
    bind_sender(&pool, seed.workspace_id, installation_id, seed.member).await;

    // ---- 平台替身（真 HTTP 的两端） ----
    let state = Arc::new(BotState::default());
    state.updates.lock().expect("lock").push_back(json!({
        "update_id": 1,
        "message": {
            "message_id": 41,
            "from": { "id": SENDER_ID, "is_bot": false, "first_name": "Dev" },
            "chat": { "id": CHAT_ID, "type": "private" },
            "date": 1_760_000_000,
            "text": "hello agent"
        }
    }));
    let base = serve_stub(bot_api_stub(Arc::clone(&state))).await;
    set_api_base(base);

    let binding_id = drive_inbound(
        &pool,
        db.clone(),
        bot_id,
        bot_token.clone(),
        installation_id,
    )
    .await;

    // ---- 真出站：一条任务投递快照 → sender → 替身 ----
    let task_id = Uuid::new_v4();
    insert_task_delivery(&pool, task_id, binding_id, installation_id, "777:41", None).await;
    let delivery_row = ChannelDeliveryRepo::new(db.clone())
        .get_task_delivery(mc_core::Id(task_id))
        .await
        .expect("read task delivery")
        .expect("task delivery row");
    let target = ReplyTarget::from_task_delivery(
        &delivery_row,
        Sensitive::new(bot_token.clone()),
        ChannelKind::Telegram,
    )
    .expect("reply target");

    let outbound_channel = TelegramChannel::new(
        bot_id,
        "acme_bot",
        Sensitive::new(bot_token.clone()),
        Arc::new(JsonBotApi::new()) as Arc<dyn TelegramApi>,
        None,
    );
    let sent = outbound_channel
        .send(mc_core::channel::message::OutboundMessage {
            chat_id: CHAT_ID.to_string(),
            text: "**done** — 3 files changed".to_string(),
            thread_id: String::new(),
            reply_to: "777:41".to_string(),
        })
        .await
        .expect("send");
    assert_eq!(
        sent.message_id,
        format!("{CHAT_ID}:1"),
        "复合键（chat:message）"
    );

    let frames = state.captured("sendMessage");
    assert_eq!(frames.len(), 1, "一条逻辑回复只发一帧");
    let frame = &frames[0];
    assert_eq!(frame["chat_id"], json!(CHAT_ID), "帧里的 chat id 是数值");
    assert_eq!(frame["parse_mode"], json!("HTML"), "出站走 HTML parse mode");
    assert_eq!(
        frame["text"],
        json!("<b>done</b> — 3 files changed"),
        "Markdown 转成了 HTML（帧逐字段比对）"
    );
    assert_eq!(
        frame["reply_parameters"]["message_id"],
        json!(41),
        "引用触发消息（复合键里取裸 id）"
    );
    assert!(
        frame.get("message_thread_id").is_none(),
        "私聊不带话题：空值必须缺席而不是 0"
    );

    drive_streaming_delivery(&pool, &state, &target, installation_id, binding_id, task_id).await;

    reset_api_base();
    teardown(&pool, &seed, installation_id).await;
}

/// 出站的**流式 + 投递账**那一半：`push_partial`（占位）→ `deliver_answer`（终态编辑）
/// → 真库里的 `channel_reply_delivery` 收口，并把帧逐字段比对回替身。
///
/// 单独成一个函数只是为了可读性（门 ③ 的 `too_many_lines` 也是这么要求的）：
/// 它仍然是同一条端到端链路的第二段。
async fn drive_streaming_delivery(
    pool: &PgPool,
    state: &BotState,
    target: &ReplyTarget,
    installation_id: Uuid,
    binding_id: Uuid,
    task_id: Uuid,
) {
    let ledger = Arc::new(DeliveryLedger::new(Arc::new(PgDeliveryStore::new(
        ChannelDeliveryRepo::new(mc_db::Db::from_pool(pool.clone())),
    ))));
    let delivery = mc_channel::telegram::delivery::DeliveryTarget {
        task_id: mc_core::Id(task_id),
        binding_id: mc_core::Id(binding_id),
        installation_id: mc_core::Id(installation_id),
        kind: ChannelKind::Telegram,
        chat_id: CHAT_ID.to_string(),
    };
    let turn = ledger.turn_for(delivery.task_id).await.expect("turn");
    assert_eq!(turn.id, delivery.task_id, "没有队列行的任务就是自己的轮次");
    let (lease, status) = ledger
        .acquire(&delivery, turn, PHASE_STREAMING)
        .await
        .expect("acquire streaming");
    assert_eq!(
        status,
        mc_channel::telegram::delivery::DeliveryStatus::Acquired
    );

    let outbound = Outbound::new(
        Arc::new(JsonBotApi::new()) as Arc<dyn TelegramApi>,
        Arc::clone(&ledger),
    );
    let lease = lease.expect("lease");
    let streamed = outbound.push_partial(target, &lease, "working…", 0).await;
    assert!(
        matches!(
            streamed,
            mc_channel::telegram::outbound::StreamStep::PlaceholderCreated { .. }
        ),
        "第一帧 partial 发占位消息：{streamed:?}"
    );

    // 上游的流式帧在**每次调用之后**交还租约（`defer releaseDelivery`）⇒ 终态路径才拿得到它。
    assert!(ledger.release(&lease).await.expect("release"));

    let (terminal, status) = ledger
        .acquire(&delivery, turn, PHASE_TERMINAL)
        .await
        .expect("acquire terminal");
    assert_eq!(
        status,
        mc_channel::telegram::delivery::DeliveryStatus::Acquired
    );
    let terminal = terminal.expect("lease");
    let chunks = Outbound::plan_chunks("**final answer**");
    let mut progress = AnswerProgress {
        streamed_message_id: 2,
        ..AnswerProgress::default()
    };
    let step = outbound
        .deliver_answer(target, &terminal, &chunks, &mut progress)
        .await;
    assert!(step.is_done(), "单片的答案一步收口：{step:?}");

    // 投递账在真库里收口：`settled` + `delivered` + 已投一片。
    let settled: (String, String, i32, String) = sqlx::query_as(
        "SELECT phase, settled_reason, chunks_sent, message_id \
         FROM channel_reply_delivery WHERE turn_id = $1",
    )
    .bind(task_id)
    .fetch_one(pool)
    .await
    .expect("delivery row");
    assert_eq!(settled.0, "settled");
    assert_eq!(settled.1, "delivered");
    assert_eq!(settled.2, 1, "最终答案投了一片");
    // ⚠️ 投递账里存的是**裸** platform id（`deliveryLease.messageID()` 要把它 parse 成 `i64`），
    // 而 `Channel::send` 的 `SendResult` 是复合键 —— 两处形态不同，逐字照上游。
    assert_eq!(settled.3, "2", "占位消息就是编辑目标（裸 id）");

    // 占位消息与终态编辑都到了替身（帧逐字段）。
    let placeholders = state.captured("sendMessage");
    assert_eq!(placeholders.len(), 2, "第二条是占位消息");
    assert_eq!(placeholders[1]["text"], json!("working…"));
    assert_eq!(placeholders[1]["parse_mode"], json!("HTML"));
    let edits = state.captured("editMessageText");
    assert_eq!(edits.len(), 1, "终态走编辑而不是新发");
    assert_eq!(edits[0]["message_id"], json!(2));
    assert_eq!(edits[0]["text"], json!("<b>final answer</b>"));
}

/// 入站的**第一段**：把一条 `getUpdates` 帧喂给**真** `TelegramChannel` + **真** engine `Router`
/// （五个必填端口接在真 PG 仓储上），等真库出现去重行与会话绑定行，并读回绑定 id。
///
/// 单独成一个函数只是为了可读性（门 ③ 的 `too_many_lines`）；它仍是同一条链路的一段。
async fn drive_inbound(
    pool: &PgPool,
    db: mc_db::Db,
    bot_id: i64,
    bot_token: String,
    installation_id: Uuid,
) -> Uuid {
    // ---- 真入站：engine 的 Router + 五个真 PG 端口 ----
    let resolvers = TelegramResolverSet::from_repos(
        ChannelInstallationRepo::new(db.clone()),
        ChannelBindingRepo::new(db.clone()),
        MemberRepo::new(db.clone()),
        ChannelInboundDedupRepo::new(db.clone()),
        Arc::new(ChannelChatSessionRepo::new(db.clone())),
        ChannelInboundAuditRepo::new(db.clone()),
    );
    let engine_router = Arc::new(EngineRouter::new(
        Arc::new(NoCommands),
        Arc::new(RecordingTrigger),
        Arc::new(FixedReader),
        Arc::new(RefusingIssues),
        RouterConfig::default(),
    ));
    engine_router.register(ChannelKind::Telegram, resolvers.into_engine_set());

    let channel = TelegramChannel::new(
        bot_id,
        "acme_bot",
        Sensitive::new(bot_token.clone()),
        Arc::new(JsonBotApi::new()) as Arc<dyn TelegramApi>,
        Some(Arc::clone(&engine_router) as SharedInboundHandler),
    );
    let polling = tokio::spawn(async move {
        let _ = channel.connect().await;
    });

    // ---- 真 DB：帧 → 去重行 + 会话绑定 ----
    let dedup = wait_for(
        || async {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM channel_inbound_message_dedup \
                 WHERE installation_id = $1 AND message_id = $2",
            )
            .bind(installation_id)
            .bind(format!("{CHAT_ID}:41"))
            .fetch_one(pool)
            .await
            .unwrap_or(0)
                > 0
        },
        Duration::from_secs(20),
    )
    .await;
    assert!(dedup, "入站帧必须落一条去重行");

    // ⚠️ 等待条件必须带上**本用例要断言的那个字段**：绑定行的插入先把消息 id 留空，
    // `last_message_id` 是**之后**由 `update_session_reply_target`（append 路径）/
    // route-start 的收尾 UPDATE 写进去的 ⇒ 只等「行存在」会在全量 ⑥ 的负载下读到 NULL
    // （失败现场那行 `last_message_id` / `history_start_message_id` 均为 NULL，`docs/32` §36.2）。
    let bound = wait_for(
        || async {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM channel_chat_session_binding \
                 WHERE installation_id = $1 AND channel_chat_id = $2 \
                   AND last_message_id IS NOT NULL",
            )
            .bind(installation_id)
            .bind(CHAT_ID.to_string())
            .fetch_one(pool)
            .await
            .unwrap_or(0)
                > 0
        },
        Duration::from_secs(20),
    )
    .await;
    assert!(
        bound,
        "已绑定的发件人必须开出会话绑定行（且出站游标已就绪）"
    );

    // 会话绑定行的 `last_message_id` 就是出站要引用的那条（真 DB 里读回来）。
    let binding: (Uuid, Option<String>) = sqlx::query_as(
        "SELECT id, last_message_id FROM channel_chat_session_binding \
         WHERE installation_id = $1 AND channel_chat_id = $2",
    )
    .bind(installation_id)
    .bind(CHAT_ID.to_string())
    .fetch_one(pool)
    .await
    .expect("binding row");
    assert_eq!(
        binding.1.as_deref(),
        Some("777:41"),
        "最近一条触发的 message id"
    );
    polling.abort();
    let _ = polling.await;

    binding.0
}
