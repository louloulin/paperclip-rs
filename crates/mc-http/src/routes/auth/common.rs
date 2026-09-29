//! `/auth/*` 各面共用的叶子工具：code 哈希与格式校验、dev 万能码、
//! session cookie 解析、`session_response` 合并响应。
//!
//! **本文件不含任何 handler。** 四个 handler 面各自持有自己的 DTO 与路由函数：
//! [`super::code`]（验证码）、[`super::session`]（登出 / 续期）、
//! [`super::cli_token`]（CLI 换 PAT）、[`super::google`]（Google 登录）。
//!
//! 拆分自拆分前的单文件 `routes/auth.rs`（LUM-2530）：item 逐字搬移，
//! 唯一规范化是拆分必需的可见性窄化（私有 `fn` → `pub(super) fn`）。

use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_auth::cookie::{CookieOptions, SameSite};
use mc_auth::session::Session;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::state::AppState;

// ============================================================
// 公共工具
// ============================================================

/// 把 raw 6 位数字 code 换算成 hex(sha256(code))。
pub(super) fn hash_code(code: &str) -> String {
    let mut h = Sha256::new();
    h.update(code.as_bytes());
    hex::encode(h.finalize())
}

/// 是否 6 位数字。
pub(super) fn is_six_digits(code: &str) -> bool {
    code.len() == 6 && code.chars().all(|c| c.is_ascii_digit())
}

/// 从 `MULTICA_DEV_VERIFICATION_CODE` 环境变量读取万能验证码（仅 dev）。
pub(super) fn dev_verification_code() -> Option<String> {
    let raw = std::env::var("MULTICA_DEV_VERIFICATION_CODE").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 是否 dev 模式 —— 上游 `APP_ENV != "production"` 才允许万能码。
pub(super) fn dev_mode(state: &AppState) -> bool {
    state.config.dev_mode
}

/// 构造 session cookie + CSRF 响应头 + JSON body 合并响应。
pub(super) fn session_response(
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

pub(super) fn email_local_part(email: &str) -> String {
    email.split('@').next().unwrap_or(email).to_string()
}

pub(super) fn parse_session_cookie(header: &str, name: &str) -> Option<String> {
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

#[allow(dead_code)]
fn _unused_uuid() -> Uuid {
    Uuid::new_v4()
}
