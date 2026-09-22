//! `/api/me/pats` 系列路由 —— 用户管理自己的 Personal Access Token。
//!
//! 上游 multica 没有显式 `/api/pats` REST endpoint（PAT 主要在 daemon auth 中间件
//! 里被消费）；本路由是 multica-rs 自加的「用户管理自己 PAT」的便捷接口。
//!
//! 存储后端：`mc_auth::PatStoreContainer`（默认 `InMemoryPatStore`）。
//! sub-issue B 后续会替换为 DB-backed store；本路由不变。
//!
//! Token 形态：
//! - 创建时返回明文 token（仅此一次）
//! - 存的是 sha256 hex
//! - 显示时只暴露 `mk_<last4>` 前缀 + last4，避免泄露完整 token

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use mc_auth::pat::Pat;
use mc_core::Id;
use mc_errors::Error;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 多实例同名前缀：multica key 前缀（与 `mc_auth::DEFAULT_API_KEY_PREFIX` 一致）。
pub const PAT_PREFIX: &str = "mk_pat_";

pub fn router() -> Router<Arc<AppState>> {
    // 注意：axum 0.7（matchit 0.7）路径参数语法是 `:id`，不是 `{id}`（那是 axum 0.8）。
    Router::new()
        .route("/api/me/pats", get(list_my_pats).post(create_my_pat))
        .route("/api/me/pats/:id", axum::routing::delete(revoke_my_pat))
}

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PatDto {
    pub id: String,
    pub name: String,
    pub token_last4: String,
    /// 显示形式：`mk_pat_xxxx`（前缀 + last4）
    pub display_token: String,
    pub expires_at: String,
    pub last_used_at: Option<String>,
    pub scopes: Vec<String>,
    pub created_at: String,
}

impl From<&Pat> for PatDto {
    fn from(p: &Pat) -> Self {
        let display = format!("{PAT_PREFIX}{}", p.token_last4);
        Self {
            id: p.id.as_string(),
            name: p.name.clone(),
            token_last4: p.token_last4.clone(),
            display_token: display,
            expires_at: p.expires_at.to_rfc3339(),
            last_used_at: p.last_used_at.as_ref().map(chrono::DateTime::to_rfc3339),
            scopes: p.scopes.clone(),
            created_at: p.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreatePatRequest {
    pub name: String,
    pub scopes: Option<Vec<String>>,
    /// 可选 TTL（秒）；默认 30 天。
    pub ttl_secs: Option<i64>,
}

/// 创建响应：除 PAT 元信息外，附带明文 token（仅此一次返回）。
#[derive(Debug, Serialize)]
pub struct CreatePatResponse {
    #[serde(flatten)]
    pub pat: PatDto,
    /// 明文 token。客户端必须保存；后续 GET 仅返回 last4。
    pub token: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_my_pats(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
) -> ApiResult<Json<Vec<PatDto>>> {
    let store = state.pat.store();
    let pats = store.list_for_user(user.id()).await.map_err(pat_err)?;
    Ok(Json(pats.iter().map(PatDto::from).collect()))
}

async fn create_my_pat(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Json(req): Json<CreatePatRequest>,
) -> ApiResult<Response> {
    if req.name.trim().is_empty() {
        return Err(Error::Validation {
            message: "name must not be empty".into(),
            details: vec![],
        }
        .into());
    }

    // 32 字节随机 → hex（64 字符）+ 前缀 `mk_pat_`。
    let raw = generate_pat_secret();
    let token = format!("{PAT_PREFIX}{raw}");
    let token_hash = sha256_hex(&raw);
    let token_last4 = raw
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();

    let now = Utc::now();
    let ttl = req.ttl_secs.unwrap_or(60 * 60 * 24 * 30);
    let expires_at = now + chrono::Duration::seconds(ttl);

    let pat = Pat {
        id: Id::new(),
        user_id: user.id(),
        name: req.name.trim().to_string(),
        token_hash,
        token_last4,
        expires_at,
        last_used_at: None,
        scopes: req.scopes.unwrap_or_default(),
        created_at: now,
    };

    state.pat.store().put(pat.clone()).await.map_err(pat_err)?;

    let dto = PatDto::from(&pat);
    Ok((
        StatusCode::CREATED,
        Json(CreatePatResponse {
            pat: dto,
            token: token.clone(),
        }),
    )
        .into_response())
}

async fn revoke_my_pat(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    axum::extract::Path(pat_id): axum::extract::Path<String>,
) -> ApiResult<StatusCode> {
    let id = Id::parse(&pat_id).map_err(|_| Error::NotFound {
        resource: "pat".into(),
    })?;

    let store = state.pat.store();
    // 仅允许用户撤销自己的 PAT；先 list 校验所有权
    let mine = store.list_for_user(user.id()).await.map_err(pat_err)?;
    if !mine.iter().any(|p| p.id == id) {
        return Err(Error::NotFound {
            resource: "pat".into(),
        }
        .into());
    }
    store.delete(id).await.map_err(pat_err)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 内部 helper
// ---------------------------------------------------------------------------

#[allow(clippy::needless_pass_by_value)] // 4 处 `.map_err(pat_err)` 的函数指针必须按值接收。
fn pat_err(e: mc_auth::pat::PatError) -> Error {
    match e {
        mc_auth::pat::PatError::NotFound => Error::NotFound {
            resource: "pat".into(),
        },
        mc_auth::pat::PatError::Expired => Error::Unauthorized {
            message: "pat expired".into(),
        },
    }
}

fn generate_pat_secret() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    hex::encode(h.finalize())
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pat_secret_is_64_hex() {
        let s = generate_pat_secret();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
        let s2 = generate_pat_secret();
        assert_ne!(s, s2);
    }

    #[test]
    fn sha256_is_deterministic() {
        let a = sha256_hex("hello");
        let b = sha256_hex("hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn last4_extraction() {
        let raw = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let last4: String = raw
            .chars()
            .rev()
            .take(4)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        assert_eq!(last4, "cdef");
    }

    #[test]
    fn pat_prefix_format() {
        let raw = "deadbeef";
        let tok = format!("{PAT_PREFIX}{raw}");
        assert!(tok.starts_with("mk_pat_"));
    }
}
