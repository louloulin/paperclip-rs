//! `POST /auth/google` 的用例（第 1 / 2 / 4 / 5 / 8 / 9-11 步）。
//!
//! stub 用本机临时 axum listener（`127.0.0.1:0`）：不引入 wiremock 这类新依赖，也不需要
//! 常驻服务。stub 的行为由**请求里的 code** 决定，所以一个 stub 服务能跑完全部用例，
//! 测试之间没有环境变量串扰（base URL 是构 state 时注入的，不读进程 env）。
//!
//! 拆分自拆分前的单文件 `routes/auth.rs`（LUM-2530）：item 逐字搬移
//! （本文件整体左移 4 空格 —— 原文这些行缩进在 `mod tests {` 里）。

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, Request, StatusCode};
use mc_db::Db;
use mc_repos::user::{NewUser, UserRepo};
use mc_repos::Repository;
use tower::ServiceExt;
use uuid::Uuid;

use super::super::router;
use super::{body_json, build_state, build_state_with_db};
use crate::state::{AppState, GoogleOAuthConfig};

use serde_json::json;

/// 出站配置：`client_id`/`client_secret` 齐全，base URL 指向本机 stub。
fn google_cfg(token_url: &str, userinfo_url: &str) -> GoogleOAuthConfig {
    GoogleOAuthConfig {
        client_id: Some("stub-client-id".into()),
        client_secret: Some("stub-client-secret".into()),
        redirect_uri: Some("https://app.test/auth/callback".into()),
        token_url: token_url.to_string(),
        userinfo_url: userinfo_url.to_string(),
        ..Default::default()
    }
}

/// 不需要 DB 的用例：`db_url` 指向一个连不上的地址（`connect_lazy` 不拨号）。
fn google_state(token_url: &str, userinfo_url: &str) -> Arc<AppState> {
    build_state_with_db(
        "postgres://127.0.0.1:1/none",
        60,
        true,
        google_cfg(token_url, userinfo_url),
    )
}

#[cfg(test)]
mod stub {
    use axum::http::header::AUTHORIZATION;
    use axum::response::{IntoResponse, Response};
    use axum::{Json, Router};

    use super::*;

    /// 起一个本机 stub Google：返回 `(token_url, userinfo_url)`。
    pub(super) async fn spawn_google_stub() -> (String, String) {
        use std::collections::HashMap;

        use axum::extract::Form;
        use axum::routing::{get, post};

        /// `POST /token`：校验表单字段名/值，再按 code 决定返回。
        async fn token(Form(form): Form<HashMap<String, String>>) -> Response {
            for key in ["code", "client_id", "client_secret", "redirect_uri"] {
                if !form.contains_key(key) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("missing form field {key}") })),
                    )
                        .into_response();
                }
            }
            if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "bad_grant_type" })),
                )
                    .into_response();
            }
            match form.get("code").map(String::as_str).unwrap_or_default() {
                "invalid_grant" => (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "invalid_grant" })),
                )
                    .into_response(),
                "provider_error" => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "internal" })),
                )
                    .into_response(),
                code => (
                    StatusCode::OK,
                    Json(json!({
                        "access_token": format!("tok-{code}"),
                        "id_token": "stub-id-token",
                        "token_type": "Bearer",
                    })),
                )
                    .into_response(),
            }
        }

        /// `GET /userinfo`：按 Bearer 里的 code 决定 profile。
        async fn userinfo(headers: HeaderMap) -> Response {
            let auth = headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            let Some(code) = auth.strip_prefix("Bearer tok-") else {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "invalid_token" })),
                )
                    .into_response();
            };
            match code {
                // 上游第 8 步：email 为空 → 400 `google_account_no_email`
                "noemail" => (
                    StatusCode::OK,
                    Json(json!({ "name": "No Email", "picture": "https://img.test/n.png" })),
                )
                    .into_response(),
                // 故意带大写 + 前后空格，验证 handler 的 trim + lowercase
                code => (
                    StatusCode::OK,
                    Json(json!({
                        "email": format!("  {code}@Stub.Test "),
                        "name": "Stub User",
                        "picture": "https://img.test/p.png",
                    })),
                )
                    .into_response(),
            }
        }

        let app = Router::new()
            .route("/token", post(token))
            .route("/userinfo", get(userinfo));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind google stub");
        let addr = listener.local_addr().expect("stub addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (
            format!("http://{addr}/token"),
            format!("http://{addr}/userinfo"),
        )
    }
}

use stub::spawn_google_stub;

/// POST `/auth/google`，返回 (status, body, headers)。
async fn post_google(
    state: Arc<AppState>,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value, HeaderMap) {
    let app = router().with_state(state);
    let req = Request::builder()
        .method("POST")
        .uri("/auth/google")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), 65536).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value, headers)
}

/// 上游 `writeFeatureDisabled`：未配置 → 403（故意不是 503）。
#[tokio::test]
async fn google_login_unconfigured_returns_feature_disabled() {
    // `build_state` 的默认 `GoogleOAuthConfig` 没有 client_id/secret
    let state = build_state(60, true);
    assert!(!state.google_oauth.is_configured());
    let (status, body, _) = post_google(state, json!({ "code": "anything" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "google_login_not_configured");
    assert_eq!(body["error"]["message"], "Google login is not configured");
}

/// 第 1/2 步：畸形 JSON 与缺失/空 `code` 都是 400。
#[tokio::test]
async fn google_login_requires_code_and_parseable_body() {
    let (token_url, userinfo_url) = spawn_google_stub().await;
    let state = google_state(&token_url, &userinfo_url);

    let app = router().with_state(state.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/auth/google")
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let (status, body) = body_json(app.oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "invalid request body");

    let (status, body, _) = post_google(state.clone(), json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["message"], "code is required");

    let (status, body, _) = post_google(state, json!({ "code": "" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "code is required");
}

/// 第 5 步：`400 + invalid_grant` → 400 `oauth_code_invalid`。
#[tokio::test]
async fn google_login_rejected_code_returns_oauth_code_invalid() {
    let (token_url, userinfo_url) = spawn_google_stub().await;
    let state = google_state(&token_url, &userinfo_url);
    let (status, body, _) = post_google(state, json!({ "code": "invalid_grant" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "oauth_code_invalid");
    assert_eq!(
        body["error"]["message"],
        "failed to exchange code with Google"
    );
}

/// 第 5 步的另一个分支：非 400（或 error 不是 `invalid_grant`）→ 502，
/// 且**没有** `oauth_code_invalid`（上游注释：只有 `invalid_grant` 才算用户错）。
#[tokio::test]
async fn google_login_provider_error_returns_502() {
    let (token_url, userinfo_url) = spawn_google_stub().await;
    let state = google_state(&token_url, &userinfo_url);
    let (status, body, _) = post_google(state, json!({ "code": "provider_error" })).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(
        body["error"]["message"],
        "failed to exchange code with Google"
    );
}

/// 第 4 步：token 端点连不上 → 502（不是 500）。
#[tokio::test]
async fn google_login_token_endpoint_unreachable_returns_502() {
    // 1/tcp 必然 connection refused（且不会真的发出去）
    let state = google_state("http://127.0.0.1:1/token", "http://127.0.0.1:1/userinfo");
    let (status, body, _) = post_google(state, json!({ "code": "abcd" })).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        body["error"]["message"],
        "failed to exchange code with Google"
    );
}

/// 第 8 步：Google 没给 email → 400 `google_account_no_email`。
#[tokio::test]
async fn google_login_without_email_returns_google_account_no_email() {
    let (token_url, userinfo_url) = spawn_google_stub().await;
    let state = google_state(&token_url, &userinfo_url);
    let (status, body, _) = post_google(state, json!({ "code": "noemail" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "google_account_no_email");
    assert_eq!(
        body["error"]["message"],
        "Google did not provide an email address for this sign-in"
    );
}

/// gate ⑥（`--ignored`）用 `MULTICA_TEST_DATABASE_URL`；本地手跑可用 `DATABASE_URL`。
fn require_test_db() -> Option<String> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|u| !u.trim().is_empty());
    if url.is_none() {
        eprintln!("MULTICA_TEST_DATABASE_URL/DATABASE_URL not set; skipping google e2e test");
    }
    url
}

/// 第 9-11 步（happy path）：建号 → 回填 profile → 签发 session/cookie → 200。
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn google_login_creates_user_and_returns_token() {
    let Some(db_url) = require_test_db() else {
        return;
    };
    let (token_url, userinfo_url) = spawn_google_stub().await;
    // email 在 stub 里带大写 + 前后空格 → 验证 handler 的 trim + lowercase
    let code = format!("Happy-{}", Uuid::new_v4());
    let email = format!("{}@stub.test", code.to_lowercase());
    let state = build_state_with_db(&db_url, 60, true, google_cfg(&token_url, &userinfo_url));

    let (status, body, headers) = post_google(state.clone(), json!({ "code": code })).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // 上游 `LoginResponse{token, user}`
    let session_id = body["token"].as_str().expect("token").to_string();
    assert!(!session_id.is_empty());
    assert_eq!(body["user"]["email"], email);
    // 新建用户默认 name = email 前缀，随后被 Google profile 覆盖
    assert_eq!(body["user"]["name"], "Stub User");

    // cookie + CSRF 头复用 verify-code 的写入路径
    let cookie = headers
        .get(SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(cookie.starts_with("multica_session="), "cookie: {cookie}");
    let session = state
        .auth
        .store()
        .get(&session_id)
        .await
        .expect("session stored");
    assert_eq!(
        headers.get("x-multica-csrf").and_then(|v| v.to_str().ok()),
        Some(session.csrf_token.as_str())
    );

    // 落库结果：name 被 profile 覆盖，avatar 落库，email 归一化
    let db = Db::connect(&db_url, 4, 1).await.expect("db");
    let users = UserRepo::new(db);
    let stored = users
        .get_by_email(&email)
        .await
        .expect("lookup")
        .expect("user row created by google login");
    assert_eq!(stored.name, "Stub User");
    assert_eq!(stored.avatar_url.as_deref(), Some("https://img.test/p.png"));
    assert_eq!(stored.id, session.user_id);
    assert_eq!(body["user"]["id"], stored.id.as_string());
    users.delete(&stored.id).await.ok();
}

/// 第 10 步的边界：用户改过 name / 已有 avatar 时**不**回填（上游只在
/// 「name 仍等于 email 前缀」与「avatar 为空」时写库）。
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn google_login_keeps_named_user_and_existing_avatar() {
    let Some(db_url) = require_test_db() else {
        return;
    };
    let (token_url, userinfo_url) = spawn_google_stub().await;
    let code = format!("keep-{}", Uuid::new_v4());
    let email = format!("{code}@stub.test");

    let db = Db::connect(&db_url, 4, 1).await.expect("db");
    let users = UserRepo::new(db);
    let seeded = users
        .create(NewUser {
            name: "Custom Name".into(),
            email: email.clone(),
            avatar_url: Some("https://mine.test/a.png".into()),
        })
        .await
        .expect("seed user");

    let state = build_state_with_db(&db_url, 60, true, google_cfg(&token_url, &userinfo_url));
    let (status, body, _) = post_google(state, json!({ "code": code })).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["name"], "Custom Name");

    let stored = users
        .get_by_email(&email)
        .await
        .expect("lookup")
        .expect("seeded row");
    assert_eq!(stored.id, seeded.id);
    assert_eq!(stored.name, "Custom Name");
    assert_eq!(
        stored.avatar_url.as_deref(),
        Some("https://mine.test/a.png")
    );
    users.delete(&seeded.id).await.ok();
}
