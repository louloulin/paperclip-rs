//! `/api/pins*` + `/api/assignee-frequency` 端到端测试（M2-A 尾片 / LUM-1691）。
//!
//! 需要真实 PG：`pinned_item` / `activity_log` / `issue` / `issue_view` / `member` / `workspace`
//! 都必须已迁移。本文件的测试均为 `#[ignore]`，通过 `MULTICA_TEST_DATABASE_URL` 触发；
//! 没有该 env 时静默 skip。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-http --test issue_pins --features test-util -- --ignored
//! ```
//!
//! 种子是 owner + 普通 member + outsider（非成员 → 404）+ 第二个 workspace（跨租户）。pin 面
//! 用到了 `issue_view` 的行（`view` 型 pin 的归属校验），所以这里两套夹具都在。
//!
//! `/api/issue-views*` 与 `/api/issue-view-preferences` 的用例在 `tests/issue_views.rs`
//! （门 ⑩ 的 800 行上限要求拆成两个测试目标）。

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
async fn create_issue(app: &Router, ws: Uuid, user: Uuid) -> String {
    let (status, body) = send(
        app,
        "POST",
        "/api/issues",
        ws,
        user,
        Some(json!({"title": "m2a-tail target"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create issue: {body}");
    body["id"].as_str().expect("issue id").to_string()
}

/// 建一个视图，返回响应体。
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
// ② pins：pin / 重复 409 / 幂等 unpin / reorder / include=view 闸门
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
// 一条端到端链路（pin/重复/闸门/reorder/unpin/隔离）平铺，拆开就看不出顺序依赖
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn pins_lifecycle_reorder_and_legacy_view_gate() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let seed = seed(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    let issue_id = create_issue(&app, seed.workspace_id, seed.owner).await;
    let view = create_view(&app, seed.workspace_id, seed.owner, "Pinnable", "workspace").await;
    let view_id = view["id"].as_str().unwrap().to_string();

    // 空列表就是 `[]`（不是 `null`）。
    let (status, body) = send(
        &app,
        "GET",
        "/api/pins",
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]), "空库 ≡ []");

    // 两个形态都能建：issue pin（无尾斜杠）与 view pin（带尾斜杠）。
    let (status, pin) = send(
        &app,
        "POST",
        "/api/pins",
        seed.workspace_id,
        seed.owner,
        Some(json!({"item_type": "issue", "item_id": issue_id})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "pin issue: {pin}");
    assert_eq!(pin["position"], 1.0);
    assert_eq!(pin["item_type"], "issue");
    let pin_id = pin["id"].as_str().unwrap().to_string();

    let (status, view_pin) = send(
        &app,
        "POST",
        "/api/pins/",
        seed.workspace_id,
        seed.owner,
        Some(json!({"item_type": "view", "item_id": view_id})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "pin view（带尾斜杠形态）: {view_pin}"
    );
    assert_eq!(view_pin["position"], 2.0, "追加到末尾 = max + 1");

    // 旧契约：默认列表不含 view pin；`?include=view` 才含（子串判定）。
    let (_, legacy) = send(
        &app,
        "GET",
        "/api/pins",
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(legacy.as_array().map(Vec::len), Some(1), "legacy: {legacy}");
    assert_eq!(legacy[0]["item_type"], "issue");
    let (_, opted_in) = send(
        &app,
        "GET",
        "/api/pins?include=view",
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(
        opted_in.as_array().map(Vec::len),
        Some(2),
        "opted in: {opted_in}"
    );

    // 重复钉同一项 → 409（唯一约束；**不是**幂等成功）。
    let (status, body) = send(
        &app,
        "POST",
        "/api/pins",
        seed.workspace_id,
        seed.owner,
        Some(json!({"item_type": "issue", "item_id": issue_id})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "重复 pin: {body}");
    assert_eq!(body["error"]["code"], "conflict");

    // 非法 item_type / 非法 item_id / 不存在的对象 → 400 / 400 / 404。
    let (status, _) = send(
        &app,
        "POST",
        "/api/pins",
        seed.workspace_id,
        seed.owner,
        Some(json!({"item_type": "label", "item_id": issue_id})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "未知 item_type");
    let (status, _) = send(
        &app,
        "POST",
        "/api/pins",
        seed.workspace_id,
        seed.owner,
        Some(json!({"item_type": "issue", "item_id": "not-a-uuid"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "非法 item_id");
    let (status, _) = send(
        &app,
        "POST",
        "/api/pins",
        seed.workspace_id,
        seed.owner,
        // 存在但属于**另一个** workspace（这里用一个随机 uuid 即可：本 workspace 查不到）。
        Some(json!({"item_type": "issue", "item_id": Uuid::new_v4().to_string()})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "对象不在本 workspace");

    // reorder：把 issue pin 排到 view pin 后面 ⇒ 默认列表仍只有 1 条（view 被闸门挡掉），
    // 但 `?include=view` 的顺序要变成 view → issue。
    let (status, _) = send(
        &app,
        "PUT",
        "/api/pins/reorder",
        seed.workspace_id,
        seed.owner,
        Some(json!({"items": [
            {"id": pin_id, "position": 5.0},
            {"id": view_pin["id"], "position": 0.5},
        ]})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "reorder");
    let (_, reordered) = send(
        &app,
        "GET",
        "/api/pins?include=view",
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(
        reordered[0]["item_type"], "view",
        "position 0.5 应排第一: {reordered}"
    );
    assert_eq!(reordered[1]["item_type"], "issue");
    // 非法 id 在动第一行之前就该 400（本片有意收紧上游的「边解析边写」）。
    let (status, _) = send(
        &app,
        "PUT",
        "/api/pins/reorder",
        seed.workspace_id,
        seed.owner,
        Some(json!({"items": [{"id": "not-a-uuid", "position": 0.0}]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "reorder 非法 id");

    // unpin：204；再删一次仍 204（幂等）。
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/api/pins/issue/{issue_id}"),
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "unpin");
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/api/pins/issue/{issue_id}"),
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "再删一次仍 204（幂等）");

    // pin 是每用户的：member 看不到 owner 的那条。
    let (_, other) = send(
        &app,
        "GET",
        "/api/pins",
        seed.workspace_id,
        seed.member,
        None,
    )
    .await;
    assert_eq!(other, json!([]), "pin 是每用户私有的");

    // 跨 workspace 读 pin 列表是空的：owner 在第二个 workspace 也是成员，这条走的是
    // `WHERE workspace_id = …` 的租户收窄，不是成员判定。
    let (status, cross) = send(
        &app,
        "GET",
        "/api/pins",
        seed.other_workspace,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "跨 workspace 列表: {cross}");
    assert_eq!(cross, json!([]));

    cleanup(&pool, &seed).await;
}

// ---------------------------------------------------------------------------
// ④ assignee-frequency：空库 `[]` + 两路合并口径
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn assignee_frequency_returns_empty_array_then_aggregated_counts() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let seed = seed(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    // 空库 ⇒ `[]`，不是 `null`。
    let (status, body) = send(
        &app,
        "GET",
        "/api/assignee-frequency",
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]), "空库 ≡ []");

    // 源 1：owner 改派到 member 两次。
    for _ in 0..2 {
        sqlx::query(
            "INSERT INTO activity_log (workspace_id, actor_type, actor_id, action, details) \
             VALUES ($1, 'member', $2, 'assignee_changed', $3::jsonb)",
        )
        .bind(seed.workspace_id)
        .bind(seed.owner)
        .bind(json!({"to_type": "member", "to_id": seed.member.to_string()}).to_string())
        .execute(&pool)
        .await
        .expect("insert assignee_changed");
    }
    // 源 2：owner 建单时已指派给 outsider。
    sqlx::query(
        "INSERT INTO issue (workspace_id, title, creator_type, creator_id, assignee_type, \
                            assignee_id, number) \
         VALUES ($1, 'freq', 'member', $2, 'member', $3, \
                 (SELECT COALESCE(MAX(number), 0) + 1 FROM issue WHERE workspace_id = $1))",
    )
    .bind(seed.workspace_id)
    .bind(seed.owner)
    .bind(seed.outsider)
    .execute(&pool)
    .await
    .expect("insert issue");

    let (status, body) = send(
        &app,
        "GET",
        "/api/assignee-frequency",
        seed.workspace_id,
        seed.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "frequency: {body}");
    assert_eq!(
        body,
        json!([
            {"assignee_type": "member", "assignee_id": seed.member.to_string(), "frequency": 2},
            {"assignee_type": "member", "assignee_id": seed.outsider.to_string(), "frequency": 1},
        ]),
        "频次降序 + 两路合并"
    );

    // 别人（没有改派活动、没建过单）拿到空数组。
    let (_, other) = send(
        &app,
        "GET",
        "/api/assignee-frequency",
        seed.workspace_id,
        seed.member,
        None,
    )
    .await;
    assert_eq!(other, json!([]));

    cleanup(&pool, &seed).await;
}
