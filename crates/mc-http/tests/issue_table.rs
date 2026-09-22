//! `/api/issues/table/*` + `/api/issues/limit-usage` 端到端测试（M2-D / LUM-1355）。
//!
//! 需要真实 PG：`issue` / `issue_status` / `workspace` / `member` 必须已迁移。
//! 通过 `MULTICA_TEST_DATABASE_URL` 触发；没有该 env 时静默 skip（与 `tests/issues.rs` 一致）。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-http --test issue_table --features test-util -- --ignored
//! ```

#![cfg(feature = "test-util")]

use std::env;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::{AdapterRegistryStub, AppState, ConfigSnapshot, RuntimeHandles};
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
    let adapters = Arc::new(AdapterRegistryStub::default());
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

/// workspace + member 种子（读路径只要求成员身份）。
async fn seed_workspace(pool: &sqlx::PgPool) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-table-ws', $1) RETURNING id",
    )
    .bind(format!("itest-table-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-table-user', $1) RETURNING id"#,
    )
    .bind(format!("table-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");

    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'member')")
        .bind(workspace_id)
        .bind(user_id)
        .execute(pool)
        .await
        .expect("insert member");

    (workspace_id, user_id)
}

async fn cleanup(pool: &sqlx::PgPool, workspace_id: Uuid, user_id: Uuid) {
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(user_id)
        .execute(pool)
        .await;
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

async fn post_table(
    app: &Router,
    path: &str,
    ws: Uuid,
    user: Uuid,
    body: Value,
) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(req("POST", path, ws, user, Some(body)))
        .await
        .unwrap();
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

/// `POST /api/issues` 建种子数据。
async fn create_issue(app: &Router, ws: Uuid, user: Uuid, body: Value) -> Value {
    let res = app
        .clone()
        .oneshot(req("POST", "/api/issues", ws, user, Some(body)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    body_json(res.into_body()).await
}

async fn app_with_fixture() -> Option<(sqlx::PgPool, Uuid, Uuid, Router, Arc<AppState>)> {
    let (pool, db) = connect().await?;
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());
    Some((pool, ws, user, app, state))
}

/// 种子：`todo` ×2（high / none）+ `done` ×1（urgent），返回 issue id 列表（创建顺序）。
async fn seed_three_issues(app: &Router, ws: Uuid, user: Uuid) -> Vec<String> {
    let first = create_issue(
        app,
        ws,
        user,
        json!({"title": "high-todo", "priority": "high"}),
    )
    .await;
    let second = create_issue(app, ws, user, json!({"title": "plain-todo"})).await;
    let third = create_issue(
        app,
        ws,
        user,
        json!({"title": "urgent-done", "status": "done", "priority": "urgent"}),
    )
    .await;
    [first, second, third]
        .iter()
        .map(|issue| issue["id"].as_str().unwrap().to_string())
        .collect()
}

fn group_of<'a>(groups: &'a Value, key: &str) -> Option<&'a Value> {
    groups
        .as_array()?
        .iter()
        .find(|entry| entry["key"].as_str() == Some(key))
}

/// 1) `/groups`：状态计数 + 上游形状的 `value` 对象 + `include_empty` 补齐。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺
async fn table_groups_counts_by_status() {
    let Some((pool, ws, user, app, _state)) = app_with_fixture().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    seed_three_issues(&app, ws, user).await;

    let (status, body) = post_table(
        &app,
        "/api/issues/table/groups",
        ws,
        user,
        json!({"query": {}, "group": {"kind": "status"}, "page": {"limit": 50}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 3);
    assert_eq!(body["next_cursor"], Value::Null);
    assert!(body["query_fingerprint"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    let groups = &body["groups"];
    assert_eq!(groups.as_array().unwrap().len(), 2, "{groups}");
    let todo = group_of(groups, "status:todo").expect("todo group");
    assert_eq!(todo["count"], 2);
    // 上游 `value` 形状（模块注释：key 带前缀，value 是对象）
    assert_eq!(todo["value"]["kind"], "status");
    assert_eq!(todo["value"]["status"], "todo");
    assert!(todo["value"]["actor"].is_null());
    assert_eq!(group_of(groups, "status:done").unwrap()["count"], 1);

    // include_empty：7 个内置状态全部出现，计数 0 的分组也在
    let (status, body) = post_table(
        &app,
        "/api/issues/table/groups",
        ws,
        user,
        json!({"query": {}, "group": {"kind": "status", "include_empty": true}, "page": {"limit": 50}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 3);
    assert_eq!(body["groups"].as_array().unwrap().len(), 7);
    assert_eq!(
        group_of(&body["groups"], "status:blocked").unwrap()["count"],
        0
    );
    assert_eq!(body["groups"][0]["key"], "status:backlog");
    assert_eq!(body["groups"][6]["key"], "status:cancelled");

    cleanup(&pool, ws, user).await;
}

/// 2) `/rows`：keyset 分页 + 游标复放 + 查询变化 → 409 `cursor_query_mismatch`。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺
async fn table_rows_paginates_and_rejects_cursor_mismatch() {
    let Some((pool, ws, user, app, _state)) = app_with_fixture().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    for title in ["r1", "r2", "r3", "r4"] {
        create_issue(&app, ws, user, json!({"title": title})).await;
    }

    let (status, first) = post_table(
        &app,
        "/api/issues/table/rows",
        ws,
        user,
        json!({"query": {}, "group": {"kind": "none"}, "page": {"limit": 2}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["total"], 4, "首页带总数");
    assert_eq!(first["rows"].as_array().unwrap().len(), 2);
    assert_eq!(first["rows"][0]["issue"]["title"], "r1");
    assert_eq!(first["rows"][0]["direct_child_count"], 0);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("next_cursor")
        .to_string();
    let fingerprint = first["query_fingerprint"].as_str().unwrap().to_string();

    let (status, second) = post_table(
        &app,
        "/api/issues/table/rows",
        ws,
        user,
        json!({
            "query": {},
            "group": {"kind": "none"},
            "page": {"limit": 2, "cursor": cursor},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    // 上游 `issueTableRowsResponse.Total` 是 `int64`（非指针、无 omitempty）：只有未分组的
    // 首页才算 total，续页/分组页/子分支都是 0（`issue_table_rows.go:458`）。
    assert_eq!(second["total"], 0, "续页 total=0（上游语义）");
    assert_eq!(
        second["query_fingerprint"], fingerprint,
        "同查询的指纹必须一致"
    );
    assert_eq!(second["rows"][0]["issue"]["title"], "r3");
    assert_eq!(second["rows"][1]["issue"]["title"], "r4");
    assert_eq!(second["next_cursor"], Value::Null);

    // 游标 + 改过的排序 → 409（上游 `issueTableCursorMatches`）
    let (status, body) = post_table(
        &app,
        "/api/issues/table/rows",
        ws,
        user,
        json!({
            "query": {"sort": {"field": "title", "direction": "desc"}},
            "group": {"kind": "none"},
            "page": {"limit": 2, "cursor": cursor},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "cursor_query_mismatch");

    cleanup(&pool, ws, user).await;
}

/// 3) `/facets`（disjunctive 计数）+ `GET /api/issues/limit-usage` 恒 204。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺
async fn table_facets_and_limit_usage() {
    let Some((pool, ws, user, app, _state)) = app_with_fixture().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    seed_three_issues(&app, ws, user).await;

    let (status, body) = post_table(
        &app,
        "/api/issues/table/facets",
        ws,
        user,
        json!({
            "query": {"filters": {"statuses": ["todo"]}},
            "facets": [{"kind": "status"}, {"kind": "priority"}],
            "include_total": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 2, "total 带上 statuses 过滤");
    let facets = body["facets"].as_array().unwrap();
    assert_eq!(facets.len(), 2);

    let status_values = facets[0]["values"].as_array().unwrap();
    // disjunctive：status facet 不吃自己的 statuses 过滤 → done 仍然可见
    assert!(status_values
        .iter()
        .any(|value| value["key"] == "todo" && value["count"] == 2));
    assert!(status_values
        .iter()
        .any(|value| value["key"] == "done" && value["count"] == 1));

    let priority_values = facets[1]["values"].as_array().unwrap();
    assert!(priority_values
        .iter()
        .any(|value| value["key"] == "high" && value["count"] == 1));
    assert!(
        !priority_values.iter().any(|value| value["key"] == "urgent"),
        "urgent 属于 done，被 statuses=todo 过滤掉：{priority_values:?}"
    );

    // limit-usage：本仓无 entitlement 源 → 永远 204（模块注释 6）
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues/limit-usage", ws, user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    cleanup(&pool, ws, user).await;
}

/// 4) 契约：未知字段 400、不支持的维度 422、`page.limit` 越界 400、非成员 403。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺
async fn table_contract_errors() {
    let Some((pool, ws, user, app, _state)) = app_with_fixture().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };

    // 未知字段 → 400（镜像上游 DisallowUnknownFields）
    let (status, body) = post_table(
        &app,
        "/api/issues/table/rows",
        ws,
        user,
        json!({"query": {}, "group": {"kind": "none"}, "page": {"limit": 2}, "bogus": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "validation_error");

    // 不支持的 group.kind → 422 unsupported_group / group_kind_unsupported
    let (status, body) = post_table(
        &app,
        "/api/issues/table/groups",
        ws,
        user,
        json!({"query": {}, "group": {"kind": "label"}}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "unsupported_group");
    assert_eq!(body["code"], "group_kind_unsupported");

    // 不支持的 filter 维度 → 422 unsupported_filter / label_filter_unsupported
    let (status, body) = post_table(
        &app,
        "/api/issues/table/groups",
        ws,
        user,
        json!({
            "query": {"filters": {"label_ids": [Uuid::new_v4().to_string()]}},
            "group": {"kind": "status"},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "unsupported_filter");
    assert_eq!(body["code"], "label_filter_unsupported");

    // group.kind=none 在 /groups 上 → 400（上游不允许分组头为 none）
    let (status, body) = post_table(
        &app,
        "/api/issues/table/groups",
        ws,
        user,
        json!({"group": {"kind": "none"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // page.limit 越界 → 400
    let (status, body) = post_table(
        &app,
        "/api/issues/table/rows",
        ws,
        user,
        json!({"group": {"kind": "none"}, "page": {"limit": 101}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 非本 workspace 成员 → 404（`require_workspace_member` 用 not_found 避免泄露存在性）
    let other_user: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-table-outsider', $1) RETURNING id"#,
    )
    .bind(format!("outsider-{}@example.com", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .expect("insert outsider");
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/table/groups",
            ws,
            other_user,
            Some(json!({"group": {"kind": "status"}})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(other_user)
        .execute(&pool)
        .await;

    cleanup(&pool, ws, user).await;
}
