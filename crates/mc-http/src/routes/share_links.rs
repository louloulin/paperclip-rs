//! `/api/workspaces/{id}/share-links*` + `/api/share-links/*` 系列路由。
//!
//! 来源：LUM-1335（`feat/multica-rs-m1` @ 3ad402e）`routes/invitation.rs` 里的
//! share-link 增量，由 M1-D（LUM-1347）按本仓约定重写（见 `docs/09-M1-INTEGRATION.md` §2.2）：
//! - 独立文件 + 独立 `router()`（挂在 `mount.rs::mount_slice_share_link`），避免与
//!   A/C 的 `workspaces.rs` / `invitations.rs` 抢同一个文件
//! - 鉴权用本仓的 `AuthUser` 提取器（`X-Multica-User-Id`），不使用上游的
//!   `headers` 手工解析；workspace 角色检查复用 `invitations.rs` 的
//!   `require_workspace_admin` / `require_workspace_member`
//! - 仓储经 `ShareLinkRepo::new(&state.db)` 现构造（本仓 `AppState` 没有 `repos` 聚合）
//!
//! 路由面（与 multica `handler/invitation.go` 对齐）：
//! - `POST   /api/workspaces/:id/share-links`          — 创建（Owner|Admin）
//! - `GET    /api/workspaces/:id/share-links`          — 列出（member 以上）
//! - `DELETE /api/workspaces/:id/share-links/:linkId`  — 撤销（Owner|Admin）
//! - `GET    /api/share-links/:code`                   — 公开元数据（无需登录）
//! - `POST   /api/share-links/join`                    — 用 code 加入（需登录）

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use mc_core::workspace::WorkspaceRole;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::member::{MemberRepo, NewMember};
use mc_repos::share_link::{NewShareLink, ShareLinkFilter, ShareLinkRepo, ShareLinkRow};
use mc_repos::{RepoError, Repository};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::{not_found, require_workspace_admin, require_workspace_member};
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    // axum 0.7（matchit 0.7）：路径参数是 `:id`，`{id}` 会被当字面量段（恒 404）。
    Router::new()
        .route(
            "/api/workspaces/:id/share-links",
            get(list_share_links).post(create_share_link),
        )
        .route(
            "/api/workspaces/:id/share-links/:linkId",
            delete(revoke_share_link),
        )
        .route("/api/share-links/join", post(join_by_code))
        .route("/api/share-links/:code", get(share_link_info))
}

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ShareLinkDto {
    pub id: String,
    pub workspace_id: String,
    pub code: String,
    pub role: String,
    pub expires_at: Option<String>,
    pub max_uses: Option<i32>,
    pub use_count: i32,
    pub is_active: bool,
    pub created_at: String,
}

impl From<&ShareLinkRow> for ShareLinkDto {
    fn from(row: &ShareLinkRow) -> Self {
        Self {
            id: row.id().to_string(),
            workspace_id: row.workspace_id().to_string(),
            code: row.code.clone(),
            role: row.role().as_str().to_string(),
            expires_at: row.expires_at.map(|d| d.to_rfc3339()),
            max_uses: row.max_uses,
            use_count: row.use_count,
            is_active: row.is_active,
            created_at: row.created_at.to_rfc3339(),
        }
    }
}

/// 公开面只暴露"可用性"，不泄露 workspace 名称 / 成员。
#[derive(Debug, Clone, Serialize)]
pub struct ShareLinkPublicDto {
    pub workspace_id: String,
    pub role: String,
    pub valid: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct JoinByCodeDto {
    pub workspace_id: String,
    pub role: String,
    /// `false` = 已是成员（幂等加入），`true` = 本次新建成员。
    pub joined: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateShareLinkRequest {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<Utc>>,
    #[serde(default)]
    pub max_uses: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JoinByCodeRequest {
    pub code: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /api/workspaces/{id}/share-links` — 创建（Owner|Admin）。
///
/// 所有 body 字段都可省略（`{}` → member 角色 + 默认 7 天有效期）。
async fn create_share_link(
    State(state): State<Arc<AppState>>,
    Path(workspace_id): Path<String>,
    user: AuthUser,
    Json(body): Json<CreateShareLinkRequest>,
) -> ApiResult<Response> {
    let ws_id = Id::parse(&workspace_id).map_err(|_| not_found("workspace"))?;
    require_workspace_admin(&state, ws_id, user.id()).await?;
    let role = parse_link_role(body.role.as_deref())?;
    let row = ShareLinkRepo::new(&state.db)
        .create(NewShareLink {
            workspace_id: ws_id,
            code: ShareLinkRepo::generate_code(),
            created_by: user.id(),
            role,
            expires_at: body.expires_at,
            max_uses: body.max_uses,
        })
        .await
        .map_err(share_err)?;
    // 与 M1 的其它"创建"端点一致（`POST /api/workspaces`、`POST .../invitations`）→ 201。
    Ok((StatusCode::CREATED, Json(ShareLinkDto::from(&row))).into_response())
}

/// `GET /api/workspaces/{id}/share-links` — 列出（member 以上）。
async fn list_share_links(
    State(state): State<Arc<AppState>>,
    Path(workspace_id): Path<String>,
    user: AuthUser,
) -> ApiResult<Json<Vec<ShareLinkDto>>> {
    let ws_id = Id::parse(&workspace_id).map_err(|_| not_found("workspace"))?;
    require_workspace_member(&state, ws_id, user.id()).await?;
    let rows = ShareLinkRepo::new(&state.db)
        .list(ShareLinkFilter {
            workspace_id: Some(ws_id),
            active_only: false,
            limit: Some(200),
            ..Default::default()
        })
        .await
        .map_err(share_err)?;
    Ok(Json(rows.iter().map(ShareLinkDto::from).collect()))
}

/// `DELETE /api/workspaces/{id}/share-links/{linkId}` — 撤销（Owner|Admin）。
async fn revoke_share_link(
    State(state): State<Arc<AppState>>,
    Path((workspace_id, link_id)): Path<(String, String)>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let ws_id = Id::parse(&workspace_id).map_err(|_| not_found("workspace"))?;
    require_workspace_admin(&state, ws_id, user.id()).await?;
    let link_id = Id::parse(&link_id).map_err(|_| not_found("share_link"))?;
    let repo = ShareLinkRepo::new(&state.db);
    // workspace 归属校验：跨 workspace 撤销一律 404（不泄露存在性）。
    let link = repo.get(link_id).await.map_err(share_err)?;
    if link.workspace_id() != ws_id {
        return Err(not_found("share_link").into());
    }
    repo.revoke(link_id).await.map_err(share_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /api/share-links/{code}` — 公开元数据（无需登录）。
async fn share_link_info(
    State(state): State<Arc<AppState>>,
    Path(code): Path<String>,
) -> ApiResult<Json<ShareLinkPublicDto>> {
    let link = ShareLinkRepo::new(&state.db)
        .find_by_code(&code)
        .await
        .map_err(share_err)?
        .ok_or_else(|| not_found("share_link"))?;
    Ok(Json(ShareLinkPublicDto {
        workspace_id: link.workspace_id().to_string(),
        role: link.role().as_str().to_string(),
        valid: link.is_usable(Utc::now()),
    }))
}

/// `POST /api/share-links/join` — 用 code 加入 workspace（需登录）。
///
/// 幂等：已是成员时 `joined=false` 且**不**重复计数 `use_count`
/// （先查成员，再决定是否消耗一次使用名额）。
async fn join_by_code(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Json(body): Json<JoinByCodeRequest>,
) -> ApiResult<Json<JoinByCodeDto>> {
    let code = body.code.trim().to_owned();
    if code.is_empty() {
        return Err(Error::Validation {
            message: "code is required".into(),
            details: vec![],
        }
        .into());
    }
    let repo = ShareLinkRepo::new(&state.db);
    let link = repo
        .find_by_code(&code)
        .await
        .map_err(share_err)?
        .ok_or_else(|| not_found("share_link"))?;
    if !link.is_usable(Utc::now()) {
        return Err(Error::Validation {
            message: "share link is no longer usable".into(),
            details: vec![],
        }
        .into());
    }
    let members = MemberRepo::new(state.db.clone());
    let existing = members
        .get_for_user(link.workspace_id(), user.id())
        .await
        .is_ok();
    if !existing {
        repo.increment_use(link.id()).await.map_err(share_err)?;
        members
            .create(NewMember {
                workspace_id: link.workspace_id(),
                user_id: user.id(),
                role: link.role(),
            })
            .await
            .map_err(|e| match e {
                RepoError::Conflict => Error::MemberAlreadyExists(user.id().to_string()),
                other => internal("create member", other),
            })?;
    }
    Ok(Json(JoinByCodeDto {
        workspace_id: link.workspace_id().to_string(),
        role: link.role().as_str().to_string(),
        joined: !existing,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_link_role(raw: Option<&str>) -> Result<WorkspaceRole, Error> {
    match raw.unwrap_or("member") {
        "admin" => Ok(WorkspaceRole::Admin),
        "member" => Ok(WorkspaceRole::Member),
        other => Err(Error::Validation {
            message: format!("share link role must be admin or member, got: {other}"),
            details: vec![],
        }),
    }
}

fn share_err(e: RepoError) -> Error {
    match e {
        RepoError::NotFound => not_found("share_link"),
        RepoError::Conflict => Error::Conflict {
            message: "share link is no longer usable".into(),
        },
        RepoError::Db(msg) => Error::Database(msg),
    }
}

#[allow(clippy::needless_pass_by_value)] // 作为 `map_err` 的函数指针必须按值接收。
fn internal(what: &'static str, e: RepoError) -> Error {
    Error::Internal(format!("{what}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_role_defaults_to_member_and_rejects_owner() {
        assert_eq!(parse_link_role(None).unwrap(), WorkspaceRole::Member);
        assert_eq!(
            parse_link_role(Some("admin")).unwrap(),
            WorkspaceRole::Admin
        );
        assert!(parse_link_role(Some("owner")).is_err());
    }

    #[test]
    fn share_err_maps_conflict_and_not_found() {
        assert!(matches!(
            share_err(RepoError::NotFound),
            Error::NotFound { .. }
        ));
        assert!(matches!(
            share_err(RepoError::Conflict),
            Error::Conflict { .. }
        ));
        assert!(matches!(
            share_err(RepoError::Db("boom".into())),
            Error::Database(_)
        ));
    }

    #[test]
    fn share_link_dto_exposes_limits() {
        let row = ShareLinkRow {
            id: uuid::Uuid::new_v4(),
            workspace_id: uuid::Uuid::new_v4(),
            code: "abc1234567".into(),
            created_by: uuid::Uuid::new_v4(),
            role: "admin".into(),
            expires_at: None,
            max_uses: Some(3),
            use_count: 1,
            is_active: true,
            created_at: Utc::now(),
        };
        let dto = ShareLinkDto::from(&row);
        assert_eq!(dto.role, "admin");
        assert_eq!(dto.max_uses, Some(3));
        assert_eq!(dto.use_count, 1);
        assert!(dto.expires_at.is_none());
    }

    #[test]
    fn public_dto_hides_workspace_name() {
        let dto = ShareLinkPublicDto {
            workspace_id: Id::new().to_string(),
            role: "member".into(),
            valid: true,
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert!(json.get("name").is_none());
        assert!(json.get("code").is_none());
        assert_eq!(json["valid"], serde_json::json!(true));
    }
}
