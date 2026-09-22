//! `/api/issues*` + `/api/issue-statuses*` 端到端测试（M2-A / LUM-1348）。
//!
//! 需要真实 PG：`issue` / `issue_status` / `issue_reaction` 三张表必须已迁移。
//! 通过 `MULTICA_TEST_DATABASE_URL` 触发；没有该 env 时静默 skip（与
//! `tests/invitations.rs` 一致）。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-http --test issues --features test-util -- --ignored
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
use serde_json::json;
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

async fn body_json(body: Body) -> serde_json::Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

async fn connect() -> Option<(sqlx::PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url).await.ok()?;
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

/// workspace + member 种子（member role 足够：本切片的读路径只要求成员身份）。
async fn seed_workspace(pool: &sqlx::PgPool) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-issue-ws', $1) RETURNING id",
    )
    .bind(format!("itest-issue-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-issue-user', $1) RETURNING id"#,
    )
    .bind(format!("issue-{}@example.com", Uuid::new_v4()))
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
    body: Option<serde_json::Value>,
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

/// 建一个 issue 并返回其 JSON。
async fn create_issue(
    app: &Router,
    workspace_id: Uuid,
    user_id: Uuid,
    body: serde_json::Value,
) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues",
            workspace_id,
            user_id,
            Some(body),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    body_json(res.into_body()).await
}

/// 1) CRUD：创建 → 按 id / identifier 读 → 更新 → 列表 → 删除。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_crud_roundtrip() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    let created = create_issue(
        &app,
        ws,
        user,
        json!({"title": "first issue", "description": "hello", "priority": "high"}),
    )
    .await;
    assert_eq!(created["title"], "first issue");
    assert_eq!(created["status"], "todo");
    assert_eq!(created["priority"], "high");
    assert_eq!(created["revision"], 1);
    assert_eq!(created["creator_type"], "user");
    assert_eq!(created["creator_id"], user.to_string());
    let issue_id = created["id"].as_str().unwrap().to_string();
    let identifier = created["identifier"].as_str().unwrap().to_string();
    assert!(!identifier.is_empty());
    // RFC3339 字符串（M1 约定）
    assert!(created["created_at"].as_str().unwrap().contains('T'));

    // 按 UUID 读
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.into_body()).await["id"], issue_id);

    // 按 identifier 读（上游支持 `LUM-1348` 形式）
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{identifier}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 更新：title + status 迁移（todo → in_progress 合法）+ 乐观并发
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"title": "renamed", "status": "in_progress", "expected_revision": 1})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let updated = body_json(res.into_body()).await;
    assert_eq!(updated["title"], "renamed");
    assert_eq!(updated["status"], "in_progress");
    assert_eq!(updated["revision"], 2);

    // 过期 revision → 409
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"title": "stale", "expected_revision": 1})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // 终态后不可回退：先把 issue 置为 done，再改回 todo → 400
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"status": "done"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.into_body()).await["status"], "done");

    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"status": "todo"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(res.into_body()).await["error"]["code"],
        "issue_transition_invalid"
    );

    // 列表
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues?limit=10", ws, user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let list = body_json(res.into_body()).await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["issues"][0]["id"], issue_id);

    // 删除 → 204；再读 → 404
    let res = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    cleanup(&pool, ws, user).await;
}

/// 2) 过滤 / 搜索 / 分组。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_filters_search_and_grouped() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    create_issue(
        &app,
        ws,
        user,
        json!({"title": "alpha bug", "priority": "high"}),
    )
    .await;
    create_issue(
        &app,
        ws,
        user,
        json!({"title": "beta feature", "priority": "low", "assignee_type": "user", "assignee_id": user.to_string()}),
    )
    .await;
    create_issue(&app, ws, user, json!({"title": "gamma chore"})).await;

    // priority 过滤
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues?priority=high", ws, user, None))
        .await
        .unwrap();
    let body = body_json(res.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["issues"][0]["title"], "alpha bug");

    // q 全文
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues?q=beta", ws, user, None))
        .await
        .unwrap();
    let body = body_json(res.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["issues"][0]["title"], "beta feature");

    // unfiltered 总数（默认含终态）
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues", ws, user, None))
        .await
        .unwrap();
    assert_eq!(body_json(res.into_body()).await["total"], 3);

    // search 必须带 q
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues/search", ws, user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // search 命中 + match_source（响应没有 total）
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues/search?q=gamma", ws, user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert!(body.get("total").is_none());
    assert_eq!(body["issues"][0]["match_source"], "title");

    // POST /query（与 query string 同 key）
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/query",
            ws,
            user,
            Some(json!({"priority": "low", "limit": 5})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.into_body()).await["total"], 1);

    // grouped（默认 assignee）
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues/grouped", ws, user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["group_by"], "assignee");
    let groups = body["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2); // 一个已分配 + 一个 unassigned
    assert!(groups.iter().any(|g| g["id"] == format!("assignee:{user}")));
    assert!(groups.iter().any(|g| g["id"] == "assignee:unassigned"));

    cleanup(&pool, ws, user).await;
}

/// 3) 父子关系：children / child-progress / move / batch。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_children_move_and_batch() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    let parent = create_issue(&app, ws, user, json!({"title": "parent"})).await;
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let first = create_issue(
        &app,
        ws,
        user,
        json!({"title": "child one", "parent_issue_id": parent_id}),
    )
    .await;
    let second = create_issue(
        &app,
        ws,
        user,
        json!({"title": "child two", "parent_issue_id": parent_id}),
    )
    .await;
    let first_id = first["id"].as_str().unwrap().to_string();
    let second_id = second["id"].as_str().unwrap().to_string();

    // 自引用 / 环 → 400
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{parent_id}"),
            ws,
            user,
            Some(json!({"parent_issue_id": parent_id})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{parent_id}"),
            ws,
            user,
            Some(json!({"parent_issue_id": first_id})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // `/:id/children`
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{parent_id}/children"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    let body = body_json(res.into_body()).await;
    assert_eq!(body["issues"].as_array().unwrap().len(), 2);

    // `/children?parent_ids=`（批量）
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/children?parent_ids={parent_id}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        body_json(res.into_body()).await["issues"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // `/child-progress`：0/2 done
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues/child-progress", ws, user, None))
        .await
        .unwrap();
    let body = body_json(res.into_body()).await;
    let entry = body["progress"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["parent_issue_id"] == parent_id)
        .unwrap()
        .clone();
    assert_eq!(entry["total"], 2);
    assert_eq!(entry["done"], 0);

    // move：把 second 排到 first 前面（before = first，after = null）
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            &format!("/api/issues/{second_id}/move"),
            ws,
            user,
            Some(json!({"before_id": first_id, "after_id": null, "status": "in_progress"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let moved = body_json(res.into_body()).await;
    assert_eq!(moved["status"], "in_progress");
    assert_eq!(moved["revision"], 2); // move + 字段补丁只 bump 一次
    assert_eq!(moved["parent_issue_id"], parent_id);

    // move 白名单之外的字段 → 400
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            &format!("/api/issues/{second_id}/move"),
            ws,
            user,
            Some(json!({"before_id": null, "after_id": null, "position": 1.0})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // batch-update：两个孩子置 high
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/batch-update",
            ws,
            user,
            Some(json!({"issue_ids": [first_id, second_id], "updates": {"priority": "high"}})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.into_body()).await["updated"], 2);

    // batch-delete：删掉两个孩子
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/batch-delete",
            ws,
            user,
            Some(json!({"issue_ids": [first_id, second_id]})),
        ))
        .await
        .unwrap();
    assert_eq!(body_json(res.into_body()).await["deleted"], 2);

    cleanup(&pool, ws, user).await;
}

/// 4) reactions / metadata / properties。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_reactions_metadata_and_properties() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    let issue = create_issue(&app, ws, user, json!({"title": "reactions"})).await;
    let issue_id = issue["id"].as_str().unwrap().to_string();

    // 加 reaction（幂等：两次 → 仍只有一条）
    for _ in 0..2 {
        let res = app
            .clone()
            .oneshot(req(
                "POST",
                &format!("/api/issues/{issue_id}/reactions"),
                ws,
                user,
                Some(json!({"emoji": "👍"})),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let body = body_json(res.into_body()).await;
        assert_eq!(body["actor_type"], "user");
        assert_eq!(body["actor_id"], user.to_string());
    }
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{issue_id}/reactions"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        body_json(res.into_body()).await.as_array().unwrap().len(),
        1
    );

    // 删 reaction（DELETE 带 body）
    let res = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/api/issues/{issue_id}/reactions"),
            ws,
            user,
            Some(json!({"emoji": "👍"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // metadata：写 → 读 → 删
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/metadata/team"),
            ws,
            user,
            Some(json!({"value": "platform"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["metadata"]["team"], "platform");
    assert_eq!(body["issue_revision"], 2);
    assert_eq!(issue["metadata"], json!({}));

    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{issue_id}/metadata"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        body_json(res.into_body()).await["metadata"]["team"],
        "platform"
    );

    // 非法 key（percent-encoded 空格）/ 非 primitive value → 400
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/metadata/bad%20key"),
            ws,
            user,
            Some(json!({"value": 1})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let long_key = "k".repeat(70);
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/metadata/{long_key}"),
            ws,
            user,
            Some(json!({"value": 1})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/metadata/nested"),
            ws,
            user,
            Some(json!({"value": {"a": 1}})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/api/issues/{issue_id}/metadata/team"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(body_json(res.into_body()).await["metadata"]
        .get("team")
        .is_none());

    // properties：写 → 删（JSONB key = :propertyId）
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/properties/severity"),
            ws,
            user,
            Some(json!({"value": "p1"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        body_json(res.into_body()).await["properties"]["severity"],
        "p1"
    );
    let res = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/api/issues/{issue_id}/properties/severity"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    cleanup(&pool, ws, user).await;
}

/// 5) issue-statuses 目录：默认 7 个 → 自定义 key → 引用它的 issue → 删除保护。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_status_catalog_lifecycle() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    // 写路径限 owner/admin（上游 router.go L2051）：member 建 status → 403
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issue-statuses",
            ws,
            user,
            Some(json!({"name": "Nope", "category": "open"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 升为 owner 后再走后续写路径
    sqlx::query("UPDATE member SET role = 'owner' WHERE workspace_id = $1 AND user_id = $2")
        .bind(ws)
        .bind(user)
        .execute(&pool)
        .await
        .expect("promote to owner");

    // 首次读取 self-heal 出 7 个内置 status
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issue-statuses", ws, user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["total"], 7);
    assert_eq!(body["categories"], json!(["open", "closed"]));
    assert!(body["statuses"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["is_system"] == true));

    // 建自定义 status（不给 key → 从 name 派生）
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issue-statuses",
            ws,
            user,
            Some(json!({"name": "Blocked on QA", "category": "open", "icon": "🐢"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let custom = body_json(res.into_body()).await;
    assert_eq!(custom["key"], "blocked_on_qa");
    assert_eq!(custom["is_system"], false);
    let custom_id = custom["id"].as_str().unwrap().to_string();

    // 重复 key → 409
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issue-statuses",
            ws,
            user,
            Some(json!({"name": "dup", "key": "blocked_on_qa", "category": "open"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // 自定义 status 可用于建 issue，且回显 status_name
    let issue = create_issue(
        &app,
        ws,
        user,
        json!({"title": "waiting for qa", "status": "blocked_on_qa"}),
    )
    .await;
    assert_eq!(issue["status"], "blocked_on_qa");
    assert_eq!(issue["status_name"], "Blocked on QA");
    assert_eq!(issue["status_category"], "open");
    let issue_id = issue["id"].as_str().unwrap().to_string();

    // reorder
    let res = app
        .clone()
        .oneshot(req(
            "PATCH",
            "/api/issue-statuses/reorder",
            ws,
            user,
            Some(json!({"ids": [custom_id], "category": "open"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["total"], 8);

    // 仍被引用的自定义 status → 409
    let res = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/api/issue-statuses/{custom_id}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // 内置 status 不可删 → 409
    let builtin_todo = body_json(
        app.clone()
            .oneshot(req("GET", "/api/issue-statuses", ws, user, None))
            .await
            .unwrap()
            .into_body(),
    )
    .await["statuses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "todo")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/api/issue-statuses/{builtin_todo}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // 改 status（PATCH）→ 名字生效
    let res = app
        .clone()
        .oneshot(req(
            "PATCH",
            &format!("/api/issue-statuses/{custom_id}"),
            ws,
            user,
            Some(json!({"name": "QA blocked", "category": "closed"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let renamed = body_json(res.into_body()).await;
    assert_eq!(renamed["name"], "QA blocked");
    assert_eq!(renamed["category"], "closed");

    // 期望 revision 冲突之外：未知 issue → 404
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{}", Uuid::new_v4()),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let _ = issue_id;

    cleanup(&pool, ws, user).await;
}

/// 6) 鉴权 / workspace 解析 / 501 占位。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_auth_workspace_and_not_implemented() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    // 无 user header → 401
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/issues")
                .header(WORKSPACE_HEADER, ws.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 无 workspace → 400
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/issues")
                .header(USER_ID_HEADER, user.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 非成员 → 404（与上游一致：不泄露 workspace 是否存在）
    let outsider: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-outsider', $1) RETURNING id"#,
    )
    .bind(format!("outsider-{}@example.com", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .unwrap();
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/issues")
                .header(USER_ID_HEADER, outsider.to_string())
                .header(WORKSPACE_HEADER, ws.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 未知 workspace uuid → 404
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues", Uuid::new_v4(), user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // quick-create：降级实现（无 daemon → 同步落库），mutation 类端点在无 user header
    // 时先撞 401；带齐认证 + 空 body 则 400（title is required）
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/quick-create",
            ws,
            user,
            Some(json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // quick-create 降级路径：带 title → 201 + `origin = quick_create`
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/quick-create",
            ws,
            user,
            Some(json!({"title": "quick", "description": "via quick-create"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let quick = body_json(res.into_body()).await;
    assert_eq!(quick["title"], "quick");
    assert_eq!(quick["origin"], "quick_create");
    assert_eq!(quick["status"], "todo");

    // 501 占位（M3 能力）。原先这里断言的是 `/api/issues/table/groups`，
    // M2-D（LUM-1355）把它实现成真实路由后改用仍未实现的 `preview-trigger`
    // 继续覆盖“占位返回 501 + `not_implemented` 错误码”这条约定。
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/preview-trigger",
            ws,
            user,
            Some(json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    assert_eq!(
        body_json(res.into_body()).await["error"]["code"],
        "not_implemented"
    );

    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(outsider)
        .execute(&pool)
        .await;
    cleanup(&pool, ws, user).await;
}

/// 7) LUM-1410：`POST /api/issues` 的 `(assignee_type, assignee_id)` **存在性**校验与
/// `attachment_ids` 形态校验（上游 `validateAssigneePair` / `parseUUIDSliceOrBadRequest`）。
///
/// 对应 golden fixture `contracts/golden/issues/021..025`：真库层期望 400，此前实测 201。
/// 同步覆盖 `PUT /api/issues/:id`（上游同一把校验，`issue.go:3814`）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn create_issue_validates_assignee_and_attachment_ids() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    let issue_count = |pool: sqlx::PgPool, ws: Uuid| async move {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM issue WHERE workspace_id = $1")
            .bind(ws)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    // 本 workspace 里不存在的目标 id（fixture 021/022 用的就是全零 UUID）
    let ghost = Uuid::nil();

    let post = |body: serde_json::Value| {
        let app = app.clone();
        async move {
            app.oneshot(req("POST", "/api/issues", ws, user, Some(body)))
                .await
                .unwrap()
        }
    };

    // 1) 不存在的 member → 400（上游 `getMemberByUserAndWorkspace` 未命中）
    let res = post(json!({
        "title": "Ghost member assignee",
        "assignee_type": "member",
        "assignee_id": ghost.to_string(),
    }))
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(res.into_body()).await["error"]["code"],
        "validation_error"
    );

    // 2) 不存在的 agent → 400（上游此前是 403 "agent not found"，现为 400）
    let res = post(json!({
        "title": "Ghost agent assignee",
        "assignee_type": "agent",
        "assignee_id": ghost.to_string(),
    }))
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 2b) 不存在的 squad → 400（本仓同类校验；无 leader 的 squad 也拒）
    let res = post(json!({
        "title": "Ghost squad assignee",
        "assignee_type": "squad",
        "assignee_id": ghost.to_string(),
    }))
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 3) 只给一半 → 400（fixture 023 / 024：这两个在修复前就是 400，作为回归锚点保留）
    for body in [
        json!({"title": "Lone assignee_type", "assignee_type": "member"}),
        json!({"title": "Lone assignee_id", "assignee_id": "not-a-uuid"}),
    ] {
        let res = post(body).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    // 4) 两半都给但 id 形态非法 → 400（修复前这条会 201：只查了"成对"没查形态）
    let res = post(json!({
        "title": "Malformed assignee_id",
        "assignee_type": "member",
        "assignee_id": "not-a-uuid",
    }))
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 5) `attachment_ids` 含非 UUID → 400，且**写库之前**（fixture 025 的
    //    "BeforeWrite"：创建前后的 issue 计数必须相等）
    let before = issue_count(pool.clone(), ws).await;
    let res = post(json!({
        "title": "Malformed attachment issue",
        "attachment_ids": ["not-a-uuid"],
    }))
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(res.into_body()).await["error"]["code"],
        "validation_error"
    );
    assert_eq!(
        issue_count(pool.clone(), ws).await,
        before,
        "400 必须发生在写库之前"
    );

    // 6) 正向对照：真实 member + 合法 attachment UUID → 201（附件只校验形态、不绑定）
    let res = post(json!({
        "title": "Valid member assignee",
        "assignee_type": "member",
        "assignee_id": user.to_string(),
        "attachment_ids": [Uuid::new_v4().to_string()],
    }))
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let created = body_json(res.into_body()).await;
    // 上游入参写作 `member`，本仓落库/回显统一 `user`（见 docs/11 §5）
    assert_eq!(created["assignee_type"], "user");
    assert_eq!(created["assignee_id"], user.to_string());
    let issue_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(issue_count(pool.clone(), ws).await, before + 1);

    // 6b) 形态非法的 `assignee_id` 在 `null` 清除路径上不参与校验（三态补丁）
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"assignee_type": null, "assignee_id": null})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(body_json(res.into_body()).await["assignee_id"].is_null());

    // 7) PUT 路径同把校验：不存在的 member → 400（上游 `UpdateIssue` L3814）
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"assignee_type": "member", "assignee_id": ghost.to_string()})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 7b) PUT 指向真实的 member → 200（同一把校验不能误杀合法指派）
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"assignee_type": "member", "assignee_id": user.to_string()})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        body_json(res.into_body()).await["assignee_id"],
        user.to_string()
    );

    cleanup(&pool, ws, user).await;
}
