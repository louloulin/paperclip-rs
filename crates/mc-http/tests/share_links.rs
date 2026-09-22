//! `/api/workspaces/{id}/share-links` + `/api/share-links/*` 端到端测试。
//!
//! 数据落在 `workspace_share_link` / `member` 表，需要真实 PG。
//! 本文件的测试均为 `#[ignore]`，通过 `MULTICA_TEST_DATABASE_URL` 触发。
//!
//! 运行示例：
//! ```
//! MULTICA_TEST_DATABASE_URL=postgres://u:p@host:5432/db \
//!   cargo test -p mc-http --test share_links --features test-util -- --ignored
//! ```

#![cfg(feature = "test-util")]

use std::env;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_core::Id;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use tower::ServiceExt;
use uuid::Uuid;

const USER_ID_HEADER: &str = "x-multica-user-id";

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

/// 建一个 workspace + owner member + 一个"路人" user（用于 join）。
async fn seed(pool: &sqlx::PgPool) -> (Uuid, Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-share-ws', $1) RETURNING id",
    )
    .bind(format!("itest-share-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let owner: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-share-owner', $1) RETURNING id"#,
    )
    .bind(format!("share-owner-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert owner");

    let joiner: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-share-joiner', $1) RETURNING id"#,
    )
    .bind(format!("share-joiner-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert joiner");

    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(workspace_id)
        .bind(owner)
        .execute(pool)
        .await
        .expect("insert owner member");

    (workspace_id, owner, joiner)
}

async fn cleanup(pool: &sqlx::PgPool, workspace_id: Uuid, owner: Uuid, joiner: Uuid) {
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    for u in [owner, joiner] {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(u)
            .execute(pool)
            .await;
    }
}

/// 1) owner 创建 share link → 公开面可查 → joiner 用 code 加入 → 成为 member；
///    再次 join 幂等（`joined=false`，不再消耗 `use_count`）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn create_share_link_then_public_info_and_join() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, owner, joiner) = seed(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    // --- POST /api/workspaces/:id/share-links （owner 可创建）---
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workspaces/{ws}/share-links"))
        .header(USER_ID_HEADER, Id(owner).as_string())
        .header("content-type", "application/json")
        .body(Body::from(r#"{"role":"member","max_uses":5}"#))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED, "owner can create");
    let link = body_json(res.into_body()).await;
    let code = link["code"].as_str().expect("code").to_string();
    assert_eq!(code.len(), 10);
    assert_eq!(link["use_count"], 0);
    assert_eq!(link["is_active"], true);

    // --- GET /api/share-links/:code （公开，无需登录）---
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/share-links/{code}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let info = body_json(res.into_body()).await;
    assert_eq!(info["valid"], true);
    assert_eq!(info["role"], "member");

    // --- GET /api/workspaces/:id/share-links （member 以上可读）---
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/workspaces/{ws}/share-links"))
        .header(USER_ID_HEADER, Id(owner).as_string())
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let listed = body_json(res.into_body()).await;
    assert_eq!(
        listed.as_array().map(Vec::len),
        Some(1),
        "one link: {listed}"
    );

    // --- 非成员读列表 → 404（不泄露存在性）---
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/workspaces/{ws}/share-links"))
        .header(USER_ID_HEADER, Id(joiner).as_string())
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // --- POST /api/share-links/join ---
    let req = Request::builder()
        .method("POST")
        .uri("/api/share-links/join")
        .header(USER_ID_HEADER, Id(joiner).as_string())
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"code":"{code}"}}"#)))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let join_body = body_json(res.into_body()).await;
    assert_eq!(join_body["joined"], true);

    let role: String =
        sqlx::query_scalar("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(ws)
            .bind(joiner)
            .fetch_one(&pool)
            .await
            .expect("joiner is a member now");
    assert_eq!(role, "member");

    // --- 再 join 一次：幂等，`joined=false` 且不再消耗 use_count ---
    let req = Request::builder()
        .method("POST")
        .uri("/api/share-links/join")
        .header(USER_ID_HEADER, Id(joiner).as_string())
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"code":"{code}"}}"#)))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let again = body_json(res.into_body()).await;
    assert_eq!(again["joined"], false, "second join is idempotent");

    let use_count: i32 =
        sqlx::query_scalar("SELECT use_count FROM workspace_share_link WHERE code = $1")
            .bind(&code)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(use_count, 1, "idempotent re-join must not consume a use");

    cleanup(&pool, ws, owner, joiner).await;
}

/// 2) 撤销后公开面 `valid=false`，join 返回 400（Validation）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn revoked_link_is_no_longer_usable() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping");
        return;
    };
    let (ws, owner, joiner) = seed(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workspaces/{ws}/share-links"))
        .header(USER_ID_HEADER, Id(owner).as_string())
        .header("content-type", "application/json")
        .body(Body::from(r"{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let link = body_json(res.into_body()).await;
    let link_id = link["id"].as_str().unwrap().to_string();
    let code = link["code"].as_str().unwrap().to_string();
    assert_eq!(link["role"], "member", "role defaults to member");

    // --- DELETE 撤销（owner 可撤销）---
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/workspaces/{ws}/share-links/{link_id}"))
        .header(USER_ID_HEADER, Id(owner).as_string())
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // --- 公开面：valid=false ---
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/share-links/{code}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.into_body()).await["valid"], false);

    // --- join → 400 ---
    let req = Request::builder()
        .method("POST")
        .uri("/api/share-links/join")
        .header(USER_ID_HEADER, Id(joiner).as_string())
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"code":"{code}"}}"#)))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    cleanup(&pool, ws, owner, joiner).await;
}

/// 3) 权限：非 owner/admin 创建 → 403；陌生人撤销 → 404。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn share_link_requires_admin_role() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping");
        return;
    };
    let (ws, owner, joiner) = seed(&pool).await;
    // joiner 以普通 member 身份加入（不经 share link，直接插 member）
    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'member')")
        .bind(ws)
        .bind(joiner)
        .execute(&pool)
        .await
        .expect("insert member");
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    // member 创建 → 403
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workspaces/{ws}/share-links"))
        .header(USER_ID_HEADER, Id(joiner).as_string())
        .header("content-type", "application/json")
        .body(Body::from(r"{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 陌生人（非成员）创建 → 404
    let stranger: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-stranger', $1) RETURNING id"#,
    )
    .bind(format!("stranger-{}@example.com", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workspaces/{ws}/share-links"))
        .header(USER_ID_HEADER, Id(stranger).as_string())
        .header("content-type", "application/json")
        .body(Body::from(r"{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 无登录头 → 401
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workspaces/{ws}/share-links"))
        .header("content-type", "application/json")
        .body(Body::from(r"{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    cleanup(&pool, ws, owner, joiner).await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(stranger)
        .execute(&pool)
        .await;
}
