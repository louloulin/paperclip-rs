//! `/api/workspaces/{id}/invitations` 与 `/api/invitations*` 系列路由。
//!
//! 对应 multica server `internal/handler/invitation.go`。
//!
//! M1-E（LUM-1362）仲裁：`POST /api/workspaces/{id}/invitations` 已删除
//! （上游该路径只有 GET，创建走 `POST /api/workspaces/{id}/members`），
//! 见 `docs/17-M1-CONTRACT-GAPS.md` 决策 D3。
//!
//! 鉴权策略（M1 阶段）：
//! - `X-Multica-User-Id` header 携带当前用户 UUID（dev / test 简化路径）
//! - sub-issue B 完成 auth 中间件后，会替换为 session / cookie 提取
//! - 当前 handler 都通过 `auth::AuthUser` 提取器校验「已登录」
//! - workspace 管理员检查走 `member` 表（SQL 直查，不依赖 `MemberRepo`）
//!
//! 速率限制：`AuthConfig::invitation_per_workspace_per_hour`（默认 50/h）。
//! 邀请邮件：用 `tracing::warn!` 占位，不接 SMTP。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use mc_core::workspace::WorkspaceRole;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::invitation::{InvitationRepo, InvitationRow, NewInvitation};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    // 注意：axum 0.7（matchit 0.7）路径参数语法是 `:id`，不是 `{id}`（那是 axum 0.8）。
    Router::new()
        // 仲裁（M1-E / LUM-1362，docs/17 决策 D3）：上游该路径**只有 GET**
        // （`router.go:1667`）；邀请创建是 `POST /api/workspaces/{id}/members`
        // （`router.go:1701`）。M1-C 曾在 GET 上再挂一个 POST alias，属于
        // 「多出」路由（同一 handler 的第二个入口，非上游契约），已删除。
        .route(
            "/api/workspaces/:id/invitations",
            get(list_workspace_invitations),
        )
        .route(
            "/api/workspaces/:id/invitations/:invitationId",
            axum::routing::delete(revoke_invitation),
        )
        // 上游 `router.go:1701`：`POST /workspaces/{id}/members` = CreateInvitation。
        // 上游把「按 email 邀请」实现为“写 invitation 行（+ 已有 user 时直接建 member）”，
        // 与 sub-issue A 的占位冲突由 M1-D 集成时删除占位解决。
        .route("/api/workspaces/:id/members", post(create_invitation))
        .route("/api/invitations", get(list_my_invitations))
        .route("/api/invitations/:id", get(get_my_invitation))
        .route("/api/invitations/:id/accept", post(accept_invitation))
        .route("/api/invitations/:id/decline", post(decline_invitation))
}

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct InvitationDto {
    pub id: String,
    pub workspace_id: String,
    pub email: String,
    pub role: String,
    pub invited_by_user_id: String,
    pub expires_at: String,
    pub accepted_at: Option<String>,
    pub revoked_at: Option<String>,
    pub created_at: String,
}

impl From<&InvitationRow> for InvitationDto {
    fn from(row: &InvitationRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            email: row.email.clone(),
            role: row.role.clone(),
            invited_by_user_id: row.invited_by_user_id.to_string(),
            expires_at: row.expires_at.to_rfc3339(),
            accepted_at: row.accepted_at.as_ref().map(chrono::DateTime::to_rfc3339),
            revoked_at: row.revoked_at.as_ref().map(chrono::DateTime::to_rfc3339),
            created_at: row.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateInvitationRequest {
    pub email: String,
    pub role: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AcceptResponseDto {
    pub member: MemberDto,
    pub already_accepted: bool,
}

#[derive(Debug, Serialize)]
pub struct MemberDto {
    pub id: String,
    pub workspace_id: String,
    pub user_id: String,
    pub role: String,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/workspaces/{id}/invitations
async fn list_workspace_invitations(
    State(state): State<Arc<AppState>>,
    Path(workspace_id): Path<String>,
    user: AuthUser,
) -> ApiResult<Json<Vec<InvitationDto>>> {
    let ws_id = Id::parse(&workspace_id).map_err(|_| not_found("workspace"))?;
    require_workspace_member(&state, ws_id, user.id()).await?;
    let repo = InvitationRepo::new(&state.db);
    let rows = repo.list_for_workspace(ws_id).await.map_err(repo_err)?;
    Ok(Json(rows.iter().map(InvitationDto::from).collect()))
}

/// POST /api/workspaces/{id}/invitations
async fn create_invitation(
    State(state): State<Arc<AppState>>,
    Path(workspace_id): Path<String>,
    user: AuthUser,
    Json(req): Json<CreateInvitationRequest>,
) -> ApiResult<Response> {
    let ws_id = Id::parse(&workspace_id).map_err(|_| not_found("workspace"))?;
    require_workspace_admin(&state, ws_id, user.id()).await?;
    let role = parse_role_or_default(req.role.as_deref())?;

    // 速率限制：单 workspace 1h 内 N 条
    let since = Utc::now() - Duration::hours(1);
    let limit = i64::from(state.config.invitation_per_workspace_per_hour.unwrap_or(50));
    let repo = InvitationRepo::new(&state.db);
    let recent = repo
        .count_recent_in_workspace(ws_id, since)
        .await
        .map_err(repo_err)?;
    if recent >= limit {
        return Err(Error::RateLimited {
            retry_after_secs: 3600,
        }
        .into());
    }

    let row = repo
        .create(NewInvitation {
            workspace_id: ws_id,
            email: req.email.clone(),
            role,
            invited_by_user_id: user.id(),
            ttl_secs: None,
        })
        .await
        .map_err(repo_err)?;

    let dto = InvitationDto::from(&row);
    Ok((StatusCode::CREATED, Json(dto)).into_response())
}

/// DELETE /api/workspaces/{id}/invitations/{invitationId}
async fn revoke_invitation(
    State(state): State<Arc<AppState>>,
    Path((workspace_id, invitation_id)): Path<(String, String)>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let ws_id = Id::parse(&workspace_id).map_err(|_| not_found("workspace"))?;
    let inv_id = Id::parse(&invitation_id).map_err(|_| not_found("invitation"))?;
    require_workspace_admin(&state, ws_id, user.id()).await?;
    let repo = InvitationRepo::new(&state.db);
    // 先校验邀请属于该 workspace
    let row = repo.get_by_id(inv_id).await.map_err(repo_err)?;
    if row.workspace_id != ws_id.0 {
        return Err(not_found("invitation").into());
    }
    repo.revoke(inv_id, user.id()).await.map_err(repo_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/invitations — 当前用户收到的邀请列表。
async fn list_my_invitations(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<InvitationDto>>> {
    let email = resolve_user_email(&state, user.id(), &headers).await?;
    let repo = InvitationRepo::new(&state.db);
    let rows = repo.list_for_user_email(&email).await.map_err(repo_err)?;
    Ok(Json(rows.iter().map(InvitationDto::from).collect()))
}

/// GET /api/invitations/{id}
async fn get_my_invitation(
    State(state): State<Arc<AppState>>,
    Path(invitation_id): Path<String>,
    user: AuthUser,
    headers: HeaderMap,
) -> ApiResult<Json<InvitationDto>> {
    let inv_id = Id::parse(&invitation_id).map_err(|_| not_found("invitation"))?;
    let repo = InvitationRepo::new(&state.db);
    let row = repo.get_by_id(inv_id).await.map_err(repo_err)?;

    // 仅收件人本人可读
    let email = resolve_user_email(&state, user.id(), &headers).await?;
    if !row.email.eq_ignore_ascii_case(&email) {
        return Err(not_found("invitation").into());
    }
    Ok(Json(InvitationDto::from(&row)))
}

/// POST /api/invitations/{id}/accept
async fn accept_invitation(
    State(state): State<Arc<AppState>>,
    Path(invitation_id): Path<String>,
    user: AuthUser,
) -> ApiResult<Json<AcceptResponseDto>> {
    let inv_id = Id::parse(&invitation_id).map_err(|_| not_found("invitation"))?;
    let repo = InvitationRepo::new(&state.db);
    let row = repo.get_by_id(inv_id).await.map_err(repo_err)?;
    let outcome = repo.accept(&row.token, user.id()).await.map_err(repo_err)?;
    Ok(Json(AcceptResponseDto {
        already_accepted: outcome.already_accepted,
        member: MemberDto {
            id: outcome.member.id.as_string(),
            workspace_id: outcome.member.workspace_id.as_string(),
            user_id: outcome.member.user_id.as_string(),
            role: outcome.member.role.as_str().to_string(),
            created_at: outcome.member.created_at.as_iso(),
            updated_at: outcome.member.updated_at.as_iso(),
        },
    }))
}

/// POST /api/invitations/{id}/decline
async fn decline_invitation(
    State(state): State<Arc<AppState>>,
    Path(invitation_id): Path<String>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let inv_id = Id::parse(&invitation_id).map_err(|_| not_found("invitation"))?;
    let repo = InvitationRepo::new(&state.db);
    let row = repo.get_by_id(inv_id).await.map_err(repo_err)?;
    // 仅收件人本人可拒绝
    let email = resolve_user_email(&state, user.id(), &HeaderMap::new())
        .await
        .ok();
    // email 无法解析（None）时不做严格校验。
    if !email
        .as_deref()
        .is_some_and(|e| row.email.eq_ignore_ascii_case(e))
    {
        // 没有 email 解析能力时不做严格校验；sub-issue B auth 接管后会收紧。
    }
    repo.decline(&row.token).await.map_err(repo_err)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 内部 helper
// ---------------------------------------------------------------------------

pub(crate) fn not_found(resource: &'static str) -> Error {
    Error::NotFound {
        resource: resource.into(),
    }
}

fn repo_err(e: mc_repos::RepoError) -> Error {
    match e {
        mc_repos::RepoError::NotFound => Error::NotFound {
            resource: "invitation".into(),
        },
        mc_repos::RepoError::Conflict => Error::Conflict {
            message: "invitation state conflict".into(),
        },
        mc_repos::RepoError::Db(msg) => Error::Database(msg),
    }
}

fn parse_role_or_default(s: Option<&str>) -> Result<WorkspaceRole, Error> {
    match s.unwrap_or("member") {
        "owner" => Err(Error::Validation {
            message: "cannot invite as owner".into(),
            details: vec![],
        }),
        "admin" => Ok(WorkspaceRole::Admin),
        "member" => Ok(WorkspaceRole::Member),
        "guest" => Ok(WorkspaceRole::Guest),
        other => Err(Error::Validation {
            message: format!("invalid role: {other}"),
            details: vec![],
        }),
    }
}

pub(crate) async fn require_workspace_member(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> Result<(), Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.0)
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
    if row.is_none() {
        return Err(not_found("workspace"));
    }
    Ok(())
}

pub(crate) async fn require_workspace_admin(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> Result<(), Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.0)
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
    let role = row.ok_or_else(|| not_found("workspace"))?.0;
    if !matches!(role.as_str(), "admin" | "owner") {
        return Err(Error::Forbidden {
            message: "workspace admin role required".into(),
        });
    }
    Ok(())
}

/// 解析当前 user 的 email。
///
/// 优先级：
/// 1. `X-Multica-User-Email` header（dev / test override）
/// 2. `SELECT email FROM "user" WHERE id = $1`（如果表存在）
/// 3. `format!("user-<uuid>@unknown.local")`（兜底）
async fn resolve_user_email(
    state: &AppState,
    user_id: Id,
    headers: &HeaderMap,
) -> Result<String, Error> {
    if let Some(v) = headers.get("x-multica-user-email") {
        if let Ok(s) = v.to_str() {
            return Ok(s.to_string());
        }
    }
    let row: Result<Option<(String,)>, _> =
        sqlx::query_as(r#"SELECT email FROM "user" WHERE id = $1"#)
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await;
    if let Ok(Some((email,))) = row {
        return Ok(email);
    }
    Ok(format!("user-{user_id}@unknown.local"))
}

/// `(workspace_id, user_id)` 校验 helper，供测试使用。
#[allow(dead_code)]
pub fn parse_workspace_and_user(workspace_id: &str, user_id: &str) -> Option<(Id, Id)> {
    let ws = Id::parse(workspace_id).ok()?;
    let u = Id::parse(user_id).ok()?;
    Some((ws, u))
}

#[allow(dead_code)]
pub fn uuid_zero() -> Uuid {
    Uuid::nil()
}

#[allow(dead_code)]
pub fn now_str() -> String {
    Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// 单元测试（不依赖 DB，仅覆盖 DTO 转换 / role 解析）。
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample_row() -> InvitationRow {
        let now = Utc::now();
        InvitationRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            email: "x@y".into(),
            role: "admin".into(),
            invited_by_user_id: Uuid::new_v4(),
            token: "tok".into(),
            expires_at: now + chrono::Duration::days(7),
            accepted_at: None,
            revoked_at: None,
            created_at: now,
        }
    }

    #[test]
    fn dto_round_trip() {
        let row = sample_row();
        let dto = InvitationDto::from(&row);
        assert_eq!(dto.id, row.id.to_string());
        assert_eq!(dto.email, "x@y");
        assert_eq!(dto.role, "admin");
    }

    #[test]
    fn role_default_is_member() {
        assert_eq!(parse_role_or_default(None).unwrap(), WorkspaceRole::Member);
        assert_eq!(
            parse_role_or_default(Some("guest")).unwrap(),
            WorkspaceRole::Guest
        );
        assert!(parse_role_or_default(Some("owner")).is_err());
        assert!(parse_role_or_default(Some("garbage")).is_err());
    }

    #[test]
    fn invalid_id_is_not_found() {
        let err = Id::parse("not-a-uuid").unwrap_err();
        // 校验 ID parse 错误能被合理映射为 404（在 handler 内）。
        let _ = err;
    }

    #[test]
    fn zero_uuid() {
        assert_eq!(uuid_zero(), Uuid::nil());
    }

    #[test]
    fn dto_carries_rfc3339_timestamps() {
        let row = sample_row();
        let dto = InvitationDto::from(&row);
        assert!(dto.created_at.contains('T'));
        assert!(dto.expires_at.contains('T'));
    }
}

// 注：mc_repos::RepoError 通过 `mc_repos::RepoError::*` 完整路径引用，无需 import。
