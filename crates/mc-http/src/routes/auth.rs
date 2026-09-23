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
//! | POST | `/auth/google` | `google_login` | `Handler.GoogleLogin` (auth.go:546) |
//!
//! 路径遵循 upstream：浏览器登录页用 `/auth/send-code`、`/auth/verify-code`，
//! 而已经 cookie 化的会话通过 `/api/auth/refresh` 续期。
//!
//! 邮件发送在本 sub-issue 用 `tracing::info!` 占位，不接 SMTP ——
//! M9（mailer）才会接 Resend / SES。`MULTICA_DEV_VERIFICATION_CODE` 在
//! 非 production 模式下作为万能验证码（参考 upstream `isDevVerificationCode`）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, SET_COOKIE};
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
use mc_repos::user::{NewUser, UserRepo};
use mc_repos::verification_code::{NewVerificationCode, VerificationCodeRepo};
use mc_repos::Repository;

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, GoogleOAuthConfig};

/// Auth 路由切片。
///
/// 返回的 Router 类型是 `Router<Arc<AppState>>`（未注入 state），
/// 在 `mount.rs::router()` 合并后由 `mc-http::router()` 顶层 `.with_state()` 注入。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/send-code", post(send_code))
        .route("/auth/verify-code", post(verify_code))
        .route("/auth/logout", post(logout))
        .route("/auth/google", post(google_login))
        .route("/api/auth/refresh", post(refresh_session))
        // M1-D（LUM-1347）从 LUM-1335（`feat/multica-rs-m1`）cherry-pick 的增量：
        // CLI 登录用的一次性 PAT（浏览器会话 → token）。
        //
        // 路径 M1-E（LUM-1362）修正：上游是 `POST /api/cli-token`
        // （`router.go:1628`，**没有** `/auth` 这一层）；M1-B 曾误注册为
        // `/api/auth/cli-token`。见 docs/17-M1-CONTRACT-GAPS.md 缺口 #7。
        .route("/api/cli-token", post(cli_token))
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
// POST /api/cli-token（上游 router.go:1628）—— CLI 登录换取 PAT
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

/// `POST /api/cli-token` —— 用已登录会话换取一个 CLI 用的 token。
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
// POST /auth/google
// ============================================================

/// 上游 `writeErrorCode`/`writeFeatureDisabled` 的等价物：
///
/// - 状态码由调用方显式给出（本仓 `mc_errors::Error` 的固定映射里
///   `Upstream` 是 500，而上游 `GoogleLogin` 的 502 必须逐条对齐）；
/// - 错误体沿用本仓 M1 的嵌套 envelope（`mc_errors::ErrorBody`，与 `ApiError`
///   的输出逐字段同形）——上游是扁平 `{"error": msg, "code": code}`，
///   登记在 `docs/29-W1-GOOGLE.md`；
/// - `code` 用上游的字符串常量（`mc_errors::Error::code()` 只能返回固定映射）。
fn google_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(mc_errors::ErrorBody {
            error: mc_errors::ErrorResponse::new(code, message),
        }),
    )
        .into_response()
}

/// 上游 502 系列（换 token / 拉 userinfo 的传输层与协议层失败）。
fn google_upstream_error(message: &str) -> Response {
    google_error(StatusCode::BAD_GATEWAY, "upstream_error", message)
}

/// 上游 500 系列（userinfo 请求构造失败、user 落库失败、签发 session 失败）。
fn google_internal_error(code: &str, message: &str) -> Response {
    google_error(StatusCode::INTERNAL_SERVER_ERROR, code, message)
}

// 上游 `auth.go:43-47` 的 code 常量，字符串逐字保留（`pub` 供 M9 接上配置面后复用）。
/// 上游 `googleLoginCodeAccountDisabled`：当前不可达（见上方偏离清单）。
pub const GOOGLE_CODE_ACCOUNT_DISABLED: &str = "account_disabled";
/// 上游 `googleLoginCodeSignupProhibited`：当前不可达（本仓无 `ALLOW_SIGNUP`）。
pub const GOOGLE_CODE_SIGNUP_PROHIBITED: &str = "signup_prohibited";
/// 上游 `googleLoginCodeEmailNotAllowed`：当前不可达（本仓无邮箱白名单）。
pub const GOOGLE_CODE_EMAIL_NOT_ALLOWED: &str = "email_not_allowed";
/// 上游 `googleLoginCodeAccountWithoutEmail`。
pub const GOOGLE_CODE_ACCOUNT_WITHOUT_EMAIL: &str = "google_account_no_email";
/// 上游 `googleLoginCodeInvalidOAuthCode`。
pub const GOOGLE_CODE_INVALID_OAUTH_CODE: &str = "oauth_code_invalid";
/// `writeFeatureDisabled` 的 code。
pub const GOOGLE_CODE_NOT_CONFIGURED: &str = "google_login_not_configured";

#[derive(Debug, Deserialize)]
pub struct GoogleLoginRequest {
    /// 上游是 `Code string` —— 字段缺失等同于空串（→ 400 `code is required`）。
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default, rename = "id_token")]
    #[allow(dead_code)]
    id_token: Option<String>,
    #[serde(default, rename = "token_type")]
    #[allow(dead_code)]
    token_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleTokenError {
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleUserInfo {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    picture: Option<String>,
}

/// `LoginResponse`（上游 `handler/auth.go:112`）。
#[derive(Debug, Serialize)]
pub struct GoogleLoginResponse {
    /// 上游是 HS256 JWT。本仓 M1 没有 JWT 层，这里是不透明 session id
    /// （同时写进 `multica_session` cookie）—— 见 `docs/29-W1-GOOGLE.md`。
    pub token: String,
    pub user: UserView,
}

/// 上游 `Handler.GoogleLogin`（`handler/auth.go:546-728`）的逐条移植。
///
/// 请求：`POST /auth/google`（**没有** `/api` 前缀），body
/// `{"code": "<google authorization code>", "redirect_uri": "<可选>"}`。
///
/// 11 步流程与状态码/错误码对齐上游（行号见 `docs/29-W1-GOOGLE.md` 的表）：
/// 1. body 解析失败 → 400 `invalid request body`
/// 2. `code` 为空 → 400 `code is required`
/// 3. 未配置 `GOOGLE_CLIENT_ID`/`GOOGLE_CLIENT_SECRET` → 403 `google_login_not_configured`
///    （上游 `writeFeatureDisabled` 故意用 403 而不是 503，避免重试与告警噪音）
/// 4. 换 token `POST {MC_GOOGLE_TOKEN_URL}`（表单）→ 传输失败 502
/// 5. 非 200：`400 + {"error":"invalid_grant"}` → 400 `oauth_code_invalid`；其余 → 502
/// 6. 响应解析失败 / 空 `access_token` → 502
/// 7. `GET {MC_GOOGLE_USERINFO_URL}`（Bearer）→ 请求构造失败 500；传输失败 / 非 200 → 502
/// 8. 空 email → 400 `google_account_no_email`
/// 9. `findOrCreateUser(email)`（按 email 查，缺失则用 `@` 前缀建号；
///    禁用/白名单三条 403 见下）→ 落库失败 500
/// 10. 回填 name（仅当 name == email 前缀）/ avatar（仅当为空）—— 失败只记日志
/// 11. 签发 session + `Set-Cookie` → `{"token", "user"}`；签发失败 500
///
/// 与上游的登记偏离（完整清单见 `docs/29-W1-GOOGLE.md`）：
/// - 错误体是嵌套 envelope（本仓 M1 约定），`code` 字符串一致；
/// - `token` 是 session id 而非 JWT；
/// - `user` 是本仓 `UserView`（字段少于上游 `UserResponse`）；
/// - `account_disabled` / `signup_prohibited` / `email_not_allowed` 三条 403 在本仓
///   **不可达**：M1 没有 signup 白名单与禁用邮箱的配置面（上游是
///   `ALLOW_SIGNUP` / `ALLOWED_EMAILS` / `ALLOWED_EMAIL_DOMAINS` +
///   `auth.IsTemporarilyDisabledUserEmail`）；
/// - 不签发上游 `SetAuthCookies` 附带的 CF region cookie；
/// - 上游这条路由有 Redis 支持的 per-IP 限流（`RATE_LIMIT_AUTH`，默认 5/min），
///   本仓 M1 无 per-IP 限流设施。
// 逐条对齐上游 11 步流程与错误码；拆函数会把「步骤 ↔ 状态码」的对应关系割裂。
#[allow(clippy::too_many_lines)]
async fn google_login(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    // ---- 1. 请求体 ----
    // 用 `Bytes` 而不是 `Json<T>`：axum 的 `Json` rejection 对「字段类型错误」是
    // 422，而上游 `json.Decode` 失败一律 400 "invalid request body"。
    let req: GoogleLoginRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(err) => {
            tracing::debug!(error = %err, "google login: invalid request body");
            return google_error(
                StatusCode::BAD_REQUEST,
                "validation_error",
                "invalid request body",
            );
        }
    };

    // ---- 2. code 必填（上游不 trim，此处保持一致）----
    if req.code.is_empty() {
        return google_error(
            StatusCode::BAD_REQUEST,
            "validation_error",
            "code is required",
        );
    }

    // ---- 3. 功能开关 ----
    let cfg: GoogleOAuthConfig = state.google_oauth.clone();
    let Some((client_id, client_secret)) = cfg.client_id.clone().zip(cfg.client_secret.clone())
    else {
        return google_error(
            StatusCode::FORBIDDEN,
            GOOGLE_CODE_NOT_CONFIGURED,
            "Google login is not configured",
        );
    };
    let Some(http) = cfg.http.clone() else {
        tracing::error!("google oauth http client unavailable");
        return google_upstream_error("failed to exchange code with Google");
    };

    // ---- 4. 用 authorization code 换 token ----
    let redirect_uri = cfg.redirect_uri_for(req.redirect_uri.as_deref());
    let form = [
        ("code", req.code.as_str()),
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("grant_type", "authorization_code"),
    ];
    let token_resp = match http.post(&cfg.token_url).form(&form).send().await {
        Ok(resp) => resp,
        Err(err) => {
            tracing::error!(error = %err, url = %cfg.token_url, "google oauth token exchange failed");
            return google_upstream_error("failed to exchange code with Google");
        }
    };
    let token_status = token_resp.status();
    let token_body = match token_resp.text().await {
        Ok(body) => body,
        Err(err) => {
            tracing::error!(error = %err, "google oauth token response could not be read");
            return google_upstream_error("failed to read Google token response");
        }
    };

    // ---- 5. 非 200 ----
    // 只有「400 + error == invalid_grant」说明授权码被拒；配置/上游/畸形响应都是
    // 服务端失败（上游注释原话：Only a valid invalid_grant response identifies a
    // rejected authorization code）。
    if token_status != StatusCode::OK {
        let provider_error = serde_json::from_str::<GoogleTokenError>(&token_body)
            .ok()
            .and_then(|err| err.error)
            .unwrap_or_default();
        tracing::error!(
            status = token_status.as_u16(),
            body = %token_body,
            "google oauth token exchange returned error"
        );
        if token_status == StatusCode::BAD_REQUEST && provider_error == "invalid_grant" {
            return google_error(
                StatusCode::BAD_REQUEST,
                GOOGLE_CODE_INVALID_OAUTH_CODE,
                "failed to exchange code with Google",
            );
        }
        return google_upstream_error("failed to exchange code with Google");
    }

    // ---- 6. token 响应体 ----
    let token: GoogleTokenResponse = match serde_json::from_str(&token_body) {
        Ok(token) => token,
        Err(err) => {
            tracing::error!(error = %err, body = %token_body, "google oauth token response could not be parsed");
            return google_upstream_error("failed to parse Google token response");
        }
    };
    let access_token = token.access_token.unwrap_or_default();
    let access_token = access_token.trim();
    if access_token.is_empty() {
        tracing::error!("google oauth token response has no access token");
        return google_upstream_error("invalid Google token response");
    }

    // ---- 7. 拉 userinfo ----
    // 请求构造失败（URL/header 非法）→ 500，与上游 `http.NewRequestWithContext`
    // 失败一致；`build()` 是唯一能在 `send()` 之前区分它的点。
    let userinfo_request = match http
        .get(&cfg.userinfo_url)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .build()
    {
        Ok(request) => request,
        Err(err) => {
            tracing::error!(error = %err, url = %cfg.userinfo_url, "failed to create userinfo request");
            return google_internal_error("internal_error", "internal error");
        }
    };
    let userinfo_resp = match http.execute(userinfo_request).await {
        Ok(resp) => resp,
        Err(err) => {
            tracing::error!(error = %err, "google userinfo fetch failed");
            return google_upstream_error("failed to fetch user info from Google");
        }
    };
    if userinfo_resp.status() != StatusCode::OK {
        let status = userinfo_resp.status();
        let body = userinfo_resp.text().await.unwrap_or_default();
        tracing::error!(status = status.as_u16(), body = %body, "google userinfo returned error");
        return google_upstream_error("failed to fetch user info from Google");
    }
    let userinfo_body = match userinfo_resp.text().await {
        Ok(body) => body,
        Err(err) => {
            tracing::error!(error = %err, "google userinfo body could not be read");
            return google_upstream_error("failed to parse Google user info");
        }
    };
    // 上游把 body 解到 `*googleUserInfo`：字面量 `null` 解出 nil 指针 → 502
    // "invalid Google user info"。`Option<GoogleUserInfo>` 保留了这条区分。
    let userinfo: Option<GoogleUserInfo> = match serde_json::from_str(&userinfo_body) {
        Ok(userinfo) => userinfo,
        Err(err) => {
            tracing::error!(error = %err, body = %userinfo_body, "google userinfo could not be parsed");
            return google_upstream_error("failed to parse Google user info");
        }
    };
    let Some(userinfo) = userinfo else {
        return google_upstream_error("invalid Google user info");
    };

    // ---- 8. email 必填（小写 + trim）----
    let email = userinfo
        .email
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if email.is_empty() {
        return google_error(
            StatusCode::BAD_REQUEST,
            GOOGLE_CODE_ACCOUNT_WITHOUT_EMAIL,
            "Google did not provide an email address for this sign-in",
        );
    }

    // ---- 9. findOrCreateUser ----
    // 上游在 findOrCreateUser 之前还有一道 `IsTemporarilyDisabledUserEmail`（403
    // `account_disabled`）；本仓 M1 没有这个硬编码列表，故该分支不可达。
    // `account_disabled` / `signup_prohibited` / `email_not_allowed` 三个 code
    // 保留为 `pub const`，M9 接上配置面（`ALLOW_SIGNUP` / `ALLOWED_EMAILS`）后再启用。
    let users = UserRepo::new(state.db.clone());
    let existing = match users.get_by_email(&email).await {
        Ok(user) => user,
        Err(err) => {
            tracing::error!(error = %err, %email, "google login: user lookup failed");
            return google_internal_error("database_error", "failed to create user");
        }
    };
    let mut is_new = false;
    let mut user = if let Some(user) = existing {
        user
    } else {
        is_new = true;
        match users
            .create(NewUser {
                name: email_local_part(&email),
                email: email.clone(),
                avatar_url: None,
            })
            .await
        {
            Ok(user) => user,
            Err(err) => {
                tracing::error!(error = %err, %email, "google login: user create failed");
                return google_internal_error("database_error", "failed to create user");
            }
        }
    };

    // ---- 10. 回填 Google profile ----
    // 上游只在「用户没改过」时回填：name 仍等于 email 本地部分、avatar 还是空。
    // 回填失败只记日志，继续用旧 user（上游 `if err == nil { user = updated }`）。
    let profile_name = userinfo.name.unwrap_or_default();
    let picture = userinfo.picture.unwrap_or_default();
    let needs_name = !profile_name.is_empty() && user.name == email_local_part(&email);
    let has_avatar = user
        .avatar_url
        .as_deref()
        .is_some_and(|url| !url.is_empty());
    let needs_avatar = !picture.is_empty() && !has_avatar;
    if needs_name || needs_avatar {
        // `UserRepo::update` 没有 avatar 字段（M1-A 的签名），改用按 email 的
        // 幂等 upsert：EXCLUDED 就是我们算好的最终值，语义等价且不动 mc-repos。
        let patch = NewUser {
            name: if needs_name {
                profile_name
            } else {
                user.name.clone()
            },
            email: email.clone(),
            avatar_url: if needs_avatar {
                Some(picture)
            } else {
                user.avatar_url.clone()
            },
        };
        match users.upsert_by_email(patch).await {
            Ok(updated) => user = updated,
            Err(err) => tracing::warn!(
                error = %err,
                %email,
                "google login: profile backfill failed, keeping stored user"
            ),
        }
    }

    // ---- 11. 签发 session + cookie ----
    let session = Session::new(user.id, state.config.session_ttl_secs);
    if let Err(err) = state.auth.store().put(session.clone()).await {
        tracing::error!(error = %err, "google login: session put failed");
        return google_internal_error("internal_error", "failed to generate token");
    }

    let body = GoogleLoginResponse {
        token: session.id.clone(),
        user: UserView {
            id: user.id.as_string(),
            name: user.name.clone(),
            email: user.email.clone(),
            created_at: user.created_at.as_iso(),
        },
    };

    tracing::info!(
        user_id = %user.id.as_string(),
        email = %email,
        is_new,
        "user logged in via google"
    );
    session_response(
        &state,
        &session,
        StatusCode::OK,
        serde_json::to_value(body).unwrap(),
    )
}

// ============================================================
// 单元测试 —— 用 tower::ServiceExt::oneshot 走真实 axum Router。
// 重点：rate limit、consumed、expired 三种失败路径。
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AdapterRegistry, ConfigSnapshot, RuntimeHandles};
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
            // M3 anchor scaffold（LUM-1406）：本处只显式给两个「随测试参数变化」的字段
            // （`port: 0` = 系统分配；`dev_mode` / `session_ttl_secs` 由调用方传入），
            // 其余与 `ConfigSnapshot::default()` 逐字相同 —— 改成 `..Default::default()`
            // 是纯语法收敛，行为不变（host/cookie 头/两个 TTL/限速均等于默认值）。
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
            daemon_requests: std::sync::Arc::new(mc_http::daemon_requests::RequestStore::new()),
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

    // ============================================================
    // POST /auth/google（W1-Google / LUM-1399）
    // ============================================================
    //
    // stub 用本机临时 axum listener（`127.0.0.1:0`）：不引入 wiremock 这类
    // 新依赖，也不需要常驻服务。stub 的行为由**请求里的 code** 决定，
    // 所以一个 stub 服务能跑完全部用例，测试之间没有环境变量串扰
    // （base URL 是构 state 时注入的，不读进程 env）。

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

    /// 起一个本机 stub Google：返回 `(token_url, userinfo_url)`。
    async fn spawn_google_stub() -> (String, String) {
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
}
