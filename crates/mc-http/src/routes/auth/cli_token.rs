//! `POST /api/cli-token`（上游 `router.go:1628`）—— CLI 登录换取 PAT。
//!
//! 上游 `handler/auth.go::IssueCliToken` 签发无状态 JWT；本仓 M1 还没有 JWT 签发链，
//! 因此本实现返回一个 **30 天 TTL 的 PAT**（与 `/api/me/pats` 同一个 `PatStore`，
//! `scopes = ["cli"]`）—— 有意的等价替换（可撤销 vs 不可撤销），见 LUM-1347
//! 集成报告；M9 接入 JWT 后可平滑切换。
//!
//! 拆分自拆分前的单文件 `routes/auth.rs`（LUM-2530）：item 逐字搬移。

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use mc_core::Id;
use mc_errors::Error;

use super::common::parse_session_cookie;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

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
pub(super) async fn cli_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Response> {
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

pub(super) fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    hex::encode(h.finalize())
}
