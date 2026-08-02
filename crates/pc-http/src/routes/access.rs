//! Access 端点：invites、board-claim、CLI auth challenges、board API keys。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/board-claim/:token", get(board_claim))
        .route("/api/board-claim/:token/claim", post(board_claim_token))
        .route("/api/bootstrap/claim", post(bootstrap_claim))
        .route("/api/cli-auth/challenges", post(cli_challenge_create))
        .route("/api/cli-auth/challenges/:id", get(cli_challenge_get))
        .route(
            "/api/cli-auth/challenges/:id/approve",
            post(cli_challenge_approve),
        )
        .route(
            "/api/cli-auth/challenges/:id/cancel",
            post(cli_challenge_cancel),
        )
        .route("/api/cli-auth/me", get(cli_auth_me))
        .route(
            "/api/board-api-keys",
            get(board_keys_list).post(board_keys_create),
        )
        .route("/api/board-api-keys/:key_id", delete(delete_board_key))
        .route("/api/cli-auth/revoke-current", post(cli_revoke_current))
        .route("/api/invites/:token", get(invites_get))
        .route("/api/invites/:token/accept", post(invites_accept))
        .route("/api/skills/available", get(skills_available))
        .route("/api/skills/index", get(skills_index))
        .route("/api/skills/:skill_name", get(skill_get))
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct ClaimBody {
    user_id: Option<String>,
    company_id: Option<String>,
}

async fn board_claim(State(_state): State<AppState>, Path(token): Path<String>) -> Json<Value> {
    Json(json!({
        "token": token,
        "kind": "board-claim",
        "valid": true
    }))
}

async fn board_claim_token(
    State(_state): State<AppState>,
    Path(token): Path<String>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = token;
    (
        StatusCode::OK,
        Json(json!({
            "claimed": true,
            "sessionToken": "tok_claimed_in_rust_build",
            "expiresAt": chrono::Utc::now() + chrono::Duration::days(7)
        })),
    )
}

async fn bootstrap_claim(
    State(_state): State<AppState>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "claimed": true,
            "userId": "u_bootstrap",
            "sessionToken": "tok_bootstrap"
        })),
    )
}

async fn cli_challenge_create(
    State(_state): State<AppState>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (
        StatusCode::CREATED,
        Json(json!({
            "id": "cli_challenge_new",
            "code": "ABCD-1234",
            "verificationUrl": "https://example.com/verify",
            "expiresAt": chrono::Utc::now() + chrono::Duration::minutes(5)
        })),
    )
}

async fn cli_challenge_get(State(_state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    Json(json!({
        "id": id,
        "status": "pending"
    }))
}

async fn cli_challenge_approve(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({ "id": id, "status": "approved" })),
    )
}

async fn cli_challenge_cancel(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({ "id": id, "status": "cancelled" })),
    )
}

async fn cli_auth_me(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({
        "actor": "anonymous",
        "roles": []
    }))
}

async fn board_keys_list(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({ "items": [] }))
}

async fn board_keys_create(
    State(_state): State<AppState>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (
        StatusCode::CREATED,
        Json(json!({
            "id": "key_new",
            "prefix": "tok_",
            "name": "new-key",
            "createdAt": chrono::Utc::now()
        })),
    )
}

async fn delete_board_key(
    State(_state): State<AppState>,
    Path(key_id): Path<String>,
) -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        Json(json!({ "id": key_id, "deleted": true })),
    )
}

async fn cli_revoke_current(State(_state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "revoked": true })))
}

async fn invites_get(State(_state): State<AppState>, Path(token): Path<String>) -> Json<Value> {
    Json(json!({
        "token": token,
        "status": "active",
        "companyId": null
    }))
}

async fn invites_accept(
    State(_state): State<AppState>,
    Path(token): Path<String>,
    Json(_body): Json<ClaimBody>,
) -> impl IntoResponse {
    let _ = token;
    (StatusCode::OK, Json(json!({ "accepted": true })))
}

async fn skills_available(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({ "items": [] }))
}

async fn skills_index(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({ "index": {} }))
}

async fn skill_get(State(_state): State<AppState>, Path(skill_name): Path<String>) -> Json<Value> {
    Json(json!({
        "name": skill_name,
        "description": null,
        "manifest": null
    }))
}
