//! Personal Access Token 路由。
//!
//! 协议与 multica upstream `handler/personal_access_token.go` 对齐：
//! - `GET /api/tokens` — 列出当前 user 的 PAT。
//! - `POST /api/tokens` — 创建；响应里一次性返回 raw token。
//! - `POST /api/tokens/current/renew` — 在原 token 的 expires_at 即将到期时
//!   就地延长 TTL，不改变 raw token（保护 daemon 流程）。
//! - `DELETE /api/tokens/{id}` — 撤销（必须在 owner 上才生效）。
//!
//! M1 简化：raw token 通过 `Authorization: Bearer mc_pat_<secret>` 承载；
//! `/api/tokens/current/renew` 走相同 header。

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

use mc_core::id::Id;
use mc_errors::Error;
use mc_repos::{NewPat, PatRepo, PatRow};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct PatResponse {
    pub id: String,
    pub name: String,
    pub token_prefix: String,
    pub token_last4: String,
    pub expires_at: String,
    pub last_used_at: Option<String>,
    pub created_at: String,
}

impl PatResponse {
    pub fn from_row(row: &PatRow) -> Self {
        Self {
            id: row.id.as_string(),
            name: row.name.clone(),
            token_prefix: row.token_prefix.clone(),
            token_last4: row.token_last4.clone(),
            expires_at: row.expires_at.to_rfc3339(),
            last_used_at: row.last_used_at.map(|d| d.to_rfc3339()),
            created_at: row.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CreatePatResponse {
    #[serde(flatten)]
    pub pat: PatResponse,
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct CreatePatRequest {
    pub name: String,
    #[serde(default)]
    pub expires_in_days: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct RenewResponse {
    pub expires_at: String,
    pub renewed: bool,
}

fn actor_user_id(headers: &HeaderMap) -> Result<Id, ApiError> {
    headers
        .get("x-multica-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| Id::parse(s))
        .transpose()
        .map_err(|e| ApiError(Error::Unauthorized { message: e.to_string() }))?
        .ok_or_else(|| ApiError(Error::Unauthorized { message: "no user id".into() }))
}

/// Extracts the bearer token (if any) from `Authorization: Bearer mc_pat_<secret>`.
fn bearer_pat(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim())
        .filter(|s| s.starts_with("mc_pat_"))
}

fn hash_token(secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    hex::encode(h.finalize())
}

fn random_secret() -> String {
    let mut buf = [0u8; 24];
    let mut rng = rand::thread_rng();
    rng.fill_bytes(&mut buf);
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn raw_token(secret: &str) -> String {
    format!("mc_pat_{secret}")
}

fn slice_prefix(raw: &str) -> (String, String) {
    // 12-char prefix: mc_pat_xxxx; last4: last 4 bytes of the secret part.
    let prefix: String = raw.chars().take(12).collect();
    let last4: String = raw
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (prefix, last4)
}

const DEFAULT_TTL_SECS: u64 = 90 * 24 * 60 * 60;
const RENEW_THRESHOLD_SECS: u64 = 7 * 24 * 60 * 60;
const RENEW_EXTENSION_SECS: u64 = 90 * 24 * 60 * 60;

/// `GET /api/tokens`
pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<PatResponse>>, ApiError> {
    let user_id = actor_user_id(&headers)?;
    let rows = state
        .repos
        .pats
        .list(mc_repos::pat::PatFilter {
            user_id: Some(user_id),
            limit: Some(200),
        })
        .await
        .map_err(|e| ApiError(Error::Internal(format!("list pats: {e}"))))?;
    Ok(Json(rows.iter().map(PatResponse::from_row).collect()))
}

/// `POST /api/tokens`
pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreatePatRequest>,
) -> Result<Json<CreatePatResponse>, ApiError> {
    let user_id = actor_user_id(&headers)?;
    if body.name.trim().is_empty() {
        return Err(ApiError(Error::Validation {
            message: "name is required".into(),
            details: vec![],
        }));
    }

    let secret = random_secret();
    let raw = raw_token(&secret);
    let (prefix, last4) = slice_prefix(&raw);
    let token_hash = hash_token(secret.as_str());

    let ttl_secs = body
        .expires_in_days
        .filter(|n| *n > 0)
        .map(|n| n as u64 * 24 * 60 * 60)
        .unwrap_or(DEFAULT_TTL_SECS);

    let row = state
        .repos
        .pats
        .create(NewPat {
            id: None,
            user_id,
            name: body.name,
            token_hash,
            token_prefix: prefix,
            token_last4: last4,
            ttl_secs: Some(ttl_secs),
            scopes: vec![],
        })
        .await
        .map_err(|e| ApiError(Error::Internal(format!("create pat: {e}"))))?;

    Ok(Json(CreatePatResponse {
        pat: PatResponse::from_row(&row),
        token: raw,
    }))
}

/// `POST /api/tokens/current/renew`
pub async fn renew_current(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<RenewResponse>, ApiError> {
    let bearer = bearer_pat(&headers)
        .ok_or_else(|| ApiError(Error::Validation {
            message: "only bearer personal access tokens can be renewed".into(),
            details: vec![],
        }))?
        .to_string();
    let hash = hash_token(&bearer);
    let pat = state
        .repos
        .pats
        .find_by_hash(&hash)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("lookup pat: {e}"))))?
        .ok_or_else(|| ApiError(Error::Unauthorized {
            message: "token is no longer valid".into(),
        }))?;

    // Defence in depth: ensure caller's user_id matches pat owner.
    let user_id = actor_user_id(&headers)?;
    if pat.user_id != user_id {
        return Err(ApiError(Error::Unauthorized {
            message: "token does not belong to caller".into(),
        }));
    }

    let now = Utc::now();
    let remaining = (pat.expires_at - now).num_seconds();
    if remaining > RENEW_THRESHOLD_SECS as i64 {
        return Ok(Json(RenewResponse {
            expires_at: pat.expires_at.to_rfc3339(),
            renewed: false,
        }));
    }

    let new_expiry = now + chrono::Duration::seconds(RENEW_EXTENSION_SECS as i64);
    let updated = state
        .repos
        .pats
        .renew_in_place(pat.id, new_expiry)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("renew pat: {e}"))))?;
    Ok(Json(RenewResponse {
        expires_at: updated.to_rfc3339(),
        renewed: true,
    }))
}

/// `DELETE /api/tokens/{id}`
pub async fn revoke(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = actor_user_id(&headers)?;
    let id = Id::parse(&id).map_err(|e| ApiError(Error::Validation {
        message: e.to_string(),
        details: vec![],
    }))?;
    state
        .repos
        .pats
        .revoke(id, user_id)
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::NotFound => {
                ApiError(Error::NotFound { resource: "pat".into() })
            }
            other => ApiError(Error::Internal(format!("revoke pat: {other}"))),
        })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// Keep `Uuid` import in use even though the helpers don't mention it.
#[allow(dead_code)]
fn _silence_uuid_import() -> Uuid {
    Uuid::new_v4()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_token_format_is_prefix_plus_secret() {
        let raw = raw_token("aaaabbbb");
        assert_eq!(raw, "mc_pat_aaaabbbb");
    }

    #[test]
    fn slice_prefix_is_first_12_chars_and_last_4() {
        let raw = "mc_pat_abcDEF12345"; // 18 chars
        let (p, l) = slice_prefix(raw);
        assert_eq!(p, "mc_pat_abcDE"); // first 12 chars
        assert_eq!(l, "2345"); // last 4 chars
    }

    #[test]
    fn hash_token_is_deterministic() {
        assert_eq!(hash_token("same"), hash_token("same"));
        assert_ne!(hash_token("same"), hash_token("diff"));
    }

    #[test]
    fn bearer_pat_recognises_only_correct_prefix() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            "Bearer mc_pat_secretabc".parse().unwrap(),
        );
        assert_eq!(bearer_pat(&headers), Some("mc_pat_secretabc"));

        headers.insert("authorization", "Bearer session-token".parse().unwrap());
        assert_eq!(bearer_pat(&headers), None);

        headers.insert("authorization", "mc_pat_no-bearer".parse().unwrap());
        assert_eq!(bearer_pat(&headers), None);
    }

    #[test]
    fn random_secret_has_url_safe_chars() {
        let secret = random_secret();
        assert!(secret.len() >= 16, "secret too short: {secret}");
        for c in secret.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "unexpected char `{c}` in {secret}"
            );
        }
    }
}
