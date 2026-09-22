//! Routes 聚合：所有 router 注册点。

use axum::{
    routing::{delete, get, patch, post, put},
    Router,
};
use std::sync::Arc;

use crate::state::AppState;

pub mod auth;
pub mod health;
pub mod invitation;
pub mod member;
pub mod openapi;
pub mod pat;
pub mod workspace;

pub use health::placeholder as placeholder_handler;

/// Build the M1 route table.
///
/// Endpoints implemented:
/// - `auth::send_code` / `verify_code` / `me` / `logout` / `cli_token`
/// - `workspace::list` / `get` / `create` / `update` / `leave`
/// - `member::list` / `update` / `delete`
/// - `invitation::create` / `list` / `revoke` / `list_for_user` / `accept` / `decline`
/// - `invitation::create_share_link` / `list_share_links` / `revoke_share_link`
/// - `invitation::share_link_info` / `join_by_code`
/// - `pat::list` / `create` / `renew_current` / `revoke`
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // ----- public health / openapi -----
        .route("/api/health", get(health::health))
        .route("/api/health/db", get(health::db_health))
        .route("/api/openapi.json", get(openapi::openapi_json))
        // ----- auth -----
        .route("/api/auth/send-code", post(auth::send_code))
        .route("/api/auth/verify-code", post(auth::verify_code))
        .route("/api/auth/me", get(auth::me))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/refresh", post(auth::not_implemented))
        .route("/api/auth/cli-token", post(auth::cli_token))
        // ----- user self -----
        // ----- workspaces + members -----
        .route("/api/workspaces", get(workspace::list).post(workspace::create))
        .route(
            "/api/workspaces/{id}",
            get(workspace::get)
                .put(workspace::update)
                .patch(workspace::update)
                .delete(auth::not_implemented), // soft delete: M2 service layer
        )
        .route("/api/workspaces/{id}/leave", post(workspace::leave))
        .route(
            "/api/workspaces/{id}/members",
            get(member::list).post(auth::not_implemented), // create = invitation
        )
        .route(
            "/api/workspaces/{id}/members/{memberId}",
            patch(member::update).delete(member::delete),
        )
        // ----- workspace invitations -----
        .route(
            "/api/workspaces/{id}/invitations",
            get(invitation::list).post(invitation::create),
        )
        .route(
            "/api/workspaces/{id}/invitations/{invitationId}",
            delete(invitation::revoke),
        )
        .route("/api/invitations", get(invitation::list_for_user))
        .route(
            "/api/invitations/{id}/accept",
            post(invitation::accept),
        )
        .route(
            "/api/invitations/{id}/decline",
            post(invitation::decline),
        )
        // ----- share links -----
        .route(
            "/api/workspaces/{id}/share-links",
            get(invitation::list_share_links).post(invitation::create_share_link),
        )
        .route(
            "/api/workspaces/{id}/share-links/{linkId}",
            delete(invitation::revoke_share_link),
        )
        .route("/api/share-links/{code}", get(invitation::share_link_info))
        .route("/api/share-links/join", post(invitation::join_by_code))
        // ----- personal access tokens -----
        .route("/api/tokens", get(pat::list).post(pat::create))
        .route("/api/tokens/current/renew", post(pat::renew_current))
        .route("/api/tokens/{id}", delete(pat::revoke))
        // ----- stubs for M2+ -----
        .route("/api/issues", get(placeholder_handler).post(placeholder_handler))
        .route("/api/issues/{id}", get(placeholder_handler))
        .route("/api/agents", get(placeholder_handler).post(placeholder_handler))
        .route("/api/runtimes", get(placeholder_handler).post(placeholder_handler))
        .route("/api/chat/sessions", get(placeholder_handler).post(placeholder_handler))
        .route("/api/inbox", get(placeholder_handler))
        .route("/api/skills", get(placeholder_handler).post(placeholder_handler))
        .route("/api/plugins", get(placeholder_handler).post(placeholder_handler))
        .route("/api/autopilots", get(placeholder_handler).post(placeholder_handler))
        .route("/api/squads", get(placeholder_handler).post(placeholder_handler))
        .route("/api/projects", get(placeholder_handler).post(placeholder_handler))
        .route("/api/comments", get(placeholder_handler).post(placeholder_handler))
        .route("/api/feature-flags", get(placeholder_handler))
        // Touch `put` to silence unused-import warnings on systems that only
        // use `.patch` for updates.
        .route(
            "/_/internal/put-ping",
            put(auth::not_implemented).patch(auth::not_implemented),
        )
}
