//! Auth routes：send-code / verify-code / me / logout / refresh / cli-token。
//!
//! 协议与 multica upstream `handler/auth.go` 等价：
//! - `POST /api/auth/send-code` `{ email, purpose }` — 发送 6 位数字验证码
//!   到指定邮箱；目的包括 email_verification / password_reset / two_factor /
//!   workspace_invite。
//! - `POST /api/auth/verify-code` `{ email, code, purpose }` — 校验验证码；
//!   通过则建立 session 并 `Set-Cookie: multica_session=...` + 返回 user JSON。
//! - `GET  /api/auth/me` — 当前登录用户（mock header-based auth in M1）。
//! - `POST /api/auth/logout` — 销毁当前 session。
//! - `POST /api/auth/refresh` — 续期 session TTL。
//! - `POST /api/auth/cli-token` — 颁发一个 CLI / daemon 用的 token。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use mc_auth::{
    cookie::CookieOptions, Session, SessionStore, VerificationCodePurpose,
};
use mc_core::Id;
use mc_errors::{Error, Result as ErrorsResult};
use mc_repos::{NewUser, UserRow, UserRepo};

use crate::error::ApiError;
use crate::state::{AppState, ConfigSnapshot};

// ---------------------------------------------------------------------------
// Request / response bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SendCodeRequest {
    pub email: String,
    pub purpose: String,
    /// Optional source tag (e.g. "web_signup" / "cli_login"). Captured for
    /// analytics but not currently emitted anywhere.
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VerifyCodeRequest {
    pub email: String,
    pub code: String,
    pub purpose: String,
    /// Optional — when present, redirect to this workspace after login.
    #[serde(default)]
    pub workspace_id: Option<Id>,
}

#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: String,
    pub name: String,
    pub email: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VerifyCodeResponse {
    pub user: UserResponse,
    pub session_id: String,
    pub csrf_token: String,
    pub expires_at: String,
}

#[derive(Debug, Serialize)]
pub struct SendCodeResponse {
    pub sent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_code: Option<String>,
    pub cooldown_secs: u32,
}

#[derive(Debug, Serialize)]
pub struct LogoutResponse {
    pub ok: bool,
}

#[derive(Debug, Serialize)]
pub struct CliTokenResponse {
    pub token: String,
    pub user: UserResponse,
    pub expires_at: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const SESSION_COOKIE: &str = "multica_session";
const DEFAULT_TTL_SECS: u64 = 60 * 60 * 24 * 30;

fn parse_purpose(s: &str) -> ErrorsResult<VerificationCodePurpose> {
    match s {
        "email_verification" => Ok(VerificationCodePurpose::EmailVerification),
        "password_reset" => Ok(VerificationCodePurpose::PasswordReset),
        "two_factor" => Ok(VerificationCodePurpose::TwoFactor),
        "workspace_invite" => Ok(VerificationCodePurpose::WorkspaceInvite),
        other => Err(Error::Validation {
            message: format!("unknown purpose `{other}`"),
            details: vec![],
        }),
    }
}

fn map_repo_purpose(
    p: VerificationCodePurpose,
) -> mc_repos::verification::VerificationPurpose {
    use mc_repos::verification::VerificationPurpose as V;
    match p {
        VerificationCodePurpose::EmailVerification => V::EmailVerification,
        VerificationCodePurpose::PasswordReset => V::PasswordReset,
        VerificationCodePurpose::TwoFactor => V::TwoFactor,
        VerificationCodePurpose::WorkspaceInvite => V::WorkspaceInvite,
    }
}

/// SHA-256 the supplied code so the database only ever sees the hashed form.
fn hash_code(code: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(code.as_bytes());
    let out = h.finalize();
    hex::encode(out)
}

fn generate_six_digit_code() -> String {
    let mut rng = rand::thread_rng();
    let n: u32 = rng.next_u32() % 1_000_000;
    format!("{:06}", n)
}

async fn issue_session(
    state: &AppState,
    user_id: Id,
    workspace_id: Option<Id>,
) -> Session {
    let mut session = Session::new(user_id, DEFAULT_TTL_SECS);
    session.workspace_id = workspace_id;
    let store: Arc<dyn SessionStore> = state.auth.store();
    let _ = store.put(session.clone()).await;
    session
}

fn user_to_response(user: &UserRow) -> UserResponse {
    UserResponse {
        id: user.id.as_string(),
        name: user.name.clone(),
        email: user.email.clone(),
        avatar_url: user.avatar_url.clone(),
    }
}

fn session_cookie(cfg: &ConfigSnapshot, session: &Session) -> String {
    CookieOptions::session_cookie(&cfg.session_cookie, &session.id).render()
}

fn dev_mode(cfg: &ConfigSnapshot) -> bool {
    cfg.host.starts_with("127.") || cfg.host.starts_with("localhost")
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /api/auth/send-code`
pub async fn send_code(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SendCodeRequest>,
) -> Result<Json<SendCodeResponse>, ApiError> {
    let purpose = parse_purpose(&body.purpose).map_err(ApiError::from)?;
    if body.email.is_empty() || !body.email.contains('@') {
        return Err(ApiError(Error::Validation {
            message: "valid email required".into(),
            details: vec![],
        }));
    }
    let code = generate_six_digit_code();
    let code_hash = hash_code(&code);

    let new_code = mc_repos::NewVerificationCode {
        id: None,
        user_id: None,
        email: Some(body.email.clone()),
        purpose: map_repo_purpose(purpose),
        code_hash,
        expires_at: Utc::now() + chrono::Duration::minutes(10),
    };
    state
        .repos
        .verification_codes
        .create(new_code)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("create code: {e}"))))?;

    // Email delivery is not part of M1 (M9+). Log instead.
    tracing::info!(
        email = %body.email,
        purpose = %purpose.as_str(),
        "verification code generated"
    );

    let dev_code = if dev_mode(&state.config) {
        Some(code)
    } else {
        None
    };

    Ok(Json(SendCodeResponse {
        sent: true,
        dev_code,
        cooldown_secs: 60,
    }))
}

/// `POST /api/auth/verify-code`
pub async fn verify_code(
    State(state): State<Arc<AppState>>,
    Json(body): Json<VerifyCodeRequest>,
) -> Result<Response, ApiError> {
    let purpose = parse_purpose(&body.purpose).map_err(ApiError::from)?;
    let purpose_repo = map_repo_purpose(purpose);
    let row = state
        .repos
        .verification_codes
        .find_active(&body.email, purpose_repo)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("lookup code: {e}"))))?
        .ok_or_else(|| ApiError(Error::VerificationCodeInvalid("expired or not found".into())))?;
    state
        .repos
        .verification_codes
        .increment_attempts(row.id)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("increment attempts: {e}"))))?;
    let expected_hash = hash_code(&body.code);
    if !constant_time_eq(expected_hash.as_bytes(), row.code_hash.as_bytes()) {
        return Err(ApiError(Error::VerificationCodeInvalid(
            "code mismatch".into(),
        )));
    }
    state
        .repos
        .verification_codes
        .consume(row.id)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("consume: {e}"))))?;

    // Find or create user.
    let user = match state
        .repos
        .users
        .find_by_email(&body.email)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("lookup user: {e}"))))?
    {
        Some(u) => u,
        None => {
            let new_user = NewUser::new(derive_name_from_email(&body.email), &body.email);
            state
                .repos
                .users
                .create(new_user)
                .await
                .map_err(|e| ApiError(Error::Internal(format!("create user: {e}"))))?
        }
    };

    let session = issue_session(&state, user.id, body.workspace_id).await;

    let payload = VerifyCodeResponse {
        user: user_to_response(&user),
        session_id: session.id.clone(),
        csrf_token: session.csrf_token.clone(),
        expires_at: session.expires_at.to_rfc3339(),
    };

    let set_cookie = session_cookie(&state.config, &session);
    let mut headers = HeaderMap::new();
    headers.insert("set-cookie", set_cookie.parse().unwrap());
    let json = Json(payload);
    let mut resp = json.into_response();
    resp.headers_mut().extend(headers);
    Ok(resp)
}

/// `GET /api/auth/me` — return the current authenticated user.
/// M1: session lookup via `X-Multica-Session-Id` header (cookie parsing is M2).
pub async fn me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<UserResponse>, ApiError> {
    let session_id = headers
        .get("x-multica-session-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError(Error::Unauthorized {
            message: "no session".into(),
        }))?;
    let store: Arc<dyn SessionStore> = state.auth.store();
    let session = store
        .get(session_id)
        .await
        .map_err(|e| ApiError(Error::Unauthorized { message: e.to_string() }))?;
    let user = state
        .repos
        .users
        .get(session.user_id)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("get user: {e}"))))?;
    Ok(Json(user_to_response(&user)))
}

/// `POST /api/auth/logout`
pub async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Json<LogoutResponse> {
    if let Some(value) = headers.get("x-multica-session-id").and_then(|v| v.to_str().ok()) {
        let store: Arc<dyn SessionStore> = state.auth.store();
        let _ = store.delete(value).await;
    }
    Json(LogoutResponse { ok: true })
}

/// `POST /api/auth/cli-token` — mint a long-lived bearer token for daemon use.
pub async fn cli_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<CliTokenResponse>, ApiError> {
    let session_id = headers
        .get("x-multica-session-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError(Error::Unauthorized {
            message: "no session".into(),
        }))?;
    let store: Arc<dyn SessionStore> = state.auth.store();
    let session = store
        .get(session_id)
        .await
        .map_err(|e| ApiError(Error::Unauthorized { message: e.to_string() }))?;
    let user = state
        .repos
        .users
        .get(session.user_id)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("get user: {e}"))))?;
    let token = format!("mc_cli_{}", Uuid::new_v4().simple());
    Ok(Json(CliTokenResponse {
        token,
        user: user_to_response(&user),
        expires_at: (Utc::now() + chrono::Duration::days(7)).to_rfc3339(),
    }))
}

/// Fallback placeholder while M2 routes land.
pub async fn not_implemented() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": { "code": "not_implemented", "message": "M2+ handler" }
        })),
    )
        .into_response()
}

fn derive_name_from_email(email: &str) -> String {
    email
        .split('@')
        .next()
        .unwrap_or("user")
        .chars()
        .take(64)
        .collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_purpose_round_trip_known_values() {
        for (s, expected) in [
            ("email_verification", VerificationCodePurpose::EmailVerification),
            ("password_reset", VerificationCodePurpose::PasswordReset),
            ("two_factor", VerificationCodePurpose::TwoFactor),
            ("workspace_invite", VerificationCodePurpose::WorkspaceInvite),
        ] {
            assert_eq!(parse_purpose(s).unwrap(), expected);
        }
    }

    #[test]
    fn parse_purpose_rejects_unknown() {
        let err = parse_purpose("zzz").unwrap_err();
        assert!(matches!(err, Error::Validation { .. }));
    }

    #[test]
    fn six_digit_code_has_correct_width() {
        let code = generate_six_digit_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn hash_code_is_deterministic() {
        assert_eq!(hash_code("123456"), hash_code("123456"));
        assert_ne!(hash_code("123456"), hash_code("654321"));
    }

    #[test]
    fn constant_time_eq_handles_length_mismatch() {
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
    }

    #[test]
    fn derive_name_uses_local_part_of_email() {
        assert_eq!(derive_name_from_email("alice@example.com"), "alice");
        assert_eq!(derive_name_from_email(""), "user");
    }

    #[test]
    fn repo_purpose_mapping_is_bijective() {
        for p in [
            VerificationCodePurpose::EmailVerification,
            VerificationCodePurpose::PasswordReset,
            VerificationCodePurpose::TwoFactor,
            VerificationCodePurpose::WorkspaceInvite,
        ] {
            let mapped = map_repo_purpose(p);
            let back = match mapped {
                mc_repos::verification::VerificationPurpose::EmailVerification => {
                    VerificationCodePurpose::EmailVerification
                }
                mc_repos::verification::VerificationPurpose::PasswordReset => {
                    VerificationCodePurpose::PasswordReset
                }
                mc_repos::verification::VerificationPurpose::TwoFactor => {
                    VerificationCodePurpose::TwoFactor
                }
                mc_repos::verification::VerificationPurpose::WorkspaceInvite => {
                    VerificationCodePurpose::WorkspaceInvite
                }
            };
            assert_eq!(back, p);
        }
    }
}
