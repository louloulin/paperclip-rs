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
use serde_json::json;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/get-session", get(get_session))
        .route("/api/auth/sign-in", post(sign_in))
        .route("/api/auth/sign-out", post(sign_out))
        .route("/api/auth/issue-key", post(issue_key))
        .route("/api/auth/revoke-key", post(revoke_key))
}

#[derive(Debug, Serialize)]
struct SessionInfo {
    user_id: String,
    name: String,
    email: String,
    email_verified: bool,
    method: &'static str,
}

async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<SessionInfo>> {
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
    let row: Option<(String, Option<String>, bool)> =
        sqlx::query_as("SELECT id, name, email_verified FROM \"user\" WHERE id = $1")
            .bind(&user_id)
            .fetch_optional(state.db.pool())
            .await?;
    let (uid, name, ev) = row.ok_or_else(|| ApiError::Unauthorized("user not found".into()))?;
    Ok(Json(SessionInfo {
        user_id: uid,
        name: name.unwrap_or_default(),
        email: String::new(),
        email_verified: ev,
        method,
    }))
}

#[derive(Debug, Deserialize)]
struct SignInBody {
    email: String,
    #[serde(default)]
    name: Option<String>,
    /// 简化版：直接传入 `user_id`（不实现密码哈希）
    #[serde(default)]
    user_id: Option<String>,
}

#[derive(Debug, Serialize)]
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
        ensure_user(
            &state,
            &uid,
            body.name.as_deref().unwrap_or("user"),
            &body.email,
        )
        .await?;
        uid
    } else if !body.email.is_empty() {
        // 通过 email 查找
        let row: Option<(String,)> = sqlx::query_as("SELECT id FROM \"user\" WHERE email = $1")
            .bind(&body.email)
            .fetch_optional(state.db.pool())
            .await?;
        if let Some((id,)) = row {
            id
        } else {
            let new_id = format!("u_{}", random_id(21));
            ensure_user(
                &state,
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

    // 创建 session（7 天有效）
    let session_id = format!("s_{}", random_id(32));
    let session_token = format!("tok_{}", random_id(48));
    let expires_at = Utc::now() + Duration::days(7);
    sqlx::query(
        "INSERT INTO session (id, user_id, token, expires_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, now(), now())",
    )
    .bind(&session_id)
    .bind(&user_id)
    .bind(&session_token)
    .bind(expires_at)
    .execute(state.db.pool())
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

async fn ensure_user(
    state: &AppState,
    user_id: &str,
    name: &str,
    email: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) \
         VALUES ($1, $2, $3, false, now(), now()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(user_id)
    .bind(name)
    .bind(email)
    .execute(state.db.pool())
    .await?;
    Ok(())
}

async fn sign_out(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<StatusCode> {
    if let Some(token) = extract_token(&headers) {
        sqlx::query("DELETE FROM session WHERE token = $1")
            .bind(&token)
            .execute(state.db.pool())
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
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
    let r =
        sqlx::query("UPDATE board_api_keys SET revoked_at = now() WHERE id = $1 AND user_id = $2")
            .bind(key_id)
            .bind(&user_id)
            .execute(state.db.pool())
            .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound("api key".into()));
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
