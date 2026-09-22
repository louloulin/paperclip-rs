//! `/api/tokens` 系列路由 —— 用户管理自己的 Personal Access Token。
//!
//! 上游契约（`server/cmd/server/router.go:1879-1884`，`handler/personal_access_token.go`）：
//!
//! | Method | Path | Handler |
//! | --- | --- | --- |
//! | GET | `/api/tokens` | `ListPersonalAccessTokens` |
//! | POST | `/api/tokens` | `CreatePersonalAccessToken` |
//! | POST | `/api/tokens/current/renew` | `RenewCurrentPersonalAccessToken` |
//! | DELETE | `/api/tokens/{id}` | `RevokePersonalAccessToken` |
//!
//! M1-E（LUM-1362）：`docs/08-M1-PAT.md` 原来写「上游没有显式 PAT REST endpoint」
//! 是事实错误——上游有 `/api/tokens`；本模块把主路径改为上游路径，并把 M0 自造的
//! `/api/me/pats` 保留一个发布周期作为 **deprecated alias**（响应带 `Deprecation` 头）。
//! 决策与残留偏离见 `docs/17-M1-CONTRACT-GAPS.md`。
//!
//! 存储后端（**M1-F / LUM-1375 更正**）：`personal_access_token` 表，经
//! `mc_repos::pat::PatRepo` —— 每次请求 `PatRepo::new(state.db.clone())`，
//! 与 `routes/auth.rs` 里 `VerificationCodeRepo` 的用法一致。
//!
//! **生产路径不再经过 `mc_auth::PatStoreContainer`**：内存实现只作无库场景的
//! fallback（见 `state.rs` 字段注释；`docs/17` R7 已闭环）。本切片之前 PAT 全部
//! 落在 `InMemoryPatStore`，进程重启即失效、`GET /api/tokens` 看不到历史 token。
//!
//! 上游 daemon 对 PAT 的消费中间件（`Authorization: Bearer` → session）仍属 M3；
//! 本切片只要求 `PatRepo::touch`（`last_used_at`）**函数可达 + DB 测试覆盖**，
//! 不在路由层实现该中间件。
//!
//! Token 形态：
//! - 创建时返回明文 token（仅此一次）
//! - 存的是 sha256 hex
//! - 显示时只暴露 last4，避免泄露完整 token

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use mc_core::Id;
use mc_errors::Error;
use mc_repos::pat::{NewPat, PatRepo, PatRow};
use mc_repos::RepoError;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 多实例同名前缀：multica key 前缀（与 `mc_auth::DEFAULT_API_KEY_PREFIX` 一致）。
pub const PAT_PREFIX: &str = "mk_pat_";

/// 上游 `PATRenewThreshold`（`handler/personal_access_token.go:22`）：剩余寿命进入
/// 该窗口后 PAT 可被就地续期（7 天）。
const PAT_RENEW_THRESHOLD_SECS: i64 = 7 * 24 * 60 * 60;
/// 上游 `PATRenewExtension`：每次续期把 `expires_at` 推到 now + 90 天。
const PAT_RENEW_EXTENSION_SECS: i64 = 90 * 24 * 60 * 60;

/// 上游路径（主）+ M0 旧路径（deprecated alias）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(tokens_router())
        .merge(deprecated_pats_router())
}

/// 上游契约路径。
///
/// 注意：axum 0.7（matchit 0.7）路径参数语法是 `:id`，不是 `{id}`（那是 axum 0.8）。
/// 上游 chi 在 `r.Route("/api/tokens", ...)` 里注册 `r.Get("/")` 与 `r.Post("/")`；
/// Mount 语义下 `/api/tokens` 与 `/api/tokens/` **两种形态都合法**。上游自己的客户端
/// （`server/cmd/multica/cmd_auth.go:329`）与 daemon
/// （`server/internal/daemon/client.go:704`）都只请求**不带**尾斜杠的 `/api/tokens`
/// 与 `/api/tokens/current/renew`，本仓当初因此只注册无尾斜杠形式，并把「打尾斜杠者会
/// 404」记为已知风险（`docs/17` R3）。现按全仓统一口径补齐带尾斜杠的别名：风险归零，
/// 代价是两个路由键（`docs/37` §15）。
fn tokens_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/tokens", get(list_my_pats).post(create_my_pat))
        .route("/api/tokens/", get(list_my_pats).post(create_my_pat))
        .route(
            "/api/tokens/current/renew",
            axum::routing::post(renew_current_pat),
        )
        .route("/api/tokens/:id", axum::routing::delete(revoke_my_pat))
}

/// M0/`docs/08` 的旧路径，保留一个发布周期：所有响应带 `Deprecation: true`
/// 与指向新路径的 `Link: rel="successor-version"`（RFC 8594）。
fn deprecated_pats_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/me/pats", get(list_my_pats).post(create_my_pat))
        .route("/api/me/pats/:id", axum::routing::delete(revoke_my_pat))
        .layer(axum::middleware::from_fn(add_deprecation_headers))
}

/// 给 deprecated alias 的响应统一加迁移提示头。
async fn add_deprecation_headers(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    headers.insert("deprecation", HeaderValue::from_static("true"));
    headers.insert(
        "link",
        HeaderValue::from_static("</api/tokens>; rel=\"successor-version\""),
    );
    res
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

impl From<&PatRow> for PatDto {
    fn from(p: &PatRow) -> Self {
        let display = format!("{PAT_PREFIX}{}", p.token_last4);
        Self {
            id: p.id.as_string(),
            name: p.name.clone(),
            token_last4: p.token_last4.clone(),
            display_token: display,
            expires_at: p.expires_at.to_rfc3339(),
            last_used_at: p.last_used_at.map(|t| t.to_rfc3339()),
            scopes: scopes_of(p),
            created_at: p.created_at.to_rfc3339(),
        }
    }
}

/// `personal_access_token.scopes` 在 DB 里是 `JSONB`（`PatRow.scopes` 为
/// `serde_json::Value`）；本仓的对外形状是 `Vec<String>`。非数组 / 非字符串
/// 元素一律退化为空列表，避免一行脏数据把整个 list 响应打成 500。
fn scopes_of(row: &PatRow) -> Vec<String> {
    serde_json::from_value::<Vec<String>>(row.scopes.clone()).unwrap_or_default()
}

#[derive(Debug, Deserialize)]
pub struct CreatePatRequest {
    pub name: String,
    pub scopes: Option<Vec<String>>,
    /// 可选 TTL（秒）；默认 30 天。与上游 `expires_in_days` 二选一。
    pub ttl_secs: Option<i64>,
    /// 上游 `CreatePATRequest.expires_in_days`（`handler/personal_access_token.go:57`）。
    /// 提供时优先于 `ttl_secs`。注意上游 `nil` / `<= 0` 表示**永不过期**，本仓
    /// `Pat.expires_at` 非 Option，故退化为默认 30 天（docs/17 残留偏离 R2）。
    pub expires_in_days: Option<i64>,
}

/// 创建响应：除 PAT 元信息外，附带明文 token（仅此一次返回）。
#[derive(Debug, Serialize)]
pub struct CreatePatResponse {
    #[serde(flatten)]
    pub pat: PatDto,
    /// 明文 token。客户端必须保存；后续 GET 仅返回 last4。
    pub token: String,
}

/// 续期响应（上游 `RenewPATResponse`，`handler/personal_access_token.go:139`）。
///
/// `renewed=false` 不是错误：只表示调用时 token 还没进入续期窗口。
#[derive(Debug, Serialize)]
pub struct RenewPatResponse {
    pub expires_at: String,
    pub renewed: bool,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_my_pats(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
) -> ApiResult<Json<Vec<PatDto>>> {
    let repo = PatRepo::new(state.db.clone());
    let pats = repo.list_for_user(user.id()).await.map_err(pat_err)?;
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

    let ttl = match (req.expires_in_days, req.ttl_secs) {
        // 上游 `expires_in_days`：仅 > 0 有效；`nil` / `<= 0` 上游为「永不过期」。
        (Some(days), _) if days > 0 => days * 24 * 60 * 60,
        _ => req.ttl_secs.unwrap_or(60 * 60 * 24 * 30),
    };
    let expires_at = Utc::now() + chrono::Duration::seconds(ttl);

    let repo = PatRepo::new(state.db.clone());
    let row = repo
        .create(NewPat {
            user_id: user.id(),
            name: req.name.trim().to_string(),
            // hash / last4 都用 `PatRepo` 的 canonical helper：与
            // `get_by_token` 的查询口径、以及 daemon 中间件（M3）必须完全一致。
            token_hash: PatRepo::hash_token(&raw),
            token_last4: PatRepo::last4(&raw),
            expires_at,
            scopes: req.scopes.unwrap_or_default(),
        })
        .await
        .map_err(pat_err)?;

    // `create` 的 RETURNING 带回 DB 生成的 id / created_at —— 响应即库内真实状态。
    let dto = PatDto::from(&row);
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

    let repo = PatRepo::new(state.db.clone());
    // 仅允许用户撤销自己的 PAT；先按用户列出（`list_for_user` 的 SQL 已带
    // `user_id = $1 AND revoked_at IS NULL`）再撤销 —— 别人的 id 一律 404，
    // 不泄露「该 id 存在」这一信息。
    let mine = repo.list_for_user(user.id()).await.map_err(pat_err)?;
    if !mine.iter().any(|p| p.id == id) {
        return Err(Error::NotFound {
            resource: "pat".into(),
        }
        .into());
    }
    // 撤销 = `revoked_at = now()`（迁移 0002），不是物理删除：
    // 行留在库里，`get_by_token` / `list_for_user` 都会过滤掉它。
    repo.revoke(id).await.map_err(pat_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/tokens/current/renew`（上游 `RenewCurrentPersonalAccessToken`）。
///
/// 就地延长**本请求所持 PAT** 的 `expires_at`，不重新签发 token：
/// 上游刻意不轮换明文（CLI/daemon 多进程共享同一 PAT，轮换会同时打断所有进程）。
///
/// 语义对齐上游（`handler/personal_access_token.go:159`）：
/// - 身份只来自 `Authorization: Bearer <PAT>`，**不**另取 `x-multica-user-id`
///   （上游中间件已把 PAT 解析成 userID；本仓直接按 `token_hash` 取行，等价且更少耦合）；
/// - 非 `Bearer ` 前缀 / 非 `mk_pat_` 前缀 → 400 `only personal access tokens can be renewed`；
/// - 已过期或查不到 → 401 `token is no longer valid`；
/// - 剩余寿命 > 7 天 → 200 `{expires_at, renewed: false}`（不是错误）；
/// - 否则推到 now + 90 天 → 200 `{expires_at, renewed: true}`。
pub async fn renew_current_pat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<RenewPatResponse>> {
    let raw = bearer_token(&headers).ok_or_else(renew_not_a_pat)?;
    let secret = raw.strip_prefix(PAT_PREFIX).ok_or_else(renew_not_a_pat)?;

    let repo = PatRepo::new(state.db.clone());
    // `get_by_token` 的 SQL 自带 `revoked_at IS NULL AND expires_at > now()` 过滤：
    // `Ok(None)` = 不存在 / 已撤销 / 已过期。上游由 auth 中间件拦过期 token，
    // 本仓没有该中间件，所以由这条 SQL 承担（否则会把过期 PAT 复活成 now+90d）。
    //
    // 注意与旧内存实现的差别：真实的 DB 故障现在是 500（`RepoError::Db`），
    // 不再被伪装成 401 —— 基础设施错误不该让客户端去重新 login。
    let row = repo
        .get_by_token(secret)
        .await
        .map_err(pat_err)?
        .ok_or_else(|| Error::Unauthorized {
            message: "token is no longer valid".into(),
        })?;

    let now = Utc::now();
    let remaining = row.expires_at - now;
    if remaining > chrono::Duration::seconds(PAT_RENEW_THRESHOLD_SECS) {
        return Ok(Json(RenewPatResponse {
            expires_at: row.expires_at.to_rfc3339(),
            renewed: false,
        }));
    }

    // 就地延长（不轮换明文），并**落库** —— 否则重启后续期白做。
    let new_expiry = now + chrono::Duration::seconds(PAT_RENEW_EXTENSION_SECS);
    repo.update_expires_at(row.id, new_expiry)
        .await
        .map_err(pat_err)?;
    Ok(Json(RenewPatResponse {
        expires_at: new_expiry.to_rfc3339(),
        renewed: true,
    }))
}

/// 从 `Authorization: Bearer <token>` 取明文 token；缺少 `Bearer ` 前缀或为空 → None
/// （与上游 `strings.TrimPrefix` + `rawToken == authHeader` 判定一致）。
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let raw = value.strip_prefix("Bearer ")?.trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw)
}

fn renew_not_a_pat() -> Error {
    Error::Validation {
        message: "only personal access tokens can be renewed".into(),
        details: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// 内部 helper
// ---------------------------------------------------------------------------

#[allow(clippy::needless_pass_by_value)] // 5 处 `.map_err(pat_err)` 的函数指针必须按值接收。
fn pat_err(e: RepoError) -> Error {
    match e {
        RepoError::NotFound => Error::NotFound {
            resource: "pat".into(),
        },
        // `personal_access_token` 目前没有唯一约束会触发 Conflict；保留分支是为了
        // 穷尽匹配，映射成 409 而不是 500。
        RepoError::Conflict => Error::Conflict {
            message: "pat already exists".into(),
        },
        RepoError::Db(msg) => Error::Internal(msg),
    }
}

fn generate_pat_secret() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
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

    /// 入库口径 = `sha256 hex`，与上游 `personal_access_tokens.token_hash` 列一致。
    /// 钉死已知值（而不是自己和自己比），这样 helper 被换掉时会立刻红。
    #[test]
    fn token_hash_is_sha256_hex() {
        assert_eq!(
            PatRepo::hash_token("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(PatRepo::hash_token("hello").len(), 64);
    }

    /// 展示用的 last4 必须与入库时用的 helper 同源，否则 list 响应里的
    /// `display_token` 会和 daemon（M3）反查出的行对不上。
    #[test]
    fn last4_extraction() {
        let raw = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(PatRepo::last4(raw), "cdef");
        // 短于 4 位时原样返回（`PatRepo::last4` 的约定）。
        assert_eq!(PatRepo::last4("ab"), "ab");
    }

    #[test]
    fn pat_prefix_format() {
        let raw = "deadbeef";
        let tok = format!("{PAT_PREFIX}{raw}");
        assert!(tok.starts_with("mk_pat_"));
    }

    #[test]
    fn bearer_token_requires_bearer_prefix() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer mk_pat_abc"),
        );
        assert_eq!(bearer_token(&h), Some("mk_pat_abc"));

        // 上游：没有 `Bearer ` 前缀 → 视为非 PAT，返回 400。
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("mk_pat_abc"),
        );
        assert_eq!(bearer_token(&h), None);

        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer   "),
        );
        assert_eq!(bearer_token(&h), None);

        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }

    #[test]
    fn renew_window_constants_match_upstream() {
        // 上游 `PATRenewThreshold = 7d` / `PATRenewExtension = 90d`。
        assert_eq!(PAT_RENEW_THRESHOLD_SECS, 7 * 24 * 60 * 60);
        assert_eq!(PAT_RENEW_EXTENSION_SECS, 90 * 24 * 60 * 60);
    }

    #[test]
    fn renew_rejection_is_400() {
        assert_eq!(renew_not_a_pat().http_status(), 400);
    }
}
