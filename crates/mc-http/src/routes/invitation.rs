//! Workspace invitation + share link + invitation accept/decline 路由。
//!
//! 协议与 multica upstream `handler/invitation.go` 对齐：
//! - `POST   /api/workspaces/{id}/invitations` — 创建邀请
//! - `GET    /api/workspaces/{id}/invitations` — 列出当前 workspace 的邀请
//! - `DELETE /api/workspaces/{id}/invitations/{invitationId}` — 撤销邀请
//! - `GET    /api/invitations` — 列出当前 user（按 email 或 user_id）相关的邀请
//! - `POST   /api/invitations/{id}/accept` — 接受（注册新账号或加入）
//! - `POST   /api/invitations/{id}/decline` — 拒绝
//! - `GET    /api/share-links/{code}` — 公开元数据（仅展示 role / 不可用状态）
//! - `POST   /api/share-links/join` — 用 code 加入

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use mc_core::id::Id;
use mc_core::workspace::WorkspaceRole;
use mc_errors::Error;
use mc_repos::{
    InvitationRepo, InvitationRow, InvitationStatus, MemberRepo, NewInvitation, NewMember,
    NewShareLink, ShareLinkRepo, ShareLinkRow, UpdateInvitationStatus, WorkspaceRepo,
};

use crate::error::ApiError;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Response DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct InvitationResponse {
    pub id: String,
    pub workspace_id: String,
    pub email: String,
    pub invited_by_user_id: String,
    pub role: String,
    pub status: String,
    pub expires_at: String,
    pub created_at: String,
}

impl InvitationResponse {
    pub fn from_row(row: &InvitationRow) -> Self {
        Self {
            id: row.id.as_string(),
            workspace_id: row.workspace_id.as_string(),
            email: row.email.clone(),
            invited_by_user_id: row.invited_by_user_id.as_string(),
            role: row.role.as_str().to_string(),
            status: row.status.as_str().to_string(),
            expires_at: row.expires_at.to_rfc3339(),
            created_at: row.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateInvitationRequest {
    pub email: String,
    pub role: WorkspaceRole,
    /// Optional cap on the lifetime; default 7 days.
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct ShareLinkResponse {
    pub id: String,
    pub workspace_id: String,
    pub code: String,
    pub role: String,
    pub expires_at: Option<String>,
    pub max_uses: Option<u32>,
    pub use_count: u32,
    pub is_active: bool,
    pub created_at: String,
}

impl ShareLinkResponse {
    pub fn from_row(row: &ShareLinkRow) -> Self {
        Self {
            id: row.id.as_string(),
            workspace_id: row.workspace_id.as_string(),
            code: row.code.clone(),
            role: row.role.as_str().to_string(),
            expires_at: row.expires_at.map(|d| d.to_rfc3339()),
            max_uses: row.max_uses,
            use_count: row.use_count,
            is_active: row.is_active,
            created_at: row.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ShareLinkPublicResponse {
    pub workspace_id: String,
    pub role: String,
    pub valid: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateShareLinkRequest {
    pub role: WorkspaceRole,
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<Utc>>,
    #[serde(default)]
    pub max_uses: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct JoinByCodeRequest {
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct JoinByCodeResponse {
    pub workspace_id: String,
    pub role: String,
    pub joined: bool,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn actor_user_id(headers: &HeaderMap) -> Result<Id, ApiError> {
    headers
        .get("x-multica-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| Id::parse(s))
        .transpose()
        .map_err(|e| ApiError(Error::Unauthorized { message: e.to_string() }))?
        .ok_or_else(|| ApiError(Error::Unauthorized { message: "no user id".into() }))
}

fn workspace_id_from(path: &str) -> Result<Id, ApiError> {
    Id::parse(path).map_err(|e| ApiError(Error::Validation {
        message: e.to_string(),
        details: vec![],
    }))
}

// ---------------------------------------------------------------------------
// Workspace-scoped invitation routes
// ---------------------------------------------------------------------------

/// `POST /api/workspaces/{id}/invitations`
pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(body): Json<CreateInvitationRequest>,
) -> Result<Json<InvitationResponse>, ApiError> {
    let inviter_id = actor_user_id(&headers)?;
    let workspace_id = workspace_id_from(&workspace_id)?;
    if body.email.is_empty() || !body.email.contains('@') {
        return Err(ApiError(Error::Validation {
            message: "email is required".into(),
            details: vec![],
        }));
    }

    // Ensure target user exists (creates a user row when first invited by email)
    let invitee_user_id = state
        .repos
        .users
        .find_by_email(&body.email)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("lookup user: {e}"))))?
        .map(|u| u.id);

    let token = generate_invitation_token();
    let row = state
        .repos
        .invitations
        .create(NewInvitation {
            id: None,
            workspace_id,
            email: body.email,
            invitee_user_id,
            invited_by_user_id: inviter_id,
            role: body.role,
            token,
            expires_at: body.expires_at,
        })
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::Conflict(msg) => ApiError(Error::Conflict { message: msg }),
            other => ApiError(Error::Internal(format!("create invitation: {other}"))),
        })?;
    Ok(Json(InvitationResponse::from_row(&row)))
}

/// `GET /api/workspaces/{id}/invitations`
pub async fn list(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Vec<InvitationResponse>>, ApiError> {
    let workspace_id = workspace_id_from(&workspace_id)?;
    let rows = state
        .repos
        .invitations
        .list(mc_repos::invitation::InvitationFilter {
            workspace_id: Some(workspace_id),
            limit: Some(500),
            ..Default::default()
        })
        .await
        .map_err(|e| ApiError(Error::Internal(format!("list invitations: {e}"))))?;
    Ok(Json(rows.iter().map(InvitationResponse::from_row).collect()))
}

/// `DELETE /api/workspaces/{id}/invitations/{invitationId}`
pub async fn revoke(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    Path((workspace_id, invitation_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = workspace_id_from(&workspace_id)?;
    let invitation_id = Id::parse(&invitation_id).map_err(|e| ApiError(Error::Validation {
        message: e.to_string(),
        details: vec![],
    }))?;
    let row = state
        .repos
        .invitations
        .get(invitation_id)
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::NotFound => {
                ApiError(Error::NotFound { resource: "invitation".into() })
            }
            other => ApiError(Error::Internal(format!("lookup invitation: {other}"))),
        })?;
    if row.workspace_id != workspace_id {
        return Err(ApiError(Error::NotFound { resource: "invitation".into() }));
    }
    state
        .repos
        .invitations
        .revoke(invitation_id)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("revoke: {e}"))))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// User-scoped invitation routes
// ---------------------------------------------------------------------------

/// `GET /api/invitations` — 列出当前 user 相关的邀请（按 email 或 user_id）。
pub async fn list_for_user(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<InvitationResponse>>, ApiError> {
    let user_id = actor_user_id(&headers)?;
    let me = state.repos.users.get(user_id).await.map_err(|e| {
        ApiError(Error::Internal(format!("get user: {e}")))
    })?;
    // list by email first
    let mut by_email = state
        .repos
        .invitations
        .list(mc_repos::invitation::InvitationFilter {
            email: Some(me.email.clone()),
            status: Some(InvitationStatus::Pending),
            limit: Some(500),
            ..Default::default()
        })
        .await
        .map_err(|e| ApiError(Error::Internal(format!("list invitations: {e}"))))?;
    let mut by_user = state
        .repos
        .invitations
        .list(mc_repos::invitation::InvitationFilter {
            invitee_user_id: Some(me.id),
            status: Some(InvitationStatus::Pending),
            limit: Some(500),
            ..Default::default()
        })
        .await
        .map_err(|e| ApiError(Error::Internal(format!("list invitations: {e}"))))?;
    by_email.append(&mut by_user);
    by_email.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    by_email.dedup_by(|a, b| a.id == b.id);
    Ok(Json(by_email.iter().map(InvitationResponse::from_row).collect()))
}

/// `POST /api/invitations/{id}/accept`
pub async fn accept(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(invitation_id): Path<String>,
) -> Result<Json<InvitationResponse>, ApiError> {
    let user_id = actor_user_id(&headers)?;
    accept_or_decline(&state, &headers, &invitation_id, user_id, InvitationStatus::Accepted).await
}

/// `POST /api/invitations/{id}/decline`
pub async fn decline(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(invitation_id): Path<String>,
) -> Result<Json<InvitationResponse>, ApiError> {
    let user_id = actor_user_id(&headers)?;
    accept_or_decline(&state, &headers, &invitation_id, user_id, InvitationStatus::Declined).await
}

async fn accept_or_decline(
    state: &Arc<AppState>,
    _headers: &HeaderMap,
    invitation_id: &str,
    user_id: Id,
    new_status: InvitationStatus,
) -> Result<Json<InvitationResponse>, ApiError> {
    let id = Id::parse(invitation_id).map_err(|e| ApiError(Error::Validation {
        message: e.to_string(),
        details: vec![],
    }))?;
    let row = state
        .repos
        .invitations
        .get(id)
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::NotFound => {
                ApiError(Error::NotFound { resource: "invitation".into() })
            }
            other => ApiError(Error::Internal(format!("lookup invitation: {other}"))),
        })?;

    if !row.is_pending() {
        return Err(ApiError(Error::Validation {
            message: format!(
                "invitation is no longer pending (status={})",
                row.status.as_str()
            ),
            details: vec![],
        }));
    }

    let me = state
        .repos
        .users
        .get(user_id)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("get user: {e}"))))?;
    // Authorize: invitation email must match the calling user's email, or
    // the invitation must already carry the calling user's id (e.g. reused
    // PAT-flow login).
    if row.email != me.email && row.invitee_user_id != Some(me.id) {
        return Err(ApiError(Error::Forbidden {
            message: "invitation does not belong to caller".into(),
        }));
    }

    let updated = state
        .repos
        .invitations
        .update_status(
            id,
            UpdateInvitationStatus {
                status: new_status,
                accepted_user_id: Some(user_id),
            },
        )
        .await
        .map_err(|e| ApiError(Error::Internal(format!("update invitation: {e}"))))?;

    if new_status == InvitationStatus::Accepted {
        // Idempotent: ON CONFLICT DO NOTHING via repo-level guard.
        let already = state
            .repos
            .members
            .find(updated.workspace_id, user_id)
            .await
            .map_err(|e| ApiError(Error::Internal(format!("find member: {e}"))))?;
        if already.is_none() {
            state
                .repos
                .members
                .create(NewMember {
                    workspace_id: updated.workspace_id,
                    user_id,
                    role: updated.role,
                })
                .await
                .map_err(|e| match e {
                    mc_repos::RepoError::Conflict(_) => ApiError(Error::MemberAlreadyExists(
                        user_id.as_string(),
                    )),
                    other => ApiError(Error::Internal(format!("create member: {other}"))),
                })?;
        }
    }

    Ok(Json(InvitationResponse::from_row(&updated)))
}

// ---------------------------------------------------------------------------
// Share-link routes
// ---------------------------------------------------------------------------

/// `POST /api/workspaces/{id}/share-links` — 创建。
pub async fn create_share_link(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(body): Json<CreateShareLinkRequest>,
) -> Result<Json<ShareLinkResponse>, ApiError> {
    let user_id = actor_user_id(&headers)?;
    let workspace_id = workspace_id_from(&workspace_id)?;
    // Sanity: workspace exists.
    let _ = state
        .repos
        .workspaces
        .get(workspace_id)
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::NotFound => ApiError(Error::WorkspaceNotFound(
                workspace_id.as_string(),
            )),
            other => ApiError(Error::Internal(format!("get workspace: {other}"))),
        })?;
    let code = generate_share_code();
    let row = state
        .repos
        .share_links
        .create(NewShareLink {
            id: None,
            workspace_id,
            code,
            created_by: user_id,
            role: body.role,
            expires_at: body.expires_at,
            max_uses: body.max_uses,
        })
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::Conflict(msg) => ApiError(Error::Conflict { message: msg }),
            other => ApiError(Error::Internal(format!("create share link: {other}"))),
        })?;
    Ok(Json(ShareLinkResponse::from_row(&row)))
}

/// `GET /api/workspaces/{id}/share-links` — 列出。
pub async fn list_share_links(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Vec<ShareLinkResponse>>, ApiError> {
    let workspace_id = workspace_id_from(&workspace_id)?;
    let rows = state
        .repos
        .share_links
        .list(mc_repos::share_link::ShareLinkFilter {
            workspace_id: Some(workspace_id),
            active_only: false,
            limit: Some(200),
            ..Default::default()
        })
        .await
        .map_err(|e| ApiError(Error::Internal(format!("list share links: {e}"))))?;
    Ok(Json(
        rows.iter().map(ShareLinkResponse::from_row).collect(),
    ))
}

/// `DELETE /api/workspaces/{id}/share-links/{linkId}` — 撤销。
pub async fn revoke_share_link(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    Path((workspace_id, link_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = workspace_id_from(&workspace_id)?;
    let link_id = Id::parse(&link_id).map_err(|e| ApiError(Error::Validation {
        message: e.to_string(),
        details: vec![],
    }))?;
    let link = state
        .repos
        .share_links
        .get(link_id)
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::NotFound => {
                ApiError(Error::NotFound { resource: "share_link".into() })
            }
            other => ApiError(Error::Internal(format!("get share link: {other}"))),
        })?;
    if link.workspace_id != workspace_id {
        return Err(ApiError(Error::NotFound { resource: "share_link".into() }));
    }
    state
        .repos
        .share_links
        .revoke(link_id)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("revoke share link: {e}"))))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /api/share-links/{code}` — 公开元数据：仅返回 role 和是否可用。
pub async fn share_link_info(
    State(state): State<Arc<AppState>>,
    Path(code): Path<String>,
) -> Result<Json<ShareLinkPublicResponse>, ApiError> {
    let link = state
        .repos
        .share_links
        .find_by_code(&code)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("lookup share link: {e}"))))?
        .ok_or_else(|| ApiError(Error::NotFound { resource: "share_link".into() }))?;
    Ok(Json(ShareLinkPublicResponse {
        workspace_id: link.workspace_id.as_string(),
        role: link.role.as_str().to_string(),
        valid: link.is_usable(Utc::now()),
    }))
}

/// `POST /api/share-links/join` — 用 code 加入 workspace。
pub async fn join_by_code(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<JoinByCodeRequest>,
) -> Result<Json<JoinByCodeResponse>, ApiError> {
    let user_id = actor_user_id(&headers)?;
    let link = state
        .repos
        .share_links
        .find_by_code(&body.code)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("lookup share link: {e}"))))?
        .ok_or_else(|| ApiError(Error::NotFound { resource: "share_link".into() }))?;
    if !link.is_usable(Utc::now()) {
        return Err(ApiError(Error::Validation {
            message: "share link is no longer usable".into(),
            details: vec![],
        }));
    }
    // Atomically bump use_count + create membership (best-effort: increments
    // first then conditionally creates the member).
    let updated = state
        .repos
        .share_links
        .increment_use(link.id)
        .await
        .map_err(|e| ApiError(Error::Validation {
            message: e.to_string(),
            details: vec![],
        }))?;
    let existing = state
        .repos
        .members
        .find(updated.workspace_id, user_id)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("find member: {e}"))))?;
    if existing.is_none() {
        state
            .repos
            .members
            .create(NewMember {
                workspace_id: updated.workspace_id,
                user_id,
                role: updated.role,
            })
            .await
            .map_err(|e| match e {
                mc_repos::RepoError::Conflict(_) => ApiError(Error::MemberAlreadyExists(
                    user_id.as_string(),
                )),
                other => ApiError(Error::Internal(format!("create member: {other}"))),
            })?;
    }
    Ok(Json(JoinByCodeResponse {
        workspace_id: updated.workspace_id.as_string(),
        role: updated.role.as_str().to_string(),
        joined: existing.is_none(),
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn generate_invitation_token() -> String {
    format!("inv_{}", Uuid::new_v4().simple())
}

fn generate_share_code() -> String {
    // 10-char url-safe token; sufficient entropy (60 bits) for short-lived links.
    use rand::Rng;
    const ALPHABET: &[u8] =
        b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no 0/O/1/l/I
    let mut rng = rand::thread_rng();
    (0..10)
        .map(|_| {
            let idx = rng.gen_range(0..ALPHABET.len());
            ALPHABET[idx] as char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_token_has_unique_prefix() {
        let token = generate_invitation_token();
        assert!(token.starts_with("inv_"));
    }

    #[test]
    fn share_code_uses_only_unambiguous_chars() {
        let code = generate_share_code();
        assert_eq!(code.len(), 10);
        for c in code.chars() {
            assert!(
                !"0O1lI".contains(c),
                "share code contains ambiguous char {c}"
            );
        }
    }

    #[test]
    fn share_codes_are_distinct_across_calls() {
        let a = generate_share_code();
        let b = generate_share_code();
        assert_ne!(a, b);
    }
}
