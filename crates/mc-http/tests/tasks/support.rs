//! task 面端到端测试：support 分片（与 `tests/agents/` 同手法，R7 800 行上限）。
//!
//! 上游 schema（`contracts/upstream-schema.sql`）与本地 `0001` 的 `agent` /
//! `agent_task_queue` 形状不同，因此所有种子都写在**上游形状**上。

use std::env;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";
pub(crate) const WORKSPACE_HEADER: &str = "x-workspace-id";
pub(crate) const CLIENT_PLATFORM_HEADER: &str = "x-client-platform";
pub(crate) const CLIENT_VERSION_HEADER: &str = "x-client-version";
pub(crate) const CLIENT_OS_HEADER: &str = "x-client-os";

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

pub(crate) fn build_state_with_db(db: Db) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-test"));
    let actors = ActorRegistry::new();
    let adapters = Arc::new(AdapterRegistry::default());
    let state = AppState::new(
        db,
        RuntimeHandles { actors, adapters },
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

/// 建 router（与 `tests/agents/` 相同：`router(state).with_state(state)`）。
pub(crate) fn app_with_db(db: Db) -> Router {
    let state = build_state_with_db(db);
    mc_http::routes::router(state.clone()).with_state(state)
}

/// `MULTICA_TEST_DATABASE_URL` 缺失 → `None`；**设了却连不上 → panic**。
pub(crate) async fn connect() -> Option<(PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url)
        .await
        .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

pub(crate) async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// 种子
// ---------------------------------------------------------------------------

/// 一套「workspace + 成员 + runtime + agent + issue」的最小现场。
///
/// agent 的 `owner_id` 就是 `user_id`，所以 invoke 门（`canInvoke` 的 owner 分支）
/// 天然通过；要测「拒绝」就另建一个 owner 是别人的 agent。
pub(crate) struct Fixture {
    pub(crate) pool: PgPool,
    pub(crate) db: Db,
    pub(crate) workspace_id: Uuid,
    pub(crate) user_id: Uuid,
    pub(crate) runtime_id: Uuid,
    pub(crate) agent_id: Uuid,
    pub(crate) issue_id: Uuid,
}

impl Fixture {
    pub(crate) fn app(&self) -> Router {
        app_with_db(self.db.clone())
    }

    /// 再建一个 issue（`number` / `identifier` 在本 workspace 内自增）。
    pub(crate) async fn issue(&self, status: &str) -> Uuid {
        seed_issue(
            &self.pool,
            self.workspace_id,
            self.user_id,
            status,
            None,
            false,
            None,
        )
        .await
    }

    pub(crate) async fn cleanup(&self) {
        cleanup(&self.pool, self.workspace_id, &[self.user_id]).await;
    }
}

pub(crate) async fn setup() -> Option<Fixture> {
    let (pool, db) = connect().await?;
    let workspace_id = seed_workspace(&pool, "member").await;
    let user_id: Uuid =
        sqlx::query_scalar("SELECT user_id FROM member WHERE workspace_id = $1 LIMIT 1")
            .bind(workspace_id)
            .fetch_one(&pool)
            .await
            .expect("fixture member");
    let runtime_id = seed_runtime(&pool, workspace_id, Some(user_id), "public", "online").await;
    let agent_id = seed_agent(&pool, workspace_id, runtime_id, Some(user_id)).await;
    let issue_id = seed_issue(
        &pool,
        workspace_id,
        user_id,
        "todo",
        Some(("agent", agent_id)),
        false,
        None,
    )
    .await;
    Some(Fixture {
        pool,
        db,
        workspace_id,
        user_id,
        runtime_id,
        agent_id,
        issue_id,
    })
}

/// workspace + 一个 `role` 成员，返回 `workspace_id`（成员 id 另查）。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> Uuid {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug, issue_prefix) VALUES ('itest-m3b', $1, 'IT') RETURNING id",
    )
    .bind(format!("itest-m3b-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");
    let _ = seed_user(pool, workspace_id, role).await;
    workspace_id
}

/// 往已有 workspace 里加一个 `role` 成员。
pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m3b-user', $1) RETURNING id"#,
    )
    .bind(format!("m3b-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");

    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(workspace_id)
        .bind(user_id)
        .bind(role)
        .execute(pool)
        .await
        .expect("insert member");
    user_id
}

/// 建一个「只属于自己」的 workspace（用于跨租户 / 非成员判定）。
pub(crate) async fn seed_foreign_user(pool: &PgPool) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m3b-foreign', $1) RETURNING id",
    )
    .bind(format!("itest-m3b-foreign-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert foreign workspace");
    let user_id = seed_user(pool, workspace_id, "member").await;
    (workspace_id, user_id)
}

pub(crate) async fn seed_runtime(
    pool: &PgPool,
    workspace_id: Uuid,
    owner: Option<Uuid>,
    visibility: &str,
    status: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime \
            (workspace_id, name, runtime_mode, provider, owner_id, visibility, status, last_seen_at) \
         VALUES ($1, $2, 'local', 'claude', $3, $4, $5, now()) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("rt-{}", Uuid::new_v4()))
    .bind(owner)
    .bind(visibility)
    .bind(status)
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime")
}

/// 建 agent（`kind='user'`，`permission_mode='private'`）。
pub(crate) async fn seed_agent(
    pool: &PgPool,
    workspace_id: Uuid,
    runtime_id: Uuid,
    owner: Option<Uuid>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent (workspace_id, name, runtime_mode, runtime_id, kind, owner_id) \
         VALUES ($1, $2, 'local', $3, 'user', $4) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("agent-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .bind(owner)
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

/// 建 issue。`assignee` 是 `(type, id)`；`triage` 打上 `triage_state`。
pub(crate) async fn seed_issue(
    pool: &PgPool,
    workspace_id: Uuid,
    creator_id: Uuid,
    status: &str,
    assignee: Option<(&str, Uuid)>,
    triage: bool,
    parent_issue_id: Option<Uuid>,
) -> Uuid {
    let number: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(number), 0) + 1 FROM issue WHERE workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("issue number");
    sqlx::query_scalar(
        "INSERT INTO issue (workspace_id, title, status, creator_type, creator_id, number, \
             identifier, assignee_type, assignee_id, triage_state, parent_issue_id) \
         VALUES ($1, $2, $3, 'member', $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-issue-{number}"))
    .bind(status)
    .bind(creator_id)
    .bind(number)
    .bind(format!("IT-{number}"))
    .bind(assignee.map(|(t, _)| t))
    .bind(assignee.map(|(_, id)| id))
    .bind(triage.then_some("pending"))
    .bind(parent_issue_id)
    .fetch_one(pool)
    .await
    .expect("insert issue")
}

/// 一条 `agent_task_queue` 行；`Fixture` 的 agent/runtime 是默认值。
#[derive(Default)]
pub(crate) struct TaskSeed {
    pub(crate) issue_id: Option<Uuid>,
    pub(crate) agent_id: Option<Uuid>,
    pub(crate) runtime_id: Option<Uuid>,
    pub(crate) status: Option<String>,
    pub(crate) context: Option<Value>,
    pub(crate) chat_session_id: Option<Uuid>,
    pub(crate) started: bool,
    pub(crate) completed: bool,
    pub(crate) rerun_of_task_id: Option<Uuid>,
    pub(crate) escalation_for_task_id: Option<Uuid>,
}

impl TaskSeed {
    pub(crate) fn queued(issue_id: Uuid) -> Self {
        Self {
            issue_id: Some(issue_id),
            status: Some("queued".to_owned()),
            ..Self::default()
        }
    }

    pub(crate) fn status(mut self, status: &str) -> Self {
        self.status = Some(status.to_owned());
        if status != "queued" {
            self.started = true;
        }
        if matches!(status, "completed" | "failed" | "cancelled") {
            self.completed = true;
        }
        self
    }

    /// 抹掉 `started_at`（升级占位行的判定条件之一）。
    pub(crate) fn not_started(mut self) -> Self {
        self.started = false;
        self
    }

    pub(crate) fn agent(mut self, agent_id: Uuid) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    pub(crate) fn escalation(mut self, parent: Uuid) -> Self {
        self.escalation_for_task_id = Some(parent);
        self
    }

    pub(crate) fn context(mut self, context: Value) -> Self {
        self.context = Some(context);
        self
    }

    pub(crate) async fn insert(&self, fx: &Fixture) -> Uuid {
        let agent_id = self.agent_id.unwrap_or(fx.agent_id);
        let runtime_id = self.runtime_id.unwrap_or(fx.runtime_id);
        let status = self.status.clone().unwrap_or_else(|| "queued".to_owned());
        sqlx::query_scalar(
            "INSERT INTO agent_task_queue \
                (agent_id, runtime_id, issue_id, status, context, chat_session_id, \
                 started_at, completed_at, rerun_of_task_id, initiator_user_id, \
                 escalation_for_task_id) \
             VALUES ($1, $2, $3, $4, $5, $6, \
                 CASE WHEN $7 THEN now() END, CASE WHEN $8 THEN now() END, $9, $10, $11) \
             RETURNING id",
        )
        .bind(agent_id)
        .bind(runtime_id)
        .bind(self.issue_id)
        .bind(status)
        .bind(self.context.clone())
        .bind(self.chat_session_id)
        .bind(self.started)
        .bind(self.completed)
        .bind(self.rerun_of_task_id)
        .bind(fx.user_id)
        .bind(self.escalation_for_task_id)
        .fetch_one(&fx.pool)
        .await
        .expect("insert agent_task_queue")
    }
}

pub(crate) async fn seed_chat_session(
    pool: &PgPool,
    workspace_id: Uuid,
    agent_id: Uuid,
    creator_id: Uuid,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO chat_session (workspace_id, agent_id, creator_id, title, status) \
         VALUES ($1, $2, $3, 'itest-chat', 'active') RETURNING id",
    )
    .bind(workspace_id)
    .bind(agent_id)
    .bind(creator_id)
    .fetch_one(pool)
    .await
    .expect("insert chat_session")
}

pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    // 这三张表没有指向 `workspace` / `user` 的 FK，级联删不掉，必须显式清。
    let _ = sqlx::query("DELETE FROM agent_builder_draft WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issue_source_context WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    // 删 workspace 前先把它的成员记下来：`member` 行会随之级联消失，`user` 行不会。
    let members: Vec<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM member WHERE workspace_id = $1")
            .bind(workspace_id)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    for user_id in members.iter().chain(user_ids) {
        let _ = sqlx::query("DELETE FROM client_usage_daily WHERE user_id = $1")
            .bind(user_id)
            .execute(pool)
            .await;
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user_id)
            .execute(pool)
            .await;
    }
}

// ---------------------------------------------------------------------------
// 请求
// ---------------------------------------------------------------------------

/// 通用请求构造：两个身份 header 都是可选的（缺 = 不发送）。
pub(crate) fn req_with(
    method: &str,
    uri: &str,
    user_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
    extra: &[(&str, &str)],
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(user_id) = user_id {
        builder = builder.header(USER_ID_HEADER, user_id.to_string());
    }
    if let Some(workspace_id) = workspace_id {
        builder = builder.header(WORKSPACE_HEADER, workspace_id.to_string());
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    match body {
        Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

pub(crate) fn req(
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Option<Value>,
) -> Request<Body> {
    req_with(method, uri, Some(user_id), Some(workspace_id), &[], body)
}

/// 发一次请求，返回 `(status, headers, json)`。
pub(crate) async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let res = app.clone().oneshot(request).await.expect("router call");
    let status = res.status();
    let headers = res.headers().clone();
    (status, headers, body_json(res.into_body()).await)
}

/// 发一次请求，只关心 `(status, json)`。
pub(crate) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, _, body) = send(app, req(method, uri, workspace_id, user_id, body)).await;
    (status, body)
}

/// 带额外 header 的请求（client-usage / client 侧探针）。
pub(crate) async fn call_extra(
    app: &Router,
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    extra: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = req_with(method, uri, Some(user_id), Some(workspace_id), extra, body);
    let (status, _, body) = send(app, request).await;
    (status, body)
}

// ---------------------------------------------------------------------------
// 断言小工具
// ---------------------------------------------------------------------------

pub(crate) fn id_of(value: &Value) -> Uuid {
    Uuid::parse_str(value["id"].as_str().expect("id")).expect("uuid")
}

/// 标准错误信封的 `code`。
pub(crate) fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

/// 错误正文里**上游原文**那一段（剥掉 `mc-errors` 的内部前缀）。
pub(crate) fn error_message(body: &Value) -> &str {
    const PREFIXES: [&str; 10] = [
        "validation error: ",
        "not found: ",
        "conflict: ",
        "forbidden: ",
        "unauthorized: ",
        "database error: ",
        "internal error: ",
        "workspace not found: ",
        "workspace archived: ",
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

/// 断言 400 + 上游文案。
pub(crate) fn assert_bad_request(status: StatusCode, body: &Value, expected: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(body), "validation_error", "{body}");
    assert_eq!(error_message(body), expected, "{body}");
}
