//! `/api/workspaces/{id}/invitations` 系列路由的端到端测试。
//!
//! 邀请存储在 `workspace_invitation` 表 + `member` 表，需要真实 PG。
//! 本文件包含 `#[ignore]` 标注的集成测试，通过 `MULTICA_TEST_DATABASE_URL` env
//! 触发；无 DB 时静默 skip。
//!
//! 运行示例：
//! ```
//! MULTICA_TEST_DATABASE_URL=postgres://u:p@host:5432/db \
//!   cargo test -p mc-http --test invitations --features test-util -- --ignored
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
use mc_http::state::{AdapterRegistryStub, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use tower::ServiceExt;
use uuid::Uuid;

const USER_ID_HEADER: &str = "x-multica-user-id";

async fn build_state_with_db(db: Db) -> Arc<AppState> {
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

/// 准备一个 workspace + inviter member（admin role）。
async fn seed_admin_and_workspace(pool: &sqlx::PgPool) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-inv-ws', $1) RETURNING id",
    )
    .bind(format!("itest-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let inviter: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-inv-admin', $1) RETURNING id"#,
    )
    .bind(format!("inviter-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert inviter");

    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'admin')")
        .bind(workspace_id)
        .bind(inviter)
        .execute(pool)
        .await
        .expect("insert member");

    (workspace_id, inviter)
}

async fn cleanup(pool: &sqlx::PgPool, workspace_id: Uuid, inviter: Uuid) {
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM \"user\" WHERE id = $1")
        .bind(inviter)
        .execute(pool)
        .await;
}

/// 1) admin 邀请 → list_my_invitations 收到。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn admin_invite_then_list_my_invitations() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };

    let (ws, inviter) = seed_admin_and_workspace(&pool).await;
    let inviter_id = Id(inviter);
    let state = build_state_with_db(db).await;
    let app = mc_http::routes::router().with_state(state.clone());

    // POST 邀请
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workspaces/{ws}/invitations"))
        .header(USER_ID_HEADER, inviter_id.as_string())
        .header("content-type", "application/json")
        .header("x-multica-user-email", "alice@example.com")
        .body(Body::from(
            r#"{"email": "alice@example.com", "role": "member"}"#,
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["email"], "alice@example.com");
    assert_eq!(body["role"], "member");

    // 用 inviter 的 id + x-multica-user-email = alice 来调 list_my_invitations
    let req = Request::builder()
        .method("GET")
        .uri("/api/invitations")
        .header(USER_ID_HEADER, inviter_id.as_string())
        .header("x-multica-user-email", "alice@example.com")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    let arr = body.as_array().expect("array");
    assert!(
        !arr.is_empty(),
        "expected at least one invitation, got {body}"
    );
    assert_eq!(arr[0]["email"], "alice@example.com");

    cleanup(&pool, ws, inviter).await;
}

/// 2) admin 邀请 → accept → 自动成为 member。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn accept_invitation_becomes_member() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping");
        return;
    };
    let (ws, inviter) = seed_admin_and_workspace(&pool).await;

    // 创建 recipient user
    let recipient: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-inv-recipient', $1) RETURNING id"#,
    )
    .bind(format!("recipient-{}@example.com", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .expect("insert recipient");

    let inviter_id = Id(inviter);
    let recipient_id = Id(recipient);
    let state = build_state_with_db(db).await;
    let app = mc_http::routes::router().with_state(state.clone());

    // admin invite recipient
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workspaces/{ws}/invitations"))
        .header(USER_ID_HEADER, inviter_id.as_string())
        .header("content-type", "application/json")
        .header("x-multica-user-email", "anyone@example.com")
        .body(Body::from(
            r#"{"email": "anyone@example.com", "role": "member"}"#,
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let body = body_json(res.into_body()).await;
    let inv_id = body["id"].as_str().expect("id").to_string();

    // recipient accepts
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/invitations/{inv_id}/accept"))
        .header(USER_ID_HEADER, recipient_id.as_string())
        .header("x-multica-user-email", "anyone@example.com")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "accept should succeed");
    let body = body_json(res.into_body()).await;
    assert_eq!(body["member"]["user_id"], recipient_id.as_string());
    assert_eq!(body["member"]["workspace_id"], ws.to_string());
    assert!(body["already_accepted"] == false);

    // 校验 member 表里真的有这条
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::BIGINT FROM member WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(ws)
    .bind(recipient)
    .fetch_one(&pool)
    .await
    .expect("count member");
    assert_eq!(count, 1, "expected exactly 1 member row");

    cleanup(&pool, ws, inviter).await;
    let _ = sqlx::query("DELETE FROM \"user\" WHERE id = $1")
        .bind(recipient)
        .execute(&pool)
        .await;
}

/// 3) 速率限制：连续 N+1 次邀请第 N+1 次 429。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn rate_limit_rejects_extra_invite() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping");
        return;
    };
    let (ws, inviter) = seed_admin_and_workspace(&pool).await;

    // 直接走 repo 层插 50 条
    use mc_core::workspace::WorkspaceRole;
    use mc_repos::invitation::{InvitationRepo, NewInvitation};
    let repo = InvitationRepo::new(&db);
    let inviter_id = Id(inviter);
    for i in 0..50 {
        repo.create(NewInvitation {
            workspace_id: Id(ws),
            email: format!("seed{i}@example.com"),
            role: WorkspaceRole::Member,
            invited_by_user_id: inviter_id,
            ttl_secs: Some(3600),
        })
        .await
        .expect("seed invite");
    }

    let state = build_state_with_db(db).await;
    let app = mc_http::routes::router().with_state(state.clone());

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workspaces/{ws}/invitations"))
        .header(USER_ID_HEADER, inviter_id.as_string())
        .header("content-type", "application/json")
        .header("x-multica-user-email", "alice@example.com")
        .body(Body::from(
            r#"{"email": "alice@example.com", "role": "member"}"#,
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "expected 429 after 50 invites in 1h"
    );

    cleanup(&pool, ws, inviter).await;
}
