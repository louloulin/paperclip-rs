//! M10-B3 用例的共用件：`AppState` 字面量 + 请求装置 + 真库 fixture。
//!
//! 拆出来是门 ⑩（单文件 800 行硬上限）的需要，先例 = `docs/32` §30 的 **D10**
//! （`routes/cloud_runtime/tests/{support,db}.rs`）。
//!
//! `AppState` 用**不可达**地址的 `connect_lazy`（与 `routes/probes/ready/tests.rs` 同款）：
//! 离线那半的用例只走到「成员门之前」就该停；一旦有人把一条 handler 改成先查库，
//! 它会拿到连接错误（5xx）而不是静默通过 —— 那是**要被看见**的失败。

use std::sync::Arc;

use axum::body::Body as AxumBody;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
use tower::ServiceExt as _;
use uuid::Uuid;

use crate::routes::auth_user::USER_ID_HEADER;
use crate::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};

/// 不可达库（离线那半专用）。
pub(super) const UNREACHABLE_DB: &str = "postgres://nobody:nobody@127.0.0.1:1/none";

/// 装一个 `AppState`（库不可达）。
pub(super) fn lazy_state() -> Arc<AppState> {
    let db = Db::connect_lazy(UNREACHABLE_DB, 1, 0).expect("lazy pool");
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "lum-2114"));
    Arc::new(AppState::new(
        db,
        RuntimeHandles {
            actors: ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
        ConfigSnapshot::default(),
        realtime,
        ws,
    ))
}

/// 全量 router（与 `apps/mc-server/src/main.rs` 同款装配）。
pub(super) fn lazy_app() -> Router {
    let state = lazy_state();
    crate::apply_default_middleware(crate::routes::router(state.clone())).with_state(state)
}

/// 真库那一半的 router（同一个装配，只是池子是真的）。
pub(super) fn app_with(db: Db) -> Router {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "lum-2114-db"));
    let state = Arc::new(AppState::new(
        db,
        RuntimeHandles {
            actors: ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
        ConfigSnapshot::default(),
        realtime,
        ws,
    ));
    crate::apply_default_middleware(crate::routes::router(state.clone())).with_state(state)
}

/// 真库那一半的连接（设了变量连不上 ⇒ **panic**，不许静默假装绿）。
pub(super) async fn pool() -> Option<Db> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    Some(
        Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}")),
    )
}

macro_rules! fixture {
    () => {
        match pool().await {
            Some(db) => db,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}
// `macro_rules!` 的文本作用域不跨模块 ⇒ 把名字再导出一次（`cloud_runtime/tests/support.rs` 同款）。
pub(super) use fixture;

// ---------------------------------------------------------------------------
// 请求装置
// ---------------------------------------------------------------------------

/// 一条待发请求。
pub(super) struct Call {
    method: &'static str,
    uri: String,
    user: Option<Uuid>,
    workspace: Option<Uuid>,
    pub(super) body: Option<Vec<u8>>,
    actor_source: Option<&'static str>,
}

impl Call {
    pub(super) fn new(method: &'static str, uri: &str, user: Uuid, workspace: Uuid) -> Self {
        Self {
            method,
            uri: uri.to_string(),
            user: Some(user),
            workspace: Some(workspace),
            body: None,
            actor_source: None,
        }
    }

    pub(super) fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub(super) fn machine(mut self, actor: &'static str) -> Self {
        self.actor_source = Some(actor);
        self
    }

    pub(super) fn without_session(mut self) -> Self {
        self.user = None;
        self
    }

    pub(super) fn without_workspace(mut self) -> Self {
        self.workspace = None;
        self
    }
}

fn request_of(call: &Call) -> Request<AxumBody> {
    let mut builder = Request::builder().method(call.method).uri(&call.uri);
    if let Some(user) = call.user {
        builder = builder.header(USER_ID_HEADER, user.to_string());
    }
    if let Some(workspace) = call.workspace {
        builder = builder.header("x-workspace-id", workspace.to_string());
    }
    if let Some(actor) = call.actor_source {
        builder = builder.header("x-actor-source", actor);
    }
    builder
        .body(
            call.body
                .clone()
                .map_or_else(AxumBody::empty, AxumBody::from),
        )
        .expect("request")
}

/// 发一条请求 ⇒ `(status, headers, body-as-json)`。
pub(super) async fn send(app: &Router, call: &Call) -> (StatusCode, HeaderMap, Value) {
    let response = app
        .clone()
        .oneshot(request_of(call))
        .await
        .expect("router call");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, headers, value)
}

/// 错误信封里的 `error` 对象。
pub(super) fn error_of(value: &Value) -> Value {
    value["error"].clone()
}

// ---------------------------------------------------------------------------
// 真库那一半的种子与请求体（门 ⑥；放在这里是为了让 `db.rs` 留在 800 行以内）
// ---------------------------------------------------------------------------

/// 一个 workspace + 三个用户（owner / member / 外人）+ 一枚 agent + 一条 issue。
pub(super) struct Seed {
    pub(super) workspace: Uuid,
    pub(super) owner: Uuid,
    pub(super) member: Uuid,
    pub(super) outsider: Uuid,
    /// `permission_mode = 'private'` 的 agent（只有 owner 能 invoke）。
    pub(super) private_agent: Uuid,
    /// `public_to` + `workspace` 目标的 agent（每个成员都能 invoke）。
    pub(super) public_agent: Uuid,
    pub(super) issue: Uuid,
}

impl Seed {
    /// `agent` 行需要一条 `agent_runtime`（`runtime_mode` / `runtime_id` NOT NULL）。
    pub(super) async fn runtime(db: &mc_db::Db, workspace: Uuid, owner: Uuid, tag: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO agent_runtime (workspace_id, name, runtime_mode, provider, status, \
                 owner_id) VALUES ($1, $2, 'local', 'claude_code', 'online', $3) RETURNING id",
        )
        .bind(workspace)
        .bind(format!("itest-qa-rt-{tag}"))
        .bind(owner)
        .fetch_one(db.pool())
        .await
        .expect("insert agent_runtime")
    }

    /// 建一个 `public_to` + `workspace` 目标的 agent。
    pub(super) async fn public_agent(
        db: &mc_db::Db,
        workspace: Uuid,
        owner: Uuid,
        tag: &str,
        runtime: Uuid,
    ) -> Uuid {
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent (workspace_id, name, runtime_mode, runtime_id, permission_mode, \
                 owner_id, kind) VALUES ($1, $2, 'local', $3, 'public_to', $4, 'user') RETURNING id",
        )
        .bind(workspace)
        .bind(format!("itest-qa-public-{tag}"))
        .bind(runtime)
        .bind(owner)
        .fetch_one(db.pool())
        .await
        .expect("insert public agent");
        sqlx::query(
            "INSERT INTO agent_invocation_target (agent_id, target_type, target_id) \
             VALUES ($1, 'workspace', $2)",
        )
        .bind(agent)
        .bind(workspace)
        .execute(db.pool())
        .await
        .expect("insert invocation target");
        agent
    }
}

pub(super) async fn seed_workspace(db: &mc_db::Db) -> Seed {
    async fn new_user(db: &mc_db::Db, tag: &str, email: &str) -> Uuid {
        sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
            .bind(format!("itest-qa-{tag}"))
            .bind(email.to_string())
            .fetch_one(db.pool())
            .await
            .expect("insert user")
    }
    async fn join(db: &mc_db::Db, workspace: Uuid, user: Uuid, role: &str) {
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(workspace)
            .bind(user)
            .bind(role)
            .execute(db.pool())
            .await
            .expect("insert member");
    }

    // 每条用例一枚唯一 tag ⇒ 用户表的 `user_email_key` UNIQUE 不会被并行用例撞到。
    let tag = Uuid::new_v4().simple().to_string();
    let workspace: Uuid =
        sqlx::query_scalar("INSERT INTO workspace(name, slug) VALUES ($1, $2) RETURNING id")
            .bind(format!("itest-qa-{tag}"))
            .bind(format!("itest-qa-{tag}"))
            .fetch_one(db.pool())
            .await
            .expect("insert workspace");

    let owner = new_user(db, "owner", &format!("itest-qa-owner-{tag}@example.com")).await;
    let member = new_user(db, "member", &format!("itest-qa-member-{tag}@example.com")).await;
    let outsider = new_user(db, "out", &format!("itest-qa-out-{tag}@example.com")).await;
    for (user, role) in [(owner, "owner"), (member, "member")] {
        join(db, workspace, user, role).await;
    }

    let runtime = Seed::runtime(db, workspace, owner, &tag).await;
    let private_agent: Uuid = sqlx::query_scalar(
        "INSERT INTO agent (workspace_id, name, runtime_mode, runtime_id, permission_mode, \
             owner_id, kind) VALUES ($1, $2, 'local', $3, 'private', $4, 'user') RETURNING id",
    )
    .bind(workspace)
    .bind(format!("itest-qa-private-{tag}"))
    .bind(runtime)
    .bind(owner)
    .fetch_one(db.pool())
    .await
    .expect("insert private agent");
    let public_agent = Seed::public_agent(db, workspace, owner, &tag, runtime).await;

    let issue: Uuid = sqlx::query_scalar(
        "INSERT INTO issue (workspace_id, number, identifier, title, status, creator_type, \
             creator_id) VALUES ($1, 1, $2, 'seed', 'todo', 'member', $3) RETURNING id",
    )
    .bind(workspace)
    .bind(format!("QA-{tag}"))
    .bind(owner)
    .fetch_one(db.pool())
    .await
    .expect("insert issue");

    Seed {
        workspace,
        owner,
        member,
        outsider,
        private_agent,
        public_agent,
        issue,
    }
}

/// 建一条动作（走真实端点，返回响应体）。
pub(super) async fn create(
    app: &axum::Router,
    seed: &Seed,
    who: Uuid,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let (status, _, value) = send(
        app,
        &Call::new("POST", "/api/quick-actions/", who, seed.workspace)
            .body(body.to_string().into_bytes()),
    )
    .await;
    (status, value)
}

pub(super) fn action_body(_seed: &Seed, visibility: &str, agent: Uuid) -> serde_json::Value {
    json!({
        // 名字上限 32 字符 ⇒ 取 uuid 的**前 8 位**做唯一性（每条用例一个 workspace，
        // 撞名也只影响本用例自己的断言）。
        "name": format!("act-{}", &Uuid::new_v4().simple().to_string()[..8]),
        "description": "d",
        "assignee_type": "agent",
        "assignee_id": agent.to_string(),
        "prompt": "do the thing",
        "visibility": visibility,
    })
}
