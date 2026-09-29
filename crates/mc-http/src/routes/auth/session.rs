//! `POST /auth/logout` + `POST /api/auth/refresh` —— 会话的清除与续期。
//!
//! 对应上游 `handler/auth.go::Handler.Logout` 与
//! `handler/session.go::Handler.RefreshSession`。
//!
//! 拆分自拆分前的单文件 `routes/auth.rs`（LUM-2530）：item 逐字搬移。

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use mc_auth::cookie::{CookieOptions, SameSite};
use mc_errors::Error;

use super::common::{parse_session_cookie, session_response};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

// ============================================================
// POST /auth/logout
// ============================================================

#[derive(Debug, Serialize)]
pub struct LogoutResponse {
    pub message: &'static str,
}

pub(super) async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Response> {
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

pub(super) async fn refresh_session(
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
