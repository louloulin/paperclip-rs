//! `/api/auth*` 路由：sign-in / sign-out / issue-key / get-session。

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{Duration, Utc};
use pc_auth::ApiKeyIssuer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use pc_core::Timestamp;
use pc_repos::auth::{AuthRepo, NewSession, NewUser};
use pc_repos::company_member::CompanyMemberRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        // ===== Better-Auth 风格 wire 端点 (Round 29) =====
        .route("/api/auth/get-session", get(get_session))
        // ---- Round 41: legacy short aliases ----
        .route("/api/get-session", get(get_session_short))
        .route("/api/profile", get(get_profile_short))
        .route("/api/auth/sign-in/email", post(sign_in_email))
        .route("/api/auth/sign-up/email", post(sign_up_email))
        .route("/api/auth/sign-out", post(sign_out))
        .route("/api/auth/refresh", post(refresh_session))
        .route("/api/auth/profile", get(get_profile).patch(patch_profile))
        // ===== Legacy 简化端点（保留向后兼容） =====
        .route("/api/auth/sign-in", post(sign_in))
        .route("/api/auth/issue-key", post(issue_key))
        .route("/api/auth/revoke-key", post(revoke_key))
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct SessionEnvelope {
    session: SessionInner,
    user: SessionUser,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct SessionInner {
    id: String,
    user_id: String,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct SessionUser {
    id: String,
    email: String,
    name: String,
    image: Option<String>,
    email_verified: bool,
}

async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<SessionEnvelope>> {
    let token = extract_token(&headers)
        .ok_or_else(|| ApiError::Unauthorized("missing credentials".into()))?;
    // Try API key first, then session
    let (user_id, method) = if let Some((_, uid)) = pc_auth::resolve_api_key(&state.db, &token)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        (uid, "api_key")
    } else if let Some((uid, _)) = pc_auth::resolve_session(&state.db, &token)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        (uid, "session")
    } else {
        return Err(ApiError::Unauthorized("invalid credentials".into()));
    };
    let user = AuthRepo::new(&state.db)
        .find_by_id(&user_id)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("user not found".into()))?;
    Ok(Json(SessionEnvelope {
        session: SessionInner {
            id: format!("paperclip:{}:{}", method, user.id),
            user_id: user.id.clone(),
        },
        user: SessionUser {
            id: user.id,
            email: user.email,
            name: user.name,
            image: user.image,
            email_verified: user.email_verified,
        },
    }))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SignInBody {
    email: String,
    #[serde(default)]
    name: Option<String>,
    /// 简化版：直接传入 `user_id`（不实现密码哈希）
    #[serde(default)]
    user_id: Option<String>,
    /// Optional plaintext password. When provided, the server verifies
    /// against the stored argon2id hash on the user's `account` row.
    #[serde(default)]
    password: Option<String>,
    /// When true, rotate the session token even if a valid session already
    /// exists for the user. Mirrors Node-side `rotateSessionOnSignIn`.
    #[serde(default)]
    rotate_session: bool,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct SignInResponse {
    user_id: String,
    session_token: String,
    expires_at: chrono::DateTime<Utc>,
}

async fn sign_in(
    State(state): State<AppState>,
    Json(body): Json<SignInBody>,
) -> ApiResult<impl IntoResponse> {
    // 简化版 sign-in：通过 email 查找 user，没有则用 user_id 创建
    fn random_id(n: usize) -> String {
        use sha2::{Digest, Sha256};
        // 用 (nanos + uuid) 作熵源，做 4 轮 SHA-256 输出 base36
        let s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .wrapping_add(uuid::Uuid::new_v4().as_u128());
        let mut out = String::with_capacity(n);
        let mut h = Sha256::digest(s.to_le_bytes()).to_vec();
        while out.len() < n {
            for &b in &h {
                if out.len() >= n {
                    break;
                }
                let c = u32::from(b % 36);
                out.push(std::char::from_digit(c, 36).unwrap());
            }
            h = Sha256::digest(&h).to_vec();
        }
        out
    }
    let user_id = if let Some(uid) = body.user_id {
        // 直接指定 user_id
        AuthRepo::new(&state.db)
            .ensure_user(
                &uid,
                body.name.as_deref().unwrap_or("user"),
                &body.email,
            )
            .await?;
        uid
    } else if !body.email.is_empty() {
        // 通过 email 查找
        let id = AuthRepo::new(&state.db)
            .find_user_id_by_email(&body.email)
            .await?;
        if let Some(id) = id {
            id
        } else {
            let new_id = format!("u_{}", random_id(21));
            AuthRepo::new(&state.db)
                .ensure_user(
                    &new_id,
                    body.name.as_deref().unwrap_or(&body.email),
                    &body.email,
                )
                .await?;
            new_id
        }
    } else {
        return Err(ApiError::BadRequest("email or user_id required".into()));
    };

    // Password verification: when a password is provided, look up the
    // `account` row for this user and verify the argon2id hash. If no
    // password is provided, fall back to the legacy email-only flow.
    if let Some(plaintext) = body.password.as_deref() {
        let acct = AuthRepo::new(&state.db)
            .find_account_for_user(&user_id, "credential")
            .await?;
        let stored = acct.and_then(|a| a.password);
        match stored {
            Some(hash) if !hash.is_empty() => {
                if !pc_auth::verify_password(plaintext, &hash) {
                    return Err(ApiError::Unauthorized("invalid credentials".into()));
                }
            }
            _ => {
                return Err(ApiError::Unauthorized("password not set for user".into()));
            }
        }
    }

    // Session rotation: drop any existing active sessions for this user
    // when `rotate_session` is true (or always when password auth was used).
    let should_rotate = body.rotate_session || body.password.is_some();
    if should_rotate {
        AuthRepo::new(&state.db)
            .revoke_all_sessions_for_user(&user_id)
            .await?;
    }

    // 创建 session（7 天有效）
    let session_id = format!("s_{}", random_id(32));
    let session_token = format!("tok_{}", random_id(48));
    let expires_at = Utc::now() + Duration::days(7);
    AuthRepo::new(&state.db)
        .upsert_session(&NewSession {
            id: session_id,
            token: session_token.clone(),
            user_id: user_id.clone(),
            expires_at: Timestamp::from_dt(expires_at),
            ip_address: None,
            user_agent: None,
        })
        .await?;

    state.realtime.publish(
        pc_realtime::LiveEvent::new("auth.signed_in", "user", Uuid::nil()).with_actor(&user_id),
    );

    Ok((
        StatusCode::OK,
        Json(SignInResponse {
            user_id,
            session_token,
            expires_at,
        }),
    ))
}

async fn sign_out(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let mut deleted = 0;
    if let Some(token) = extract_token(&headers) {
        deleted = AuthRepo::new(&state.db)
            .revoke_session_by_token(&token)
            .await? as u64;
    }
    state.realtime.publish(
        pc_realtime::LiveEvent::new("auth.signed_out", "user", Uuid::nil()).with_actor("anonymous"),
    );
    Ok(Json(json!({"success": true, "deletedSessions": deleted})))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct IssueKeyBody {
    name: String,
}

async fn issue_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<IssueKeyBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let user_id = require_user(&state, &headers).await?;
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let (id, raw) = ApiKeyIssuer::create(&state.db, &user_id, &body.name)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "id": id, "name": body.name, "raw_token": raw,
        "note": "Store this token securely. It will not be shown again."
    })))
}

async fn revoke_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<StatusCode> {
    let user_id = require_user(&state, &headers).await?;
    let key_id: Uuid = body
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| ApiError::BadRequest("id (uuid) required".into()))?;
    let revoked = AuthRepo::new(&state.db)
        .revoke_api_key(key_id, &user_id)
        .await?;
    if !revoked {
        return Err(ApiError::NotFound(format!("api key {key_id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ============ 辅助 ============

async fn require_user(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    let token = extract_token(headers)
        .ok_or_else(|| ApiError::Unauthorized("missing credentials".into()))?;
    // 先试 api key
    if let Some((_, user_id)) = pc_auth::resolve_api_key(&state.db, &token)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        return Ok(user_id);
    }
    // 再试 session
    if let Some((user_id, _)) = pc_auth::resolve_session(&state.db, &token)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        return Ok(user_id);
    }
    Err(ApiError::Unauthorized("invalid credentials".into()))
}

fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(t) = auth.strip_prefix("Bearer ") {
            return Some(t.to_string());
        }
    }
    if let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for kv in cookie.split(';') {
            if let Some(v) = kv.trim().strip_prefix("paperclip_session=") {
                return Some(v.to_string());
            }
        }
    }
    None
}


#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct ProfilePayload {
    id: String,
    email: Option<String>,
    name: Option<String>,
    image: Option<String>,
}

async fn get_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<ProfilePayload>> {
    let user_id = require_user(&state, &headers).await?;
    let user = AuthRepo::new(&state.db)
        .find_by_id(&user_id)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("user not found".into()))?;
    Ok(Json(ProfilePayload {
        id: user.id,
        email: Some(user.email),
        name: Some(user.name),
        image: user.image,
    }))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct PatchProfileBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    image: Option<String>,
}

async fn patch_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PatchProfileBody>,
) -> ApiResult<Json<ProfilePayload>> {
    let user_id = require_user(&state, &headers).await?;
    // 仅当至少一个字段提供才更新
    if body.name.is_none() && body.image.is_none() {
        return Err(ApiError::BadRequest(
            "at least one of `name` or `image` required".into(),
        ));
    }
    if let Some(ref n) = body.name {
        if n.len() > 200 {
            return Err(ApiError::BadRequest("name too long".into()));
        }
    }
    if let Some(ref img) = body.image {
        if img.len() > 4096 {
            return Err(ApiError::BadRequest("image url too long".into()));
        }
    }
    // 动态构建 update
    let repo = AuthRepo::new(&state.db);
    if let Some(name) = body.name.as_ref() {
        repo.update_user_name(&user_id, name).await?;
    }
    if let Some(image) = body.image.as_ref() {
        repo.update_user_image(&user_id, image).await?;
    }
    state.realtime.publish(
        pc_realtime::LiveEvent::new("auth.profile_updated", "user", Uuid::nil())
            .with_actor(&user_id),
    );
    let user = AuthRepo::new(&state.db)
        .find_by_id(&user_id)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("user not found".into()))?;
    Ok(Json(ProfilePayload {
        id: user.id,
        email: Some(user.email),
        name: Some(user.name),
        image: user.image,
    }))
}


// ============ Round 29: Better-Auth wire endpoints ============

#[derive(Debug, Deserialize)]
struct SignInEmailBody {
    email: String,
    password: String,
    #[serde(default)]
    #[allow(dead_code)]
    remember_me: Option<bool>,
}

#[derive(Debug, Serialize)]
struct AuthSuccessResponse {
    #[serde(rename = "success")]
    success: bool,
    #[serde(rename = "user")]
    user: SessionUserOut,
    #[serde(rename = "redirect")]
    redirect: bool,
    #[serde(rename = "token")]
    token: String,
    #[serde(rename = "expiresAt")]
    expires_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct SessionUserOut {
    id: String,
    email: String,
    name: String,
    #[serde(rename = "emailVerified")]
    email_verified: bool,
    #[serde(rename = "image")]
    image: Option<String>,
}

async fn sign_in_email(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SignInEmailBody>,
) -> ApiResult<Json<AuthSuccessResponse>> {
    let email = body.email.trim().to_lowercase();
    let password = body.password;
    if email.is_empty() || password.is_empty() {
        return Err(ApiError::BadRequest("email and password required".into()));
    }
    let repo = AuthRepo::new(&state.db);
    // 1) find user by email
    let user = repo
        .find_by_email(&email)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("invalid email or password".into()))?;
    // 2) find credential account with password
    let acct = repo
        .find_account_for_user(&user.id, "credential")
        .await?
        .ok_or_else(|| ApiError::Unauthorized("no password set for this account".into()))?;
    let stored_hash = acct
        .password
        .ok_or_else(|| ApiError::Unauthorized("no password set for this account".into()))?;
    if !pc_auth::verify_password(&password, &stored_hash) {
        return Err(ApiError::Unauthorized("invalid email or password".into()));
    }
    // 3) rotate: delete old session(s) for this user (if any from cookie), then issue new
    if let Some(t) = extract_token(&headers) {
        let _ = repo.revoke_session_by_token(&t).await;
    }
    let session_token = pc_auth::generate_session_token();
    let session_id: String = format!("s_{}", uuid::Uuid::new_v4().simple());
    let expires_at = Utc::now() + Duration::days(30);
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string());
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    repo.upsert_session(&NewSession {
        id: session_id,
        token: session_token.clone(),
        user_id: user.id.clone(),
        expires_at: Timestamp::from_dt(expires_at),
        ip_address: ip,
        user_agent: ua,
    })
    .await?;
    state.realtime.publish(
        pc_realtime::LiveEvent::new("auth.signed_in", "user", Uuid::nil()).with_actor(&user.id),
    );
    Ok(Json(AuthSuccessResponse {
        success: true,
        user: SessionUserOut {
            id: user.id.clone(),
            email: user.email.clone(),
            name: user.name.clone(),
            email_verified: user.email_verified,
            image: user.image.clone(),
        },
        redirect: false,
        token: session_token,
        expires_at,
    }))
}

#[derive(Debug, Deserialize)]
struct SignUpEmailBody {
    name: String,
    email: String,
    password: String,
}

#[derive(Debug, Serialize)]
struct SignUpResponse {
    success: bool,
    user: SessionUserOut,
    token: String,
    expires_at: chrono::DateTime<Utc>,
}

async fn sign_up_email(
    State(state): State<AppState>,
    Json(body): Json<SignUpEmailBody>,
) -> ApiResult<Json<SignUpResponse>> {
    let email = body.email.trim().to_lowercase();
    let name = body.name.trim();
    if email.is_empty() || name.is_empty() || body.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "name, email, password (>=8 chars) required".into(),
        ));
    }
    let repo = AuthRepo::new(&state.db);
    // 1) check existing email
    if repo.find_user_id_by_email(&email).await?.is_some() {
        return Err(ApiError::Conflict("email already registered".into()));
    }
    // 2) create user
    let user_id = format!("u_{}", uuid::Uuid::new_v4().simple());
    repo.upsert_user(&NewUser {
        id: user_id.clone(),
        name: name.to_string(),
        email: email.clone(),
        email_verified: false,
        image: None,
    })
    .await?;
    // 3) create credential account with argon2 hash
    let phc = pc_auth::hash_password(&body.password)
        .map_err(|e| ApiError::Internal(format!("hash failed: {e}")))?;
    repo.create_credential_account(&user_id, &phc).await?;
    // 4) issue session
    let session_token = pc_auth::generate_session_token();
    let session_id = format!("s_{}", uuid::Uuid::new_v4().simple());
    let expires_at = Utc::now() + Duration::days(30);
    repo.upsert_session(&NewSession {
        id: session_id,
        token: session_token.clone(),
        user_id: user_id.clone(),
        expires_at: Timestamp::from_dt(expires_at),
        ip_address: None,
        user_agent: None,
    })
    .await?;
    state.realtime.publish(
        pc_realtime::LiveEvent::new("auth.signed_up", "user", Uuid::nil()).with_actor(&user_id),
    );
    Ok(Json(SignUpResponse {
        success: true,
        user: SessionUserOut {
            id: user_id.clone(),
            email: email.clone(),
            name: name.to_string(),
            email_verified: false,
            image: None,
        },
        token: session_token,
        expires_at,
    }))
}

#[derive(Debug, Deserialize)]
struct RefreshBody {
    #[serde(default)]
    token: Option<String>,
}

#[derive(Debug, Serialize)]
struct RefreshResponse {
    success: bool,
    token: String,
    expires_at: chrono::DateTime<Utc>,
}

async fn refresh_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RefreshBody>,
) -> ApiResult<Json<RefreshResponse>> {
    // Prefer explicit token from body, fall back to Authorization/Cookie
    let old_token = body
        .token
        .clone()
        .or_else(|| extract_token(&headers))
        .ok_or_else(|| ApiError::Unauthorized("no token to refresh".into()))?;
    let repo = AuthRepo::new(&state.db);
    // 1) find old session
    let session_row = repo
        .find_session_by_token(&old_token)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("invalid token".into()))?;
    let user_id = session_row.user_id.clone();
    // 2) check user still active (email_verified not required for refresh)
    if !repo.user_exists(&user_id).await? {
        let _ = repo.revoke_session_by_token(&old_token).await;
        return Err(ApiError::Unauthorized("user no longer exists".into()));
    }
    // 3) rotate: delete old, issue new
    repo.revoke_session_by_token(&old_token).await?;
    let new_token = pc_auth::generate_session_token();
    let session_id = format!("s_{}", uuid::Uuid::new_v4().simple());
    let expires_at = Utc::now() + Duration::days(30);
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string());
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    repo.upsert_session(&NewSession {
        id: session_id,
        token: new_token.clone(),
        user_id: user_id.clone(),
        expires_at: Timestamp::from_dt(expires_at),
        ip_address: ip,
        user_agent: ua,
    })
    .await?;
    state.realtime.publish(
        pc_realtime::LiveEvent::new("auth.session_rotated", "user", Uuid::nil()).with_actor(&user_id),
    );
    Ok(Json(RefreshResponse {
        success: true,
        token: new_token,
        expires_at,
    }))
}


// ============================================================================
// Round 41: legacy short aliases for /api/get-session and /api/profile.
// These mirror Node's older (pre-/api/auth/) routes — still used by some
// UI code paths and CLI tooling.  Both require a board session token.
// ============================================================================

async fn get_session_short(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user_id = match crate::state::require_user_id(&state, &headers).await {
        Ok(id) => id,
        Err(_) => {
            return Ok(Json(json!({
                "session": null,
                "user": null,
            })));
        }
    };
    let user = AuthRepo::new(&state.db)
        .find_by_id(&user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map(|u| {
            json!({
                "id": u.id,
                "name": u.name,
                "email": u.email,
                "image": u.image,
            })
        });
    Ok(Json(json!({
        "session": {
            "id": format!("paperclip:session:{user_id}"),
            "userId": user_id,
        },
        "user": user,
    })))
}

async fn get_profile_short(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    let user = AuthRepo::new(&state.db)
        .find_by_id(&user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("user {user_id}")))?;
    let companies = CompanyMemberRepo::new(&state.db)
        .list_company_ids_for_user(&user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "id": user.id,
        "name": user.name,
        "email": user.email,
        "image": user.image,
        "createdAt": user.created_at.as_datetime(),
        "companyIds": companies,
    })))
}
