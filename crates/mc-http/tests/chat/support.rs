//! `/api/chat/**` 集成测试的共享夹具（M4-3 写集的一部分，只被 [`crate`] 根文件 `chat.rs` 使用）。
//!
//! 为什么单独一个文件：门 ⑩（`scripts/file_size_check.py`）对**每个** `crates/**/*.rs` 有
//! 800 行硬上限。`tests/chat/` 下没有 `main.rs` ⇒ cargo 不把它当独立 target（已用
//! `cargo test --no-run` 逐字确认），因此这里只是 `tests/chat.rs` 的子模块，不是第二个
//! 测试二进制（也就不会把夹具编译两遍、更不会各自连一次库）。

use std::env;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_core::Id;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

pub use axum::http::StatusCode as SC;

const USER_ID_HEADER: &str = "x-multica-user-id";
const WORKSPACE_ID_HEADER: &str = "x-workspace-id";

/// 固定时间戳（纳秒列）：让「游标纳秒 vs 响应秒精度」可以逐字断言。
pub const AT: [&str; 6] = [
    "2026-01-01 00:00:01.100000+00",
    "2026-01-01 00:00:02.200000+00",
    "2026-01-01 00:00:02.500000+00",
    "2026-01-01 00:00:03.000000+00",
    "2026-01-01 00:00:04.123456+00",
    "2026-01-01 00:00:05.000000+00",
];

fn build_state(db: Db) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-itest"));
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
            invitation_per_workspace_per_hour: Some(50),
            ..Default::default()
        },
        realtime,
        ws,
    );
    Arc::new(state)
}

fn router_for(db: Db) -> Router {
    let state = build_state(db);
    mc_http::routes::router(state.clone()).with_state(state)
}

/// 无库 app（`connect_lazy` 不会真的连），用于路由存在性守卫。
pub fn lazy_app() -> Router {
    let db = Db::connect_lazy("postgres://u:p@127.0.0.1:5432/multica_itest_absent", 1, 0)
        .expect("connect_lazy");
    router_for(db)
}

pub async fn connect() -> Option<(sqlx::PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url).await.ok()?;
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

/// 发一个请求。`user` / `ws` 为 `None` 时不带对应头（用于测缺头分支）。
pub async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<Uuid>,
    ws: Option<&str>,
    body: Option<&str>,
) -> (SC, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user) = user {
        builder = builder.header(USER_ID_HEADER, Id(user).as_string());
    }
    if let Some(ws) = ws {
        builder = builder.header(WORKSPACE_ID_HEADER, ws);
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let req = builder
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .expect("build request");
    let res = app.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// 错误体里的 message（本仓形状 `{"error":{"code","message"}}`）。
pub fn msg(body: &Value) -> String {
    body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// 错误正文里**上游原文**那一段（剥掉 `mc-errors` 的内部前缀）。
///
/// 本仓 `Error` 的 `Display` 会给正文加内部前缀（`validation error: ` / `forbidden: ` …），
/// 而上游 `writeError` 写的是裸文案；断言上游文案时先剥前缀，与
/// `tests/agents/support.rs::error_message` 同款。`not found: <resource>` 是本仓的**已知
/// 偏离**（上游是 `<resource> not found`）—— 剥完只剩资源名，所以调用方写 `"agent"`。
pub fn error_message(body: &Value) -> &str {
    const PREFIXES: [&str; 12] = [
        "validation error: ",
        "not found: ",
        "conflict: ",
        "unprocessable entity: ",
        "forbidden: ",
        "unauthorized: ",
        "workspace not found: ",
        "workspace archived: ",
        "database error: ",
        "internal error: ",
        "io error: ",
        "upstream error: ",
    ];
    let raw = body["error"]["message"].as_str().unwrap_or("");
    for prefix in PREFIXES {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return rest;
        }
    }
    raw
}

/// 断言错误响应（状态码 + 逐字文案，文案取自上游 Go，内部前缀由
/// [`error_message`] 剥掉）。
pub fn assert_err(body: &Value, status: SC, expected: SC, want: &str) {
    assert_eq!(status, expected, "{body}");
    assert_eq!(error_message(body), want, "{body}");
}

pub fn ids(items: &Value) -> Vec<String> {
    items
        .as_array()
        .expect("array")
        .iter()
        .map(|it| it["id"].as_str().expect("id").to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

pub struct Fx {
    pub ws: Uuid,
    pub owner: Uuid,
    pub peer: Uuid,
    /// 完全不属于 ws 的路人（验证非成员 404 `not found: workspace`）。
    pub outsider: Uuid,
    /// owner 自己的 6 个 `private` agent（owner 全部可见 / 可 invoke）。
    pub agents: Vec<Uuid>,
    /// peer 拥有的 `public_to` agent，白名单放了整个 workspace ⇒ owner 也能用。
    pub shared: Uuid,
    /// peer 自己的 `private` agent ⇒ 只有 peer 可见（验证列表的 agent 可见性过滤）。
    pub peer_private: Uuid,
    pub project: Uuid,
}

pub struct Ctx {
    pub app: Router,
    pub pool: sqlx::PgPool,
    pub fx: Fx,
}

async fn seed(pool: &sqlx::PgPool) -> Fx {
    let ws: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-chat-ws', $1) RETURNING id",
    )
    .bind(format!("itest-chat-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let mut users = Vec::new();
    for tag in ["owner", "peer", "outsider"] {
        let id: Uuid =
            sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
                .bind(format!("itest-chat-{tag}"))
                .bind(format!("chat-{tag}-{}@example.com", Uuid::new_v4()))
                .fetch_one(pool)
                .await
                .expect("insert user");
        users.push(id);
    }
    let (owner, peer, outsider) = (users[0], users[1], users[2]);
    for (user, role) in [(owner, "owner"), (peer, "member")] {
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(ws)
            .bind(user)
            .bind(role)
            .execute(pool)
            .await
            .expect("insert member");
    }

    let mut agents = Vec::new();
    for n in 0..6 {
        agents.push(new_agent(pool, ws, owner, "private", None, n).await);
    }
    let shared = new_agent(pool, ws, peer, "public_to", Some(ws), 90).await;
    let peer_private = new_agent(pool, ws, peer, "private", None, 91).await;
    let project: Uuid =
        sqlx::query_scalar("INSERT INTO project(workspace_id, title) VALUES ($1, $2) RETURNING id")
            .bind(ws)
            .bind("itest project")
            .fetch_one(pool)
            .await
            .expect("insert project");

    Fx {
        ws,
        owner,
        peer,
        outsider,
        agents,
        shared,
        peer_private,
        project,
    }
}

/// 一个 agent；`target` 为 `Some(ws)` 时同时插一条 `workspace` 白名单行
/// （`public_to` 靠它放行整个 workspace，见 `memberHitsInvocationTargets`）。
async fn new_agent(
    pool: &sqlx::PgPool,
    ws: Uuid,
    owner: Uuid,
    mode: &str,
    target: Option<Uuid>,
    seq: i32,
) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent(workspace_id, name, runtime_mode, visibility, owner_id, permission_mode) \
         VALUES ($1, $2, 'local', 'workspace', $3, $4) RETURNING id",
    )
    .bind(ws)
    .bind(format!("itest-agent-{seq}"))
    .bind(owner)
    .bind(mode)
    .fetch_one(pool)
    .await
    .expect("insert agent");
    if let Some(target) = target {
        sqlx::query(
            "INSERT INTO agent_invocation_target(agent_id, target_type, target_id, created_by) \
             VALUES ($1, 'workspace', $2, $3)",
        )
        .bind(id)
        .bind(target)
        .bind(owner)
        .execute(pool)
        .await
        .expect("insert invocation target");
    }
    id
}

/// 裸插一条 `chat_session`：`explicitly_created_at` 为 `false` 时就是「没有显式创建、
/// 也没有可见消息」的隐藏渠道会话（列表与公开门都看不见）。
pub async fn raw_session(
    pool: &sqlx::PgPool,
    ws: Uuid,
    creator: Uuid,
    agent: Uuid,
    explicit: bool,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO chat_session(workspace_id, agent_id, creator_id, title, explicitly_created_at) \
         VALUES ($1, $2, $3, 'raw', CASE WHEN $4 THEN now() ELSE NULL END) RETURNING id",
    )
    .bind(ws)
    .bind(agent)
    .bind(creator)
    .bind(explicit)
    .fetch_one(pool)
    .await
    .expect("insert chat_session")
}

/// 裸插一条 `chat_message`（`ChatMessageRepo` 没有写入面 —— 写消息属 M4-4）。
pub async fn new_message(
    pool: &sqlx::PgPool,
    session: Uuid,
    role: &str,
    content: &str,
    at: &str,
    kind: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO chat_message(chat_session_id, role, content, created_at, message_kind) \
         VALUES ($1, $2, $3, $4::timestamptz, $5) RETURNING id",
    )
    .bind(session)
    .bind(role)
    .bind(content)
    .bind(at)
    .bind(kind)
    .fetch_one(pool)
    .await
    .expect("insert chat_message")
}

impl Ctx {
    /// 建库夹具 + 装好 router（也顺带覆盖 `ChatSessionRepo::create_explicit` 的事务 SQL）。
    pub async fn open(pool: sqlx::PgPool, db: Db) -> Self {
        let fx = seed(&pool).await;
        Self {
            app: router_for(db),
            pool,
            fx,
        }
    }

    /// 以任意成员身份发请求（`ws` 恒取夹具的 workspace）。
    pub async fn send(
        &self,
        method: &str,
        uri: &str,
        user: Uuid,
        body: Option<&str>,
    ) -> (SC, Value) {
        let ws = Id(self.fx.ws).as_string();
        call(&self.app, method, uri, Some(user), Some(&ws), body).await
    }

    pub async fn get(&self, uri: &str) -> (SC, Value) {
        self.send("GET", uri, self.fx.owner, None).await
    }

    /// 带 JSON 体的写请求（owner 身份；`peer` 场景直接调 [`Ctx::send`]）。
    pub async fn raw(&self, method: &str, uri: &str, body: &str) -> (SC, Value) {
        self.send(method, uri, self.fx.owner, Some(body)).await
    }

    pub async fn delete(&self, uri: &str) -> (SC, Value) {
        self.send("DELETE", uri, self.fx.owner, None).await
    }

    /// 建一个会话并返回 id（201 断言内联，省掉每处重复）。
    pub async fn create(&self, agent: Uuid, title: &str) -> String {
        let body = json!({ "agent_id": agent, "title": title }).to_string();
        let (status, res) = self.raw("POST", SESSIONS, &body).await;
        assert_eq!(status, SC::CREATED, "{res}");
        res["id"].as_str().expect("id").to_string()
    }

    pub async fn cleanup(&self) {
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(self.fx.ws)
            .execute(&self.pool)
            .await;
        for user in [self.fx.owner, self.fx.peer, self.fx.outsider] {
            let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
                .bind(user)
                .execute(&self.pool)
                .await;
        }
    }
}

/// chat 会话集合的根路径（两条形态都注册，见 `routes/chat/session.rs` 模块头）。
pub const SESSIONS: &str = "/api/chat/sessions";
