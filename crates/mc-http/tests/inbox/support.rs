//! `/api/inbox*` + `/api/issues/{id}/{subscribers,subscribe,unsubscribe*}` 端到端测试的
//! **共用件**：装配 `AppState`、发请求的 `call`/`get`/`post`、错误体取 `message`，
//! 以及夹具（`Fx` / `seed` / `cleanup` / `new_issue` / `new_item`）与响应取值（`find` / `ids`）。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 =
//! `crates/mc-http/tests/vcs/{main,support}.rs`（`LUM-1799` 的 M8-2）与
//! `crates/mc-http/tests/vcs/connections/{mod,matrix,connect,rotate_delete}.rs`。
//! **纯移动**：函数体、SQL、断言逐字未改，只做拆分必需的可见性窄化（`fn` → `pub(crate) fn`，
//! `struct Fx` 及其字段同理），逐条登记在 PR 描述里。
//!
//! 与 `tests/vcs/support.rs` 同手法：**显式 `use crate::support::{…}`**，不写
//! `use super::*;`（`docs/37` §220 / §221.3 承重的正是这条纪律）。

use std::env;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_core::Id;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

const USER_ID_HEADER: &str = "x-multica-user-id";
const WORKSPACE_ID_HEADER: &str = "x-workspace-id";

pub(crate) fn build_state(db: Db) -> Arc<AppState> {
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

/// 无库 state（`connect_lazy` 不会真的连），用于路由存在性守卫。
pub(crate) fn lazy_state() -> Arc<AppState> {
    let db = Db::connect_lazy("postgres://u:p@127.0.0.1:5432/multica_itest_absent", 1, 0)
        .expect("connect_lazy");
    build_state(db)
}

pub(crate) async fn connect() -> Option<(sqlx::PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url).await.ok()?;
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

/// 发一个请求。`user` / `ws` 为 `None` 时不带对应头（用于测缺头分支）。
pub(crate) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<Uuid>,
    ws: Option<&str>,
    body: Option<&str>,
) -> (StatusCode, Value) {
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
pub(crate) fn message(body: &Value) -> String {
    body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

pub(crate) async fn get(app: &Router, uri: &str, user: Uuid, ws: Uuid) -> (StatusCode, Value) {
    call(app, "GET", uri, Some(user), Some(&Id(ws).as_string()), None).await
}

pub(crate) async fn post(app: &Router, uri: &str, user: Uuid, ws: Uuid) -> (StatusCode, Value) {
    let ws = Id(ws).as_string();
    call(app, "POST", uri, Some(user), Some(&ws), None).await
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

pub(crate) struct Fx {
    pub(crate) ws: Uuid,
    pub(crate) owner: Uuid,
    pub(crate) peer: Uuid,
    /// ws 的成员，但没有任何 inbox item（用来验证"别人的通知" 404 分支）。
    pub(crate) third: Uuid,
    /// 完全不属于 ws 的路人（用来验证非成员 404）。
    pub(crate) outsider: Uuid,
}

pub(crate) async fn seed(pool: &sqlx::PgPool) -> Fx {
    let ws: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-inbox-ws', $1) RETURNING id",
    )
    .bind(format!("itest-inbox-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let mut users = Vec::new();
    for tag in ["owner", "peer", "third", "outsider"] {
        let id: Uuid =
            sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
                .bind(format!("itest-inbox-{tag}"))
                .bind(format!("inbox-{tag}-{}@example.com", Uuid::new_v4()))
                .fetch_one(pool)
                .await
                .expect("insert user");
        users.push(id);
    }
    let (owner, peer, third, outsider) = (users[0], users[1], users[2], users[3]);

    for (user, role) in [(owner, "owner"), (peer, "member"), (third, "member")] {
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(ws)
            .bind(user)
            .bind(role)
            .execute(pool)
            .await
            .expect("insert member");
    }

    Fx {
        ws,
        owner,
        peer,
        third,
        outsider,
    }
}

pub(crate) async fn cleanup(pool: &sqlx::PgPool, fx: &Fx) {
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(fx.ws)
        .execute(pool)
        .await;
    for user in [fx.owner, fx.peer, fx.third, fx.outsider] {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user)
            .execute(pool)
            .await;
    }
}

pub(crate) async fn new_issue(
    pool: &sqlx::PgPool,
    ws: Uuid,
    creator: Uuid,
    number: i32,
    status: &str,
    priority: &str,
    parent: Option<Uuid>,
) -> Uuid {
    sqlx::query_scalar(
        r"INSERT INTO issue(workspace_id, number, identifier, title, status, priority,
                             creator_type, creator_id, parent_issue_id)
           VALUES ($1, $2, $3, $4, $5, $6, 'user', $7::uuid, $8) RETURNING id",
    )
    .bind(ws)
    .bind(number)
    .bind(format!("T-{number}"))
    .bind(format!("itest issue {number}"))
    .bind(status)
    .bind(priority)
    .bind(creator.to_string())
    .bind(parent)
    .fetch_one(pool)
    .await
    .expect("insert issue")
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn new_item(
    pool: &sqlx::PgPool,
    ws: Uuid,
    user: Uuid,
    issue: Option<Uuid>,
    category: &str,
    title: &str,
    body: Option<&str>,
    read: bool,
    archived: bool,
) -> Uuid {
    sqlx::query_scalar(
        r"INSERT INTO inbox_item(workspace_id, recipient_type, recipient_id, issue_id, actor_type, actor_id,
                                 type, title, body, read_at, archived_at, read, archived)
          VALUES ($1, 'user', $2, $3, 'user', $4::uuid, $5, $6, $7,
                  CASE WHEN $8 THEN now() ELSE NULL END,
                  CASE WHEN $9 THEN now() ELSE NULL END, $8, $9) RETURNING id",
    )
    .bind(ws)
    .bind(user)
    .bind(issue)
    .bind(user.to_string())
    .bind(category)
    .bind(title)
    .bind(body)
    .bind(read)
    .bind(archived)
    .fetch_one(pool)
    .await
    .expect("insert inbox_item")
}

pub(crate) fn find(items: &Value, id: Uuid) -> &Value {
    let wanted = Id(id).as_string();
    items
        .as_array()
        .expect("array")
        .iter()
        .find(|it| it["id"].as_str() == Some(wanted.as_str()))
        .unwrap_or_else(|| panic!("item {wanted} not in {items}"))
}

pub(crate) fn ids(items: &Value) -> Vec<String> {
    items
        .as_array()
        .expect("array")
        .iter()
        .map(|it| it["id"].as_str().unwrap_or_default().to_string())
        .collect()
}
