//! `/auth/*` + `/api/auth/*` 切片。
//!
//! M1-B 认证流（对应 upstream `multica/server/internal/handler/auth.go` +
//! `session.go`）：
//!
//! | Method | Path | Handler | 上游对应 |
//! | --- | --- | --- | --- |
//! | POST | `/auth/send-code` | `send_code` | `Handler.SendCode` (auth.go) |
//! | POST | `/auth/verify-code` | `verify_code` | `Handler.VerifyCode` (auth.go) |
//! | POST | `/auth/logout` | `logout` | `Handler.Logout` (auth.go) |
//! | POST | `/api/auth/refresh` | `refresh_session` | `Handler.RefreshSession` (session.go) |
//!
//! 路径遵循 upstream：浏览器登录页用 `/auth/send-code`、`/auth/verify-code`，
//! 而已经 cookie 化的会话通过 `/api/auth/refresh` 续期。
//!
//! 邮件发送在本 sub-issue 用 `tracing::info!` 占位，不接 SMTP ——
//! M9（mailer）才会接 Resend / SES。`MULTICA_DEV_VERIFICATION_CODE` 在
//! 非 production 模式下作为万能验证码（参考 upstream `isDevVerificationCode`）。

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use mc_auth::cookie::{CookieOptions, SameSite};
use mc_auth::session::Session;
use mc_auth::verification::VerificationCodePurpose;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::verification_code::{NewVerificationCode, VerificationCodeRepo};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Auth 路由切片。
///
/// 返回的 Router 类型是 `Router<Arc<AppState>>`（未注入 state），
/// 在 `mount.rs::router()` 合并后由 `mc-http::router()` 顶层 `.with_state()` 注入。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/send-code", post(send_code))
        .route("/auth/verify-code", post(verify_code))
        .route("/auth/logout", post(logout))
        .route("/api/auth/refresh", post(refresh_session))
        // M1-D（LUM-1347）从 LUM-1335（`feat/multica-rs-m1`）cherry-pick 的增量：
        // CLI 登录用的一次性 PAT（浏览器会话 → token）。
        .route("/api/auth/cli-token", post(cli_token))
    // 注：`/api/me` 由 M1-A 的 routes/workspaces.rs 真实实现（仲裁 #4）。
    // 此处不得再注册同 path+method —— axum 0.7 `.merge` 重复注册会 panic。
}

// ============================================================
// 公共工具
// ============================================================

/// 把 raw 6 位数字 code 换算成 hex(sha256(code))。
fn hash_code(code: &str) -> String {
    let mut h = Sha256::new();
    h.update(code.as_bytes());
    hex::encode(h.finalize())
}

/// 是否 6 位数字。
fn is_six_digits(code: &str) -> bool {
    code.len() == 6 && code.chars().all(|c| c.is_ascii_digit())
}

/// 从 `MULTICA_DEV_VERIFICATION_CODE` 环境变量读取万能验证码（仅 dev）。
fn dev_verification_code() -> Option<String> {
    let raw = std::env::var("MULTICA_DEV_VERIFICATION_CODE").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 是否 dev 模式 —— 上游 `APP_ENV != "production"` 才允许万能码。
fn dev_mode(state: &AppState) -> bool {
    state.config.dev_mode
}

/// 构造 session cookie + CSRF 响应头 + JSON body 合并响应。
fn session_response(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    body: serde_json::Value,
) -> Response {
    let ttl_secs = state.config.session_ttl_secs;
    let mut headers = HeaderMap::new();

    // 1. Set session cookie (HttpOnly + SameSite=Lax + Secure 在 production)
    let mut cookie = CookieOptions::session_cookie(&state.config.session_cookie, &session.id);
    cookie.max_age_secs = Some(ttl_secs);
    cookie.secure = !state.config.dev_mode;
    cookie.http_only = true;
    cookie.same_site = SameSite::Lax;
    let cookie_str = cookie.render();
    if let Ok(v) = HeaderValue::from_str(&cookie_str) {
        headers.insert(SET_COOKIE, v);
    }

    // 2. CSRF header —— 与 session.csrf_token 对齐；前端把它回写到
    //    X-Multica-Csrf 做 CSRF 防护（MUL-7436）。
    if let Ok(v) = HeaderValue::from_str(&session.csrf_token) {
        headers.insert(axum::http::HeaderName::from_static("x-multica-csrf"), v);
    }

    let mut response = (status, Json(body)).into_response();
    response.headers_mut().extend(headers);
    response
}

// ============================================================
// POST /auth/send-code
// ============================================================

#[derive(Debug, Deserialize)]
pub struct SendCodeRequest {
    pub email: String,
    #[serde(default)]
    pub purpose: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SendCodeResponse {
    pub message: &'static str,
    /// 仅 dev 模式返回 —— 便于本地 curl 测试；production 永远 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_code: Option<String>,
}

async fn send_code(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SendCodeRequest>,
) -> ApiResult<Response> {
    let email = req.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError(Error::Validation {
            message: "email is required".into(),
            details: vec![],
        }));
    }

    let purpose = match req.purpose.as_deref().unwrap_or("email_verification") {
        "password_reset" => VerificationCodePurpose::PasswordReset,
        "two_factor" => VerificationCodePurpose::TwoFactor,
        "workspace_invite" => VerificationCodePurpose::WorkspaceInvite,
        _ => VerificationCodePurpose::EmailVerification,
    };

    let repo = VerificationCodeRepo::new(state.db.clone());

    // 速率限制 —— 单邮箱每分钟上限（参考 upstream RATE_LIMIT_AUTH_VERIFY）。
    let per_min = i64::from(state.config.send_code_per_email_per_min.max(1));
    let recent = repo
        .recent_for(&email, 60)
        .await
        .map_err(|e| ApiError(Error::Database(e.to_string())))?;
    if recent >= per_min {
        return Err(ApiError(Error::RateLimited {
            retry_after_secs: 60,
        }));
    }

    // 生成 6 位数字 code
    let code = format!("{:06}", rand::random::<u32>() % 1_000_000);
    let ttl_secs = state.config.verification_code_ttl_secs;
    let expires_at: DateTime<Utc> =
        Utc::now() + Duration::seconds(i64::try_from(ttl_secs).unwrap_or(i64::MAX));

    let _ = repo
        .create(NewVerificationCode {
            email: Some(email.clone()),
            user_id: None,
            purpose,
            code_hash: hash_code(&code),
            expires_at,
        })
        .await
        .map_err(|e| ApiError(Error::Database(e.to_string())))?;

    // 占位邮件 —— production 不打印 code，仅 email；dev 模式下把 code 一并
    // 写到日志，便于 `curl /auth/send-code` 后跟 verify。
    if dev_mode(&state) {
        tracing::info!(email = %email, code = %code, purpose = ?purpose, "verification code issued (dev placeholder)");
    } else {
        tracing::info!(email = %email, purpose = ?purpose, "verification code issued (mailer placeholder, M9 will send SMTP)");
    }

    let body = SendCodeResponse {
        message: "Verification code sent",
        dev_code: dev_mode(&state).then_some(code),
    };
    Ok((StatusCode::OK, Json(body)).into_response())
}

// ============================================================
// POST /auth/verify-code
// ============================================================

#[derive(Debug, Deserialize)]
pub struct VerifyCodeRequest {
    pub email: String,
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct UserView {
    pub id: String,
    pub name: String,
    pub email: String,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct VerifyCodeResponse {
    pub user: UserView,
    pub session_id: String,
    pub csrf_token: String,
}

async fn verify_code(
    State(state): State<Arc<AppState>>,
    Json(req): Json<VerifyCodeRequest>,
) -> ApiResult<Response> {
    let email = req.email.trim().to_lowercase();
    let code = req.code.trim();

    if email.is_empty() || !email.contains('@') {
        return Err(ApiError(Error::Validation {
            message: "email is required".into(),
            details: vec![],
        }));
    }
    if !is_six_digits(code) {
        return Err(ApiError(Error::Validation {
            message: "code must be 6 digits".into(),
            details: vec![],
        }));
    }

    // 校验代码 —— dev 模式下接受 `MULTICA_DEV_VERIFICATION_CODE` 万能码。
    let mut is_dev_pass = false;
    if dev_mode(&state) {
        if let Some(dev) = dev_verification_code() {
            if dev == code {
                is_dev_pass = true;
            }
        }
    }

    let repo = VerificationCodeRepo::new(state.db.clone());
    let purpose = VerificationCodePurpose::EmailVerification;

    let row = if is_dev_pass {
        // dev 路径：跳过 consume；直接复用 / 新建 user
        None
    } else {
        let hash = hash_code(code);
        repo.consume(&hash, purpose)
            .await
            .map_err(|e| ApiError(Error::Database(e.to_string())))?
    };

    if !is_dev_pass && row.is_none() {
        // 与上游 `Handler.VerifyCode`（auth.go:388，内部 L415 调
        // `IncrementVerificationCodeAttempts`）对齐：命中该邮箱待用验证码时累计
        // attempts（best-effort，不影响 401 响应）。`consume` 的 SQL 只匹配
        // `attempts < 5` 的行，累计到 5 后该行自然失效，起到暴力枚举防护作用。
        if let Ok(Some(latest)) = repo.latest_active_for(&email, purpose).await {
            if let Err(e) = repo.increment_attempts(latest.id).await {
                tracing::warn!(error = %e, "increment verification attempts failed");
            }
        }
        return Err(ApiError(Error::VerificationCodeInvalid(
            "code invalid, expired, or already used".into(),
        )));
    }

    // upsert user —— first 命中自动建 user，name 取 email 本地部分。
    // 这里直接走 SQL 是因为 UserRepo 由 sub-issue A 维护，本 sub-issue
    // 不能侵入其文件以免合并冲突。
    let user_row = sqlx::query_as::<_, (Uuid, String, String, DateTime<Utc>)>(
        r#"
        INSERT INTO "user" (name, email)
        VALUES ($1, $2)
        ON CONFLICT (email) DO UPDATE SET updated_at = now()
        RETURNING id, name, email, created_at
        "#,
    )
    .bind(email_local_part(&email))
    .bind(&email)
    .fetch_one(state.db.pool())
    .await
    .map_err(|e| ApiError(Error::Database(e.to_string())))?;

    let user_id = Id::from(user_row.0);
    let user_view = UserView {
        id: user_id.as_string(),
        name: user_row.1,
        email: user_row.2,
        created_at: user_row.3.to_rfc3339(),
    };

    // 颁发 session
    let session = Session::new(user_id, state.config.session_ttl_secs);
    let session_store = state.auth.store();
    session_store
        .put(session.clone())
        .await
        .map_err(|e| ApiError(Error::Internal(format!("session put: {e}"))))?;

    let body = VerifyCodeResponse {
        user: user_view,
        session_id: session.id.clone(),
        csrf_token: session.csrf_token.clone(),
    };
    Ok(session_response(
        &state,
        &session,
        StatusCode::OK,
        serde_json::to_value(body).unwrap(),
    ))
}

fn email_local_part(email: &str) -> String {
    email.split('@').next().unwrap_or(email).to_string()
}

// ============================================================
// POST /auth/logout
// ============================================================

#[derive(Debug, Serialize)]
pub struct LogoutResponse {
    pub message: &'static str,
}

async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Response> {
    // 从 cookie 拿 session_id；找不到也视为"已经登出"。
    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Some(sid) = parse_session_cookie(cookie_header, &state.config.session_cookie) {
        let _ = state.auth.store().delete(&sid).await;
    }

    // 清 cookie —— 设 Max-Age=0 让浏览器立刻丢掉。
    let mut cookie = CookieOptions::session_cookie(&state.config.session_cookie, "");
    cookie.max_age_secs = Some(0);
    cookie.secure = !state.config.dev_mode;
    cookie.http_only = true;
    cookie.same_site = SameSite::Lax;
    let cookie_str = cookie.render();

    let mut response = (
        StatusCode::OK,
        Json(LogoutResponse {
            message: "logged out",
        }),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie_str) {
        response.headers_mut().insert(SET_COOKIE, v);
    }
    Ok(response)
}

fn parse_session_cookie(header: &str, name: &str) -> Option<String> {
    for part in header.split(';') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            if k == name {
                return Some(v.to_string());
            }
        }
    }
    None
}

// ============================================================
// POST /api/auth/cli-token —— CLI 登录换取 PAT
// ============================================================

/// 当前用户解析：优先 session cookie，其次 M1 dev-mode 的 `X-Multica-User-Id`。
async fn current_user_id(state: &AppState, headers: &HeaderMap) -> Result<Id, ApiError> {
    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Some(sid) = parse_session_cookie(cookie_header, &state.config.session_cookie) {
        match state.auth.store().get(&sid).await {
            Ok(session) => return Ok(session.user_id),
            Err(mc_auth::session::SessionError::Expired) => {
                return Err(ApiError(Error::SessionExpired))
            }
            // 未知 session 不直接 401：可能是 dev 模式的 header 路径，继续往下看。
            Err(mc_auth::session::SessionError::NotFound) => {}
        }
    }
    let raw = headers
        .get(&crate::routes::auth_user::USER_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiError(Error::Unauthorized {
                message: "cli-token requires a session or X-Multica-User-Id header".into(),
            })
        })?;
    Id::parse(raw.trim()).map_err(|_| {
        ApiError(Error::Unauthorized {
            message: "invalid X-Multica-User-Id header (not a uuid)".into(),
        })
    })
}

#[derive(Debug, Serialize)]
pub struct CliTokenResponse {
    /// 明文 token（仅此一次返回）。
    pub token: String,
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub expires_at: String,
    pub user_id: String,
}

/// `POST /api/auth/cli-token` —— 用已登录会话换取一个 CLI 用的 token。
///
/// 上游（`handler/auth.go::IssueCliToken`）签发一个无状态 JWT；本仓 M1 还没有 JWT
/// 签发链，因此本实现返回一个 **30 天 TTL 的 PAT**（与 `/api/me/pats` 同一个
/// `PatStore`，`scopes = ["cli"]`）。这是有意的等价替换（可撤销 vs 不可撤销），
/// 见 LUM-1347 集成报告；M9 接入 JWT 后可平滑切换。
///
/// 与 LUM-1335（`feat/multica-rs-m1`）原实现的区别：原实现只拼一个
/// `mc_cli_<uuid>` 字符串且**不落库**，拿到的 token 无法被任何校验路径认可。
///
/// 签名与上游对齐：**无请求体**，200 + `{token}`。
async fn cli_token(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Response> {
    let user_id = current_user_id(&state, &headers).await?;
    let raw = generate_cli_pat_secret();
    let token = format!("{}{raw}", crate::routes::pats::PAT_PREFIX);
    let token_hash = sha256_hex(&raw);
    let token_last4 = raw
        .chars()
        .skip(raw.len().saturating_sub(4))
        .collect::<String>();
    let now = Utc::now();
    let expires_at = now + Duration::days(CLI_TOKEN_TTL_DAYS);
    let pat = mc_auth::pat::Pat {
        id: Id::new(),
        user_id,
        name: "cli".to_string(),
        token_hash,
        token_last4,
        expires_at,
        last_used_at: None,
        scopes: vec!["cli".to_string()],
        created_at: now,
    };
    state
        .pat
        .store()
        .put(pat.clone())
        .await
        .map_err(|e| ApiError(Error::Internal(format!("cli-token put: {e}"))))?;
    Ok((
        StatusCode::OK,
        Json(CliTokenResponse {
            token,
            id: pat.id.to_string(),
            name: pat.name.clone(),
            scopes: pat.scopes.clone(),
            expires_at: pat.expires_at.to_rfc3339(),
            user_id: user_id.to_string(),
        }),
    )
        .into_response())
}

/// CLI token 有效期（天）。
const CLI_TOKEN_TTL_DAYS: i64 = 30;

fn generate_cli_pat_secret() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    hex::encode(h.finalize())
}

// ============================================================
// POST /api/auth/refresh
// ============================================================

#[derive(Debug, Deserialize)]
pub struct RefreshRequest {
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RefreshResponse {
    pub session_id: String,
    pub csrf_token: String,
    pub expires_at: String,
}

async fn refresh_session(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RefreshRequest>,
) -> ApiResult<Response> {
    let sid = match req.session_id {
        Some(s) if !s.is_empty() => s,
        _ => {
            return Err(ApiError(Error::Validation {
                message: "session_id is required".into(),
                details: vec![],
            }))
        }
    };

    let session_store = state.auth.store();
    let mut session = session_store.get(&sid).await.map_err(|e| match e {
        mc_auth::session::SessionError::NotFound => ApiError(Error::Unauthorized {
            message: "session not found".into(),
        }),
        mc_auth::session::SessionError::Expired => ApiError(Error::SessionExpired),
    })?;

    // 续期：touch + 延长 TTL；新 csrf_token 不再更换（MUL-7436 把 csrf 绑
    // session.id 而不是 token 字符串本身）。
    session.last_seen_at = Utc::now();
    session.expires_at = Utc::now()
        + Duration::seconds(i64::try_from(state.config.session_ttl_secs).unwrap_or(i64::MAX));
    session_store
        .put(session.clone())
        .await
        .map_err(|e| ApiError(Error::Internal(format!("session put: {e}"))))?;
    session_store
        .touch(&session.id)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("session touch: {e}"))))?;

    let body = RefreshResponse {
        session_id: session.id.clone(),
        csrf_token: session.csrf_token.clone(),
        expires_at: session.expires_at.to_rfc3339(),
    };
    Ok(session_response(
        &state,
        &session,
        StatusCode::OK,
        serde_json::to_value(body).unwrap(),
    ))
}

#[allow(dead_code)]
fn _unused_uuid() -> Uuid {
    Uuid::new_v4()
}

// ============================================================
// 单元测试 —— 用 tower::ServiceExt::oneshot 走真实 axum Router。
// 重点：rate limit、consumed、expired 三种失败路径。
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AdapterRegistryStub, ConfigSnapshot, RuntimeHandles};
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use mc_auth::{SessionStoreContainer, VerificationStoreContainer};
    use mc_db::Db;
    use mc_realtime::{RealtimeHandle, WsState};
    use tower::ServiceExt;

    /// 构造测试用 AppState。DB 使用 `connect_lazy` —— 不会立即拨号；
    /// 需要真 DB 的测试必须在函数顶部检查 `DATABASE_URL` 并跳过。
    fn build_state(session_ttl_secs: u64, dev_mode: bool) -> Arc<AppState> {
        let db_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://127.0.0.1:1/none".to_string());
        let db = Db::connect_lazy(&db_url, 4, 1).expect("lazy db");

        let realtime = RealtimeHandle::start(8);
        let ws = Arc::new(WsState::new(realtime.clone(), "test"));
        let actors = mc_core::actor::ActorRegistry::new();
        let adapters = Arc::new(AdapterRegistryStub::default());
        let state = AppState {
            db,
            runtime: RuntimeHandles { actors, adapters },
            config: ConfigSnapshot {
                host: "127.0.0.1".into(),
                port: 0,
                session_cookie: "multica_session".into(),
                api_key_header: "X-Multica-Api-Key".into(),
                csrf_header: "X-Multica-Csrf".into(),
                dev_mode,
                session_ttl_secs,
                verification_code_ttl_secs: 600,
                send_code_per_email_per_min: 5,
                invitation_per_workspace_per_hour: None,
            },
            storage: mc_storage::Storage::new(),
            secrets: mc_secrets::Secrets::new(mc_auth::DefaultSecretsBackend::in_memory()),
            feature_flags: Arc::new(mc_feature_flags::FeatureFlagCatalog::new()),
            realtime,
            ws,
            auth: SessionStoreContainer::new(),
            pat: mc_auth::PatStoreContainer::new(),
            verification: VerificationStoreContainer::new(),
        };
        Arc::new(state)
    }

    async fn body_json(resp: Response) -> (StatusCode, serde_json::Value) {
        let status = resp.status();
        let body = to_bytes(resp.into_body(), 65536).await.unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
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
                (id, email, purpose, code_hash, expires_at, created_at)
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

    /// `/api/auth/cli-token`：dev header 路径 → 200 + 可用的 PAT（真正落 store）。
    #[tokio::test]
    async fn cli_token_issues_usable_pat() {
        let state = build_state(60, true);
        let app = router().with_state(state.clone());
        let user_id = Id::new();

        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/cli-token")
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
            .uri("/api/auth/cli-token")
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
            .uri("/api/auth/cli-token")
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
}
