//! M4-4-fu（LUM-1600）**提交后广播**的端到端用例（真库 + 真 socket）。
//!
//! 为什么必须真 socket：[`crate::support::call`] 走 `tower::ServiceExt::oneshot`，拿到的是
//! **未升级**的响应，WS 读泵不会跑 —— 帧投递只能对着真监听断言（`tests/daemon/ws.rs` 同款）。
//! 又为什么必须真库：`send` 的广播取自事务 `RETURNING` 的行（`task_id` / `agent_id` /
//! `runtime_id` / `chat_session_id`），而受众是**同工作区的用户连接**（`notify_workspace_users`
//! 的 `Index::Workspace` 维度）—— 这两件事都只有接上真 router + 真 hub 才能验证。
//!
//! 本文件锁三件事（`docs/53` §验收）：
//!
//! 1. **顺序**：用户面先 `task:queued`、后 `chat:message`（上游 `SendDirectChatMessage`
//!    的 `broadcastTaskEvent` → `NotifyTaskEnqueued` → `publishChat` 次序）—— 客户端不会
//!    先看到气泡、再等胶囊闪出来；
//! 2. **过滤**：同一工作区的 **daemon 面**连接（`mdt_` token，`user_id` 空）拿不到
//!    `chat:message` 正文（`docs/43` §1.3）；它只拿 `daemon:task_available` 唤醒；
//! 3. **取消批次**：逐条 `task:cancelled`（带自己的 task id）之后才是合并唤醒
//!    （hint **不带** task id，语义是「队列里可能还有活儿」）。
//!
//! 与 `tests/daemon/*` 的分工：那边验 daemon 面的 claim / 心跳 / RPC，这边只验 chat 派发
//! 面的广播落点。夹具各自独立（两个测试 target 不能共享非 dev-dep 模块），
//! `crates/mc-http/tests/chat/support.rs` 提供库夹具 + router，本文件补 WS 与 runtime/agent 种子。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderValue, StatusCode};
use axum::Router;
use futures_util::StreamExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

use crate::support::{call, connect, Ctx, SC};

/// 与 `tests/daemon/support.rs` 同一个 dev-mode 口径的机器标识（`mdt_` token 的 `daemon_id`）。
const DAEMON_ID: &str = "d-1600";
const USER_ID_HEADER: &str = "x-multica-user-id";
const WAIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(300);

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// ---------------------------------------------------------------------------
// 应用与连接（为什么不能复用 `tests/daemon/support.rs`：那是另一个测试 target 的私有模块）
// ---------------------------------------------------------------------------

/// 建 `AppState` + Router，并把 **state 一并返回** —— 顺序栅栏要读
/// `state.daemon_hub.<x>_connection_count()`，不能只拿 Router。
fn app_and_state(db: Db) -> (Router, Arc<AppState>) {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(
        realtime.clone(),
        "multica-rs-chat-broadcast-itest",
    ));
    let state = AppState::new(
        db,
        RuntimeHandles {
            actors: ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            ..Default::default()
        },
        realtime,
        ws,
    );
    let state = Arc::new(state);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());
    (app, state)
}

/// 真监听（WS 握手只能走真 socket），返回 `(ws base url, handle)`。
async fn spawn_server(app: Router) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("ws://{addr}"), handle)
}

/// 升级一条连接并断言 101（`extra` = 附加请求头）。
async fn connect_ws(base: &str, extra: &[(&str, &str)]) -> Socket {
    let mut request = format!("{base}/api/daemon/ws")
        .into_client_request()
        .expect("client request");
    for (name, value) in extra {
        request.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
    }
    let (socket, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("ws handshake");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    socket
}

/// 收一帧并解析成 `{type, payload}`（超时即判失败，避免用例挂死）。
async fn recv_json(socket: &mut Socket) -> Value {
    let frame = tokio::time::timeout(WAIT, socket.next())
        .await
        .expect("ws frame within 10s")
        .expect("stream open")
        .expect("frame ok");
    let text = frame.into_text().expect("text frame");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("frame json ({e}): {text}"))
}

/// 静默窗口内必须没有帧（超时即「没有」；读到帧就报出它的正文）。
async fn quiet_for(socket: &mut Socket, wait: Duration) -> bool {
    match tokio::time::timeout(wait, socket.next()).await {
        Err(_) => true,
        Ok(Some(Ok(frame))) => {
            panic!("不该收到帧，实为 {}", frame.into_text().unwrap_or_default());
        }
        Ok(other) => panic!("连接异常：{other:?}"),
    }
}

/// 轮询到条件成立（连接注册是异步的）或超时。
async fn wait_until(label: &str, mut probe: impl FnMut() -> bool) {
    for _ in 0..500 {
        if probe() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("等待超时：{label}");
}

// ---------------------------------------------------------------------------
// 种子
// ---------------------------------------------------------------------------

/// 一台 `online` 的 runtime（`mdt_` token 的 `daemon_id` 名下；`visibility` 的 check
/// 约束只允许 `private` / `public`，见 `083_runtime_visibility.up.sql`）。
async fn seed_runtime(pool: &PgPool, ws: Uuid, owner: Uuid, daemon_id: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime \
            (workspace_id, daemon_id, name, runtime_mode, provider, owner_id, visibility, \
             status, last_seen_at) \
         VALUES ($1, $2, $3, 'local', 'claude', $4, 'private', 'online', now()) RETURNING id",
    )
    .bind(ws)
    .bind(daemon_id)
    .bind(format!("rt-{}", Uuid::new_v4()))
    .bind(owner)
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime")
}

/// 绑在 runtime 上的 agent：`send` 要求 `agent.runtime_id` 非空（否则 409 `NoRuntime`），
/// 而 daemon 面连接的 `runtime_ids` 也来自这台机器名下的 runtime（空集会被 hub 400 拒掉）。
async fn seed_agent(pool: &PgPool, ws: Uuid, runtime_id: Uuid, owner: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent \
            (workspace_id, name, runtime_mode, status, kind, runtime_id, owner_id) \
         VALUES ($1, $2, 'local', 'idle', 'user', $3, $4) RETURNING id",
    )
    .bind(ws)
    .bind(format!("ag-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .bind(owner)
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

/// 内置 Mika agent：onboarding 只认 `system_key`（改名不影响），且**必须**绑 runtime
/// （否则 409 `chat agent has no runtime`）。`owner_id` 仍然要填 —— invoke 门的
/// member 分支先看 `is_agent_owner`（`agents.rs:331`）。
async fn seed_mika_agent(pool: &PgPool, ws: Uuid, runtime_id: Uuid, owner: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent \
            (workspace_id, name, system_key, runtime_mode, status, kind, runtime_id, owner_id) \
         VALUES ($1, 'Mika', 'mika', 'local', 'idle', 'user', $2, $3) RETURNING id",
    )
    .bind(ws)
    .bind(runtime_id)
    .bind(owner)
    .fetch_one(pool)
    .await
    .expect("insert mika agent")
}

/// 插一条 `mdt_` token（`daemon_token` 表 + `scope.rs` 同样的 sha256 哈希），返回原文。
/// 只有它能让升级后的 `ClientIdentity.daemon_id` 非空（`daemon_id` = 空的那条是用户面）。
async fn seed_daemon_token(pool: &PgPool, ws: Uuid, daemon_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let raw = format!("mdt_itest_{}", Uuid::new_v4().simple());
    let hash = hex::encode(Sha256::digest(raw.as_bytes()));
    sqlx::query(
        "INSERT INTO daemon_token (token_hash, workspace_id, daemon_id, expires_at) \
         VALUES ($1, $2, $3, now() + interval '1 hour')",
    )
    .bind(&hash)
    .bind(ws)
    .bind(daemon_id)
    .execute(pool)
    .await
    .expect("insert daemon_token");
    raw
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 一个装满的现场：库夹具 + 真服务 + 两条已注册的连接 + 一个带 runtime 的会话。
struct Fixture {
    ctx: Ctx,
    state: Arc<AppState>,
    server: JoinHandle<()>,
    app: Router,
    user: Socket,
    daemon: Socket,
    agent: Uuid,
    mika_agent: Uuid,
    runtime: Uuid,
    session: String,
}

impl Fixture {
    /// 没有 `MULTICA_TEST_DATABASE_URL` ⇒ `None`（与 `tests/chat.rs` 同款跳过）。
    async fn open() -> Option<Self> {
        let (pool, db) = connect().await?;
        let ctx = Ctx::open(pool.clone(), db.clone()).await;
        let (app, state) = app_and_state(db);
        let (base, server) = spawn_server(app.clone()).await;

        let runtime = seed_runtime(&pool, ctx.fx.ws, ctx.fx.owner, DAEMON_ID).await;
        let agent = seed_agent(&pool, ctx.fx.ws, runtime, ctx.fx.owner).await;
        let mika_agent = seed_mika_agent(&pool, ctx.fx.ws, runtime, ctx.fx.owner).await;
        let token = seed_daemon_token(&pool, ctx.fx.ws, DAEMON_ID).await;

        // 受众 = owner 自己的**第二个客户端**（同工作区的用户连接）。
        let user = connect_ws(&base, &[(USER_ID_HEADER, &ctx.fx.owner.to_string())]).await;
        // 排除项 = 同工作区的 daemon 面连接（`user_id` 空、只有 runtime 维度）。
        let daemon = connect_ws(&base, &[("authorization", &format!("Bearer {token}"))]).await;
        wait_until(
            "两条连接就绪（用户面 1 条 + runtime 面 1 条）",
            || {
                state
                    .daemon_hub
                    .user_connection_count(&ctx.fx.owner.to_string())
                    == 1
                    && state
                        .daemon_hub
                        .runtime_connection_count(&runtime.to_string())
                        == 1
            },
        )
        .await;

        let session = create_session(&app, ctx.fx.ws, ctx.fx.owner, agent).await;
        Some(Self {
            ctx,
            state,
            server,
            app,
            user,
            daemon,
            agent,
            mika_agent,
            runtime,
            session,
        })
    }

    fn ws_header(&self) -> String {
        self.ctx.fx.ws.to_string()
    }

    /// 发一条用户消息，返回 `(201 响应体, task_id, message_id)`。
    async fn send(&self, content: &str) -> (Value, String, String) {
        let uri = format!("/api/chat/sessions/{}/messages", self.session);
        let body = json!({ "content": content }).to_string();
        let (status, res) = call(
            &self.app,
            "POST",
            &uri,
            Some(self.ctx.fx.owner),
            Some(&self.ws_header()),
            Some(&body),
        )
        .await;
        assert_eq!(status, SC::CREATED, "{res}");
        (
            res.clone(),
            res["task_id"].as_str().expect("task_id").to_owned(),
            res["message_id"].as_str().expect("message_id").to_owned(),
        )
    }

    /// 清空会话的排队任务（上游 `ClearQueuedChatTasks`，204 空体）。
    async fn clear_queued(&self) {
        let uri = format!("/api/chat/sessions/{}/queued-tasks", self.session);
        let (status, res) = call(
            &self.app,
            "DELETE",
            &uri,
            Some(self.ctx.fx.owner),
            Some(&self.ws_header()),
            None,
        )
        .await;
        assert_eq!(status, SC::NO_CONTENT, "{res}");
    }

    async fn cleanup(self) {
        self.server.abort();
        let Fixture {
            ctx,
            state,
            user,
            daemon,
            ..
        } = self;
        // 防假绿：`quiet_for(daemon)` 若因为「daemon 连接压根没注册上」而通过，那是空断言。
        // 两条连接必须全程都在册（用例结束时仍然只有这两条）。
        assert_eq!(
            state.daemon_hub.connection_count(),
            2,
            "夹具的两条连接应当全程在册"
        );
        drop((user, daemon));
        ctx.cleanup().await;
    }
}

/// 建会话（走真 router，覆盖 `ChatSessionRepo::create_explicit` 的事务 SQL）。
async fn create_session(app: &Router, ws: Uuid, owner: Uuid, agent: Uuid) -> String {
    let body = json!({ "agent_id": agent, "title": "broadcast itest" }).to_string();
    let (status, res) = call(
        app,
        "POST",
        "/api/chat/sessions",
        Some(owner),
        Some(&ws.to_string()),
        Some(&body),
    )
    .await;
    assert_eq!(status, SC::CREATED, "{res}");
    res["id"].as_str().expect("session id").to_owned()
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// 派发一条用户消息 ⇒ 用户面**按顺序**拿到 `task:queued` 再 `chat:message`；
/// 同工作区的 daemon 面连接只拿到带真 task id 的唤醒，且**永远**拿不到用户面帧。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn send_orders_user_frames_and_keeps_daemon_face_out() {
    let Some(mut f) = Fixture::open().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };

    let (res, task_id, message_id) = f.send("第一条").await;

    // ① 用户面顺序：任务先可见，气泡后到（上游 `broadcastTaskEvent` → `publishChat`）。
    let queued = recv_json(&mut f.user).await;
    assert_eq!(queued["type"], json!("task:queued"), "{queued}");
    assert_eq!(queued["payload"]["task_id"], json!(task_id));
    assert_eq!(queued["payload"]["status"], json!("queued"));
    assert_eq!(queued["payload"]["agent_id"], json!(f.agent.to_string()));
    assert_eq!(queued["payload"]["chat_session_id"], json!(f.session));
    // chat 任务的 `issue_id` 是 NULL ⇒ 上游 `taskEvent` 写空串（键在、值空）。
    assert_eq!(queued["payload"]["issue_id"], json!(""));

    let message = recv_json(&mut f.user).await;
    assert_eq!(message["type"], json!("chat:message"), "{message}");
    assert_eq!(message["payload"]["message_id"], json!(message_id));
    assert_eq!(message["payload"]["role"], json!("user"));
    assert_eq!(message["payload"]["content"], json!("第一条"));
    assert_eq!(message["payload"]["chat_session_id"], json!(f.session));
    assert_eq!(message["payload"]["task_id"], json!(task_id));
    // 与 201 响应**同一份**秒精度时间戳（都走 `timestampToString`）。
    assert_eq!(message["payload"]["created_at"], res["created_at"]);

    // ② daemon 面只收唤醒（带真 task id = 「去认领这一条」），不收任何用户面帧。
    let wake = recv_json(&mut f.daemon).await;
    assert_eq!(wake["type"], json!("daemon:task_available"), "{wake}");
    assert_eq!(wake["payload"]["task_id"], json!(task_id));
    assert_eq!(wake["payload"]["runtime_id"], json!(f.runtime.to_string()));
    assert!(
        quiet_for(&mut f.daemon, QUIET).await,
        "daemon 面连接不该收到 chat:message / task:queued"
    );
    assert!(quiet_for(&mut f.user, QUIET).await);

    f.cleanup().await;
}

/// 清空排队任务 ⇒ 用户面**逐条** `task:cancelled`，之后 daemon 面收到合并唤醒，
/// 且唤醒 hint **不带** task id（语义是「队列里可能还有活儿」，不是「去领这一条」）。
///
/// ⚠️ 必须发**两条**：`clear` 只取消「排队追问」，**保住可见头**（`queue.rs` 的 `head` CTE）
/// —— 只发一条时被取消的集合是空的，两条面都不会有帧。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn clear_queued_broadcasts_cancel_then_wakes_without_a_task_id() {
    let Some(mut f) = Fixture::open().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };

    let (_head_res, head_task, _head_msg) = f.send("第一条（可见头）").await;
    let (_tail_res, tail_task, _tail_msg) = f.send("第二条（排队追问）").await;

    // 先把两次派发产生的帧按序读完，后面的断言才有判别力。
    // 用户面：queued(head) → message(head) → queued(tail) → message(tail)；daemon 面两次唤醒。
    assert_eq!(
        recv_json(&mut f.user).await["payload"]["task_id"],
        json!(head_task)
    );
    assert_eq!(
        recv_json(&mut f.user).await["payload"]["task_id"],
        json!(head_task)
    );
    assert_eq!(
        recv_json(&mut f.user).await["payload"]["task_id"],
        json!(tail_task)
    );
    assert_eq!(
        recv_json(&mut f.user).await["payload"]["task_id"],
        json!(tail_task)
    );
    assert_eq!(
        recv_json(&mut f.daemon).await["payload"]["task_id"],
        json!(head_task)
    );
    assert_eq!(
        recv_json(&mut f.daemon).await["payload"]["task_id"],
        json!(tail_task)
    );

    f.clear_queued().await;

    // ① 逐条取消：帧带自己的 task id（客户端据此把行从队列视图里摘掉）。
    let cancelled = recv_json(&mut f.user).await;
    assert_eq!(cancelled["type"], json!("task:cancelled"), "{cancelled}");
    assert_eq!(cancelled["payload"]["status"], json!("cancelled"));
    assert_eq!(cancelled["payload"]["task_id"], json!(tail_task));
    assert_eq!(cancelled["payload"]["agent_id"], json!(f.agent.to_string()));
    assert_eq!(cancelled["payload"]["chat_session_id"], json!(f.session));

    // ② 合并唤醒（按 runtime 去重、空 task_id 被 `omitempty` 抹掉）。
    let wake = recv_json(&mut f.daemon).await;
    assert_eq!(wake["type"], json!("daemon:task_available"), "{wake}");
    assert_eq!(wake["payload"]["runtime_id"], json!(f.runtime.to_string()));
    assert_eq!(
        wake["payload"].get("task_id"),
        None,
        "唤醒 hint 不该带 task id：{wake}"
    );

    // ③ 库里落点：追问 cancelled、可见头被保住（保住头是本路由的语义，不是副作用）。
    let status_of = |id: &str, pool: &PgPool| {
        let id = Uuid::parse_str(id).expect("uuid");
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>("SELECT status FROM agent_task_queue WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("task status")
        }
    };
    assert_eq!(status_of(&tail_task, &f.ctx.pool).await, "cancelled");
    assert_eq!(
        status_of(&head_task, &f.ctx.pool).await,
        "queued",
        "可见头必须被保住"
    );

    // ④ 再清一次：此时只剩可见头 ⇒ 空批次 ⇒ 两条面都不产生帧（上游空 `tasks` 切片）。
    f.clear_queued().await;
    assert!(quiet_for(&mut f.daemon, QUIET).await);
    assert!(quiet_for(&mut f.user, QUIET).await);

    f.cleanup().await;
}

/// onboarding 的服务端开场白（`mika_onboarding.go:181`）：只广播 `chat:message`，
/// `role` 是 `assistant`、`task_id` 键**缺席**（不是空串）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn onboarding_opening_is_broadcast_without_a_task_id() {
    let Some(mut f) = Fixture::open().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };

    let mika_session = create_session(&f.app, f.ctx.fx.ws, f.ctx.fx.owner, f.mika_agent).await;
    let uri = format!("/api/chat/sessions/{mika_session}/onboarding");
    // 唯一入参是语言（`StartMikaOnboardingRequest`）；问卷答案来自库里已有的 user 行。
    let body = json!({ "language": "zh" }).to_string();
    let (status, res) = call(
        &f.app,
        "POST",
        &uri,
        Some(f.ctx.fx.owner),
        Some(&f.ws_header()),
        Some(&body),
    )
    .await;
    assert_eq!(status, SC::CREATED, "{res}");

    // 开场白是可见气泡（kickoff 行永不广播，所以这里只会来这一帧）。
    let opening = recv_json(&mut f.user).await;
    assert_eq!(opening["type"], json!("chat:message"), "{opening}");
    assert_eq!(opening["payload"]["role"], json!("assistant"));
    assert_eq!(opening["payload"]["chat_session_id"], json!(mika_session));
    assert_eq!(
        opening["payload"].get("task_id"),
        None,
        "开场白没有关联任务 ⇒ task_id 键缺席：{opening}"
    );
    // daemon 面既没有唤醒也没有用户面帧（开场白是纯展示，没有排队工作）。
    assert!(quiet_for(&mut f.daemon, QUIET).await);
    assert!(quiet_for(&mut f.user, QUIET).await);

    f.cleanup().await;
}
