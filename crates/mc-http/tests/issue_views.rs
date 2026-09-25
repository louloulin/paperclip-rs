//! `/api/issue-views*` + `/api/issue-view-preferences` 端到端测试（M2-A 尾片 / LUM-1691）。
//!
//! 需要真实 PG：`issue_view` / `issue_view_preference` / `member` / `workspace` 都必须已迁移。
//! 本文件的测试均为 `#[ignore]`，通过 `MULTICA_TEST_DATABASE_URL` 触发；没有该 env 时静默 skip。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-http --test issue_views --features test-util -- --ignored
//! ```
//!
//! 种子是 owner + 普通 member（403 / 404 负例）+ outsider（非成员 → 404）+ 第二个 workspace
//! （跨租户 404；owner 在那边**也是**成员 ⇒ 那条 404 是租户隔离而不是成员判定）。
//! **尾斜杠两形态**在用例里各打一发 —— `route_parity.py` 会把 `/x` 与 `/x/` 折叠成一个键，
//! 只有真发请求才验得到 axum 那侧两种形态都装了。
//!
//! `/api/pins*` 与 `/api/assignee-frequency` 的用例在 `tests/issue_pins.rs`（门 ⑩ 的
//! 800 行上限要求拆成两个测试目标）。

#![cfg(feature = "test-util")]

use std::env;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const USER_ID_HEADER: &str = "x-multica-user-id";
const WORKSPACE_HEADER: &str = "x-workspace-id";

fn build_state_with_db(db: Db) -> Arc<AppState> {
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

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn connect() -> Option<(sqlx::PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url).await.ok()?;
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

/// workspace + owner + 普通 member + outsider（非成员）+ 第二个 workspace（跨租户负例）。
async fn seed(pool: &sqlx::PgPool) -> Seed {
    let workspace_id = insert_workspace(pool, "itest-m2a-tail").await;
    let other_workspace = insert_workspace(pool, "itest-m2a-tail-other").await;

    let owner = insert_user(pool, "owner").await;
    let member = insert_user(pool, "member").await;
    let outsider = insert_user(pool, "outsider").await;
    for (user, role) in [(owner, "owner"), (member, "member")] {
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(workspace_id)
            .bind(user)
            .bind(role)
            .execute(pool)
            .await
            .expect("insert member row");
    }
    // owner 也是第二个 workspace 的成员 ⇒ 那里的 404/空列表是**租户隔离**的结果，
    // 不是「因为不是成员所以看不见」（后者的断言弱得多）。
    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(other_workspace)
        .bind(owner)
        .execute(pool)
        .await
        .expect("insert other-workspace member row");

    Seed {
        workspace_id,
        other_workspace,
        owner,
        member,
        outsider,
    }
}

struct Seed {
    workspace_id: Uuid,
    other_workspace: Uuid,
    owner: Uuid,
    member: Uuid,
    outsider: Uuid,
}

async fn insert_workspace(pool: &sqlx::PgPool, prefix: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO workspace(name, slug) VALUES ($1, $2) RETURNING id")
        .bind(prefix)
        .bind(format!("{prefix}-{}", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .expect("insert workspace")
}

async fn insert_user(pool: &sqlx::PgPool, prefix: &str) -> Uuid {
    sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
        .bind(format!("itest-m2a-tail-{prefix}"))
        .bind(format!("m2a-tail-{prefix}-{}@example.com", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .expect("insert user")
}

async fn cleanup(pool: &sqlx::PgPool, seed: &Seed) {
    // 两张 M2-A 表**没有外键**（上游仓库策略）⇒ 级联删不到，必须显式清。
    for table in ["issue_view_preference", "issue_view"] {
        for ws in [seed.workspace_id, seed.other_workspace] {
            let _ = sqlx::query(&format!("DELETE FROM {table} WHERE workspace_id = $1"))
                .bind(ws)
                .execute(pool)
                .await;
        }
    }
    for ws in [seed.workspace_id, seed.other_workspace] {
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(ws)
            .execute(pool)
            .await;
    }
    for user in [seed.owner, seed.member, seed.outsider] {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user)
            .execute(pool)
            .await;
    }
}

fn req(
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Option<Value>,
) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .header(WORKSPACE_HEADER, workspace_id.to_string())
        .header("content-type", "application/json");
    match body {
        Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    ws: Uuid,
    user: Uuid,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(req(method, uri, ws, user, body))
        .await
        .unwrap();
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

/// 建一条 issue（`POST /api/issues`），返回它的 id。
async fn create_view(app: &Router, ws: Uuid, user: Uuid, name: &str, visibility: &str) -> Value {
    let (status, body) = send(
        app,
        "POST",
        "/api/issue-views",
        ws,
        user,
        Some(json!({
            "name": name,
            "scope_type": "workspace",
            "visibility": visibility,
            "query": {"status": ["todo"]},
            "display": {"group": "status"},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create view: {body}");
    body
}
// ---------------------------------------------------------------------------
// ① issue-views 全链（两形态 + 隔离 + 403/404 + 乐观并发）
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
// 一条端到端链路（建/列/读/改/权限/删除 + 两形态）平铺，拆开就看不出顺序依赖
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn issue_view_full_lifecycle_with_slash_forms_and_isolation() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let seed = seed(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    // 两形态都能建（`/api/issue-views` 与 `/api/issue-views/` 是同一个 chi Mount 根）。
    let view = create_view(&app, seed.workspace_id, seed.owner, "  Triage  ", "private").await;
    assert_eq!(view["name"], "  Triage  ", "名字不做 trim（上游不 trim）");
    assert_eq!(view["visibility"], "private");
    assert_eq!(view["definition_version"], 1, "<=0 ⇒ 默认 1");
    assert_eq!(view["revision"], 1);
    assert_eq!(view["scope_id"], Value::Null);
    assert_eq!(view["scope_variant"], Value::Null);
    let view_id = view["id"].as_str().unwrap().to_string();

    let (status, trailing) = send(
        &app,
        "POST",
        "/api/issue-views/",
        seed.workspace_id,
        seed.owner,
        Some(json!({"name": "Second", "scope_type": "workspace", "query": {}})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "带尾斜杠的挂载根也要服务: {trailing}"
    );
    assert_eq!(trailing["visibility"], "private", "visibility 缺省 private");
    assert_eq!(trailing["display"], json!({}), "display 缺省 {{}}");

    // 列表：两形态都给两条；`scope_type` 必填且必须合法。
    for uri in [
        "/api/issue-views?scope_type=workspace",
        "/api/issue-views/?scope_type=workspace",
    ] {
        let (status, body) = send(&app, "GET", uri, seed.workspace_id, seed.owner, None).await;
        assert_eq!(status, StatusCode::OK, "list {uri}: {body}");
        assert_eq!(body.as_array().map(Vec::len), Some(2), "list {uri}: {body}");
    }
    let (status, body) = send(
        &app,
        "GET",
        "/api/issue-views",
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "缺 scope_type: {body}");

    // 单体读：两形态都要服务。
    for uri in [
        format!("/api/issue-views/{view_id}"),
        format!("/api/issue-views/{view_id}/"),
    ] {
        let (status, body) = send(&app, "GET", &uri, seed.workspace_id, seed.owner, None).await;
        assert_eq!(status, StatusCode::OK, "get {uri}: {body}");
        assert_eq!(body["id"], view["id"]);
    }

    // 别人看的私有视图 → 404（存在性不泄漏），跨 workspace → 404，非成员 → 404。
    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/issue-views/{view_id}"),
        seed.workspace_id,
        seed.member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "别人的私有视图");
    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/issue-views/{view_id}"),
        seed.other_workspace,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "跨 workspace（owner 是两边成员）"
    );
    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/issue-views/{view_id}"),
        seed.workspace_id,
        seed.outsider,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "非成员");

    // PATCH 缺 expected_revision → 400；版本命中 → 200 且 revision 递增。
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/issue-views/{view_id}"),
        seed.workspace_id,
        seed.owner,
        Some(json!({"name": "Triage v2"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "expected_revision 必填: {body}"
    );

    let (status, patched) = send(
        &app,
        "PATCH",
        &format!("/api/issue-views/{view_id}/"),
        seed.workspace_id,
        seed.owner,
        Some(json!({"name": "Triage v2", "visibility": "workspace", "expected_revision": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch: {patched}");
    assert_eq!(patched["name"], "Triage v2");
    assert_eq!(patched["revision"], 2);
    // 未给的字段保持原值（`display` 缺失 ⇒ 不动，不是清空）。
    assert_eq!(patched["display"], json!({"group": "status"}));
    assert_eq!(patched["query"], json!({"status": ["todo"]}));

    // 旧版本再 PATCH → 409。
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/issue-views/{view_id}"),
        seed.workspace_id,
        seed.owner,
        Some(json!({"name": "stale", "expected_revision": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "stale revision: {body}");
    // `query: null` 是**显式给了**一个非对象 ⇒ 400（不是「不动」）。
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/issue-views/{view_id}"),
        seed.workspace_id,
        seed.owner,
        Some(json!({"query": null, "expected_revision": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "query=null: {body}");

    // 共享后普通 member 可读；但改共享视图要 owner/admin ⇒ 403。
    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/issue-views/{view_id}"),
        seed.workspace_id,
        seed.member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "共享视图对成员可读");
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/issue-views/{view_id}"),
        seed.workspace_id,
        seed.member,
        Some(json!({"name": "hijack", "expected_revision": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "member 改共享视图: {body}");
    let (status, body) = send(
        &app,
        "DELETE",
        &format!("/api/issue-views/{view_id}"),
        seed.workspace_id,
        seed.member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "member 删共享视图: {body}");

    // 所有者的 PATCH 成功；删除 → 204，再读 → 404。
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/api/issue-views/{view_id}"),
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/issue-views/{view_id}"),
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "删完再读");

    // 非法 scope / 非对象 query → 400。
    let (status, _) = send(
        &app,
        "POST",
        "/api/issue-views",
        seed.workspace_id,
        seed.owner,
        Some(json!({"name": "x", "scope_type": "nope", "query": {}})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "非法 scope_type");
    let (status, _) = send(
        &app,
        "POST",
        "/api/issue-views",
        seed.workspace_id,
        seed.owner,
        Some(json!({"name": "x", "scope_type": "workspace", "query": null})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "query=null");
    let (status, _) = send(
        &app,
        "POST",
        "/api/issue-views",
        seed.workspace_id,
        seed.owner,
        Some(json!({"name": "x", "scope_type": "workspace", "query": []})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "query 非对象");

    cleanup(&pool, &seed).await;
}

// ---------------------------------------------------------------------------
// ③ issue-view-preferences：无记录 = 200 + `{}`；project scope 校验
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)] // preferences 的六种入参形态逐个打，平铺才好对上游分支
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn issue_view_preferences_default_then_round_trip() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let seed = seed(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    // 无记录 → 200 + prefs={} + updated_at=""（**不是 404**）。
    let (status, body) = send(
        &app,
        "GET",
        "/api/issue-view-preferences?scope_type=workspace",
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "无记录不是 404: {body}");
    assert_eq!(body["prefs"], json!({}));
    assert_eq!(body["updated_at"], "", "上游字段没有 omitempty");
    assert_eq!(body["scope_type"], "workspace");
    assert_eq!(body["scope_id"], seed.workspace_id.to_string());

    // 非法 scope_type / project 缺 scope_id / project 不存在 → 400 / 400 / 404。
    let (status, _) = send(
        &app,
        "GET",
        "/api/issue-view-preferences?scope_type=nope",
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "非法 scope_type");
    let (status, _) = send(
        &app,
        "GET",
        "/api/issue-view-preferences?scope_type=project",
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "project 缺 scope_id");
    let (status, _) = send(
        &app,
        "GET",
        &format!(
            "/api/issue-view-preferences?scope_type=project&scope_id={}",
            Uuid::new_v4()
        ),
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "project 不存在");

    // PUT 落盘 → GET 读回；整文档覆盖。
    let (status, put) = send(
        &app,
        "PUT",
        "/api/issue-view-preferences",
        seed.workspace_id,
        seed.owner,
        Some(json!({"scope_type": "workspace", "prefs": {"hidden": ["builtin:all"]}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put: {put}");
    assert_eq!(put["prefs"], json!({"hidden": ["builtin:all"]}));
    assert!(
        put["updated_at"].as_str().is_some_and(|s| !s.is_empty()),
        "落盘后有真实时间戳: {put}"
    );

    let (_, got) = send(
        &app,
        "GET",
        "/api/issue-view-preferences?scope_type=workspace",
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(got["prefs"], json!({"hidden": ["builtin:all"]}));

    let (_, overwritten) = send(
        &app,
        "PUT",
        "/api/issue-view-preferences",
        seed.workspace_id,
        seed.owner,
        Some(json!({"scope_type": "workspace", "prefs": {"order": ["view:x"]}})),
    )
    .await;
    assert_eq!(
        overwritten["prefs"],
        json!({"order": ["view:x"]}),
        "整文档替换"
    );

    // `prefs` 缺省 = {}；显式 null / 非对象 → 400。
    let (status, empty) = send(
        &app,
        "PUT",
        "/api/issue-view-preferences",
        seed.workspace_id,
        seed.owner,
        Some(json!({"scope_type": "my"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "prefs 缺省: {empty}");
    assert_eq!(empty["prefs"], json!({}));
    assert_eq!(
        empty["scope_id"],
        seed.owner.to_string(),
        "my scope 用 user id 回填"
    );
    for bad in [json!(null), json!([])] {
        let (status, _) = send(
            &app,
            "PUT",
            "/api/issue-view-preferences",
            seed.workspace_id,
            seed.owner,
            Some(json!({"scope_type": "workspace", "prefs": bad})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "prefs={bad} 应 400");
    }

    // 非成员 → 404。
    let (status, _) = send(
        &app,
        "GET",
        "/api/issue-view-preferences?scope_type=workspace",
        seed.workspace_id,
        seed.outsider,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "非成员");

    cleanup(&pool, &seed).await;
}
