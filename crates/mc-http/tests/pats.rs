//! `/api/me/pats` 路由的端到端测试。
//!
//! PAT 后端是 `mc_auth::PatStoreContainer`（in-memory），不依赖数据库，
//! 所以可以构造真实的 `AppState` + `Router` 走完整 axum 调用栈。

#![cfg(feature = "test-util")]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use mc_auth::PatStore;
use mc_core::actor::ActorRegistry;
use mc_core::Id;
use mc_db::Db;
use mc_http::state::{AdapterRegistryStub, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use tower::ServiceExt;

const USER_ID_HEADER: &str = "x-multica-user-id";

async fn build_state() -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-test"));
    let actors = ActorRegistry::new();
    let adapters = Arc::new(AdapterRegistryStub::default());
    let db = Db::placeholder();
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

#[tokio::test]
async fn create_and_revoke_pat_round_trip() {
    let state = build_state().await;
    let app = mc_http::routes::router().with_state(state.clone());

    let user_id = Id::new();
    let create_body = serde_json::json!({
        "name": "ci-deploy",
        "scopes": ["read", "write"]
    });

    // 1) POST /api/me/pats
    let req = Request::builder()
        .method("POST")
        .uri("/api/me/pats")
        .header(USER_ID_HEADER, user_id.as_string())
        .header("content-type", "application/json")
        .body(Body::from(create_body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED, "create pat");
    let body = body_json(res.into_body()).await;
    let pat_id = body["id"].as_str().expect("id field").to_string();
    let token = body["token"].as_str().expect("token field").to_string();
    assert!(
        token.starts_with("mk_pat_"),
        "token has expected prefix: {token}"
    );
    assert_eq!(body["name"], "ci-deploy");
    assert_eq!(body["scopes"][0], "read");

    // 2) GET /api/me/pats —— 列表里有刚才创建的
    let req = Request::builder()
        .method("GET")
        .uri("/api/me/pats")
        .header(USER_ID_HEADER, user_id.as_string())
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1, "expected 1 pat, got {}: {arr:?}", arr.len());
    assert_eq!(arr[0]["id"], pat_id);

    // 3) DELETE /api/me/pats/{id}
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/me/pats/{pat_id}"))
        .header(USER_ID_HEADER, user_id.as_string())
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // 4) 再次 list —— 应该空了
    let req = Request::builder()
        .method("GET")
        .uri("/api/me/pats")
        .header(USER_ID_HEADER, user_id.as_string())
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn create_pat_rejects_empty_name() {
    let state = build_state().await;
    let app = mc_http::routes::router().with_state(state);

    let user_id = Id::new();
    let req = Request::builder()
        .method("POST")
        .uri("/api/me/pats")
        .header(USER_ID_HEADER, user_id.as_string())
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name": "  "}"#))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn missing_user_header_returns_401() {
    let state = build_state().await;
    let app = mc_http::routes::router().with_state(state);

    let req = Request::builder()
        .method("GET")
        .uri("/api/me/pats")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
