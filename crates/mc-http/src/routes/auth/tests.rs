//! `routes/auth.rs` 的单元测试 —— 用 `tower::ServiceExt::oneshot` 走真实 axum Router。
//!
//! 重点：rate limit、consumed、expired 三种失败路径。
//!
//! 拆两半：验证码 / session / cli-token 路径在本文件；`/auth/google` 的 8 条用例在
//! [`google`]（本机 stub 服务，用到的共用件仍在本文件）。
//!
//! 拆分自拆分前的单文件 `routes/auth.rs`（LUM-2530）：item 逐字搬移
//! （本文件整体左移 4 空格 —— 原文这些行缩进在 `mod tests {` 里）。

use std::sync::Arc;

use axum::http::header::SET_COOKIE;
use axum::http::StatusCode;
use axum::response::Response;
use chrono::Utc;
use mc_auth::session::Session;
use mc_core::Id;
use uuid::Uuid;

use super::cli_token::sha256_hex;
use super::common::hash_code;
use super::router;
use crate::state::{AppState, GoogleOAuthConfig};

mod google;

use crate::state::{AdapterRegistry, ChannelKeys, ConfigSnapshot, RuntimeHandles};
use axum::body::{to_bytes, Body};
use axum::http::Request;
use mc_auth::{SessionStoreContainer, VerificationStoreContainer};
use mc_db::Db;
use mc_realtime::{RealtimeHandle, WsState};
use tower::ServiceExt;

/// 构造测试用 AppState。DB 使用 `connect_lazy` —— 不会立即拨号；
/// 需要真 DB 的测试必须在函数顶部检查 `DATABASE_URL` 并跳过。
fn build_state(session_ttl_secs: u64, dev_mode: bool) -> Arc<AppState> {
    let db_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://127.0.0.1:1/none".to_string());
    build_state_with_db(
        &db_url,
        session_ttl_secs,
        dev_mode,
        GoogleOAuthConfig::default(),
    )
}

/// `build_state` 的完整形式：显式 `db_url` + 显式 Google 出站配置。
fn build_state_with_db(
    db_url: &str,
    session_ttl_secs: u64,
    dev_mode: bool,
    google_oauth: GoogleOAuthConfig,
) -> Arc<AppState> {
    let db = Db::connect_lazy(db_url, 4, 1).expect("lazy db");

    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "test"));
    let actors = mc_core::actor::ActorRegistry::new();
    let adapters = Arc::new(AdapterRegistry::default());
    let state = AppState {
        db,
        runtime: RuntimeHandles { actors, adapters },
        // M3 anchor（LUM-1406）：仅显式给 `port` / `dev_mode` / `session_ttl_secs`，其余同默认。
        config: ConfigSnapshot {
            port: 0,
            dev_mode,
            session_ttl_secs,
            ..ConfigSnapshot::default()
        },
        storage: mc_storage::Storage::new(),
        secrets: mc_secrets::Secrets::new(mc_auth::DefaultSecretsBackend::in_memory()),
        feature_flags: Arc::new(mc_feature_flags::FeatureFlagCatalog::new()),
        realtime,
        ws,
        auth: SessionStoreContainer::new(),
        pat: mc_auth::PatStoreContainer::new(),
        verification: VerificationStoreContainer::new(),
        google_oauth,
        daemon_hub: std::sync::Arc::new(mc_ws::hub::Hub::new()),
        daemon_requests: std::sync::Arc::new(crate::daemon_requests::RequestStore::new()),
        // M6 anchor（LUM-1665）：插件部署配置显式 `None`；解析口径见 `state.rs` 的用例。
        plugin_key: None,
        plugin_surface_origin: None,
        // M7 anchor（LUM-1765）：渠道部署密钥显式未配置（口径见 `state.rs`）。
        channel_keys: ChannelKeys::default(),
        github_keys: crate::state::integrations::GithubKeys::default(),
        vcs_keys: crate::state::integrations::VcsKeys::default(),
        composio_keys: crate::state::integrations::ComposioKeys::default(),
        // M9 anchor（`LUM-1815`）：云面两组字段显式未配置（口径见 `state/cloud.rs`；全仓 10 个字面量构造点同步，见 `docs/32` §9.13）。
        cloud: crate::state::cloud::CloudConfig::from_env_with(|_| None),
        entitlement: crate::state::cloud::EntitlementConfig::from_env_with(|_| None),
    };
    Arc::new(state)
}

async fn body_json(resp: Response) -> (StatusCode, serde_json::Value) {
    let status = resp.status();
    let body = to_bytes(resp.into_body(), 65536).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, value)
}

fn require_db() -> bool {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set; skipping e2e test");
        false
    } else {
        true
    }
}

#[tokio::test]
async fn send_then_verify_full_flow() {
    if !require_db() {
        return;
    }
    let state = build_state(60, true);
    let app = router().with_state(state);

    let email = format!("flow-{}@example.test", Uuid::new_v4());

    // send-code
    let req = Request::builder()
        .method("POST")
        .uri("/auth/send-code")
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"email":"{email}"}}"#)))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (_, body) = body_json(resp).await;
    let code = body
        .get("dev_code")
        .and_then(|v| v.as_str())
        .expect("dev_code present")
        .to_string();

    // verify-code
    let req = Request::builder()
        .method("POST")
        .uri("/auth/verify-code")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"email":"{email}","code":"{code}"}}"#
        )))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let session_cookie = resp
        .headers()
        .get(SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    assert!(session_cookie.starts_with("multica_session="));
    let csrf = resp
        .headers()
        .get("x-multica-csrf")
        .and_then(|v| v.to_str().ok())
        .expect("csrf header")
        .to_string();
    assert!(!csrf.is_empty());
}

#[tokio::test]
async fn verify_with_wrong_code_returns_401() {
    if !require_db() {
        return;
    }
    let state = build_state(60, true);
    let app = router().with_state(state);
    let email = format!("bad-{}@example.test", Uuid::new_v4());

    let req = Request::builder()
        .method("POST")
        .uri("/auth/verify-code")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"email":"{email}","code":"000000"}}"#
        )))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn verify_with_consumed_code_returns_401() {
    if !require_db() {
        return;
    }
    let state = build_state(60, true);
    let app = router().with_state(state.clone());
    let email = format!("reuse-{}@example.test", Uuid::new_v4());

    // send
    let req = Request::builder()
        .method("POST")
        .uri("/auth/send-code")
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"email":"{email}"}}"#)))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let (_, body) = body_json(resp).await;
    let code = body
        .get("dev_code")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    // 第一次 verify 成功
    let req = Request::builder()
        .method("POST")
        .uri("/auth/verify-code")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"email":"{email}","code":"{code}"}}"#
        )))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 第二次 verify（已消费）失败
    let req = Request::builder()
        .method("POST")
        .uri("/auth/verify-code")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"email":"{email}","code":"{code}"}}"#
        )))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn verify_with_expired_code_returns_401() {
    if !require_db() {
        return;
    }
    let state = build_state(60, true);
    // 直接造一条已过期 + 未消费的 code
    let email = format!("expired-{}@example.test", Uuid::new_v4());
    sqlx::query(
        r"
        INSERT INTO verification_code
            (id, email, purpose, code, expires_at, created_at)
        VALUES ($1, $2, 'email_verification', $3,
                now() - interval '1 hour', now() - interval '2 hours')
        ",
    )
    .bind(Uuid::new_v4())
    .bind(&email)
    .bind(hash_code("999999"))
    .execute(state.db.pool())
    .await
    .unwrap();

    let app = router().with_state(state);
    let req = Request::builder()
        .method("POST")
        .uri("/auth/verify-code")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"email":"{email}","code":"999999"}}"#
        )))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refresh_renews_session() {
    // 该路径仅走 in-memory SessionStore，不需要 DB
    let state = build_state(60, true);
    let app = router().with_state(state.clone());

    // 手动塞一个 session 进去
    let user_id = Id::new();
    let session = Session::new(user_id, 60);
    let sid = session.id.clone();
    state.auth.store().put(session).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("/api/auth/refresh")
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"session_id":"{sid}"}}"#)))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (_, body) = body_json(resp).await;
    assert_eq!(
        body.get("session_id").and_then(|v| v.as_str()),
        Some(sid.as_str())
    );
}

/// `/api/cli-token`：dev header 路径 → 200 + 可用的 PAT（真正落 store）。
#[tokio::test]
async fn cli_token_issues_usable_pat() {
    let state = build_state(60, true);
    let app = router().with_state(state.clone());
    let user_id = Id::new();

    let req = Request::builder()
        .method("POST")
        .uri("/api/cli-token")
        .header("x-multica-user-id", user_id.as_string())
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (_, body) = body_json(resp).await;
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .expect("token")
        .to_string();
    assert!(token.starts_with("mk_pat_"), "token prefix: {token}");
    assert_eq!(
        body.get("user_id").and_then(|v| v.as_str()),
        Some(user_id.as_string().as_str())
    );
    assert_eq!(
        body.get("scopes")
            .and_then(|v| v.get(0))
            .and_then(|v| v.as_str()),
        Some("cli")
    );

    // token 必须真的能被 PatStore 反查到（sha256(raw) 为 key）——这正是
    // LUM-1335 原实现缺的一步（它只拼字符串、不落库）。
    let raw = token.trim_start_matches(crate::routes::pats::PAT_PREFIX);
    let stored = state
        .pat
        .store()
        .get_by_hash(&sha256_hex(raw))
        .await
        .expect("pat is persisted");
    assert_eq!(stored.user_id, user_id);
    assert_eq!(stored.name, "cli");
    assert!(stored.expires_at > Utc::now());
}

/// cookie 会话路径：有效 session → 200；无 session 且无 header → 401。
#[tokio::test]
async fn cli_token_uses_session_cookie_and_requires_auth() {
    let state = build_state(60, true);
    let app = router().with_state(state.clone());
    let user_id = Id::new();
    let session = Session::new(user_id, 60);
    let sid = session.id.clone();
    state.auth.store().put(session).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("/api/cli-token")
        .header("cookie", format!("multica_session={sid}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (_, body) = body_json(resp).await;
    assert_eq!(
        body.get("user_id").and_then(|v| v.as_str()),
        Some(user_id.as_string().as_str())
    );

    // 既无 cookie 也无 header → 401
    let req = Request::builder()
        .method("POST")
        .uri("/api/cli-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refresh_unknown_session_returns_401() {
    let state = build_state(60, true);
    let app = router().with_state(state);
    let req = Request::builder()
        .method("POST")
        .uri("/api/auth/refresh")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"session_id":"{}"}}"#,
            Uuid::new_v4()
        )))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
