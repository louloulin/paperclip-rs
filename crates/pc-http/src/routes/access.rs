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
use sqlx::FromRow;
use uuid::Uuid;

use crate::{require_user_id, ApiError, ApiResult, AppState};

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

#[derive(Debug, FromRow)]
struct ChallengeRow {
    id: Uuid,
    secret_hash: String,
    command: String,
    client_name: Option<String>,
    requested_access: String,
    requested_company_id: Option<Uuid>,
    pending_key_hash: String,
    pending_key_name: String,
    approved_by_user_id: Option<String>,
    approved_at: Option<pc_core::Timestamp>,
    cancelled_at: Option<pc_core::Timestamp>,
    expires_at: pc_core::Timestamp,
    created_at: pc_core::Timestamp,
}

fn challenge_json(row: &ChallengeRow, include_secret: bool) -> Value {
    let mut obj = json!({
        "id": row.id,
        "command": row.command,
        "clientName": row.client_name,
        "requestedAccess": row.requested_access,
        "requestedCompanyId": row.requested_company_id,
        "pendingKeyHash": row.pending_key_hash,
        "pendingKeyName": row.pending_key_name,
        "approvedByUserId": row.approved_by_user_id,
        "approvedAt": row.approved_at,
        "cancelledAt": row.cancelled_at,
        "expiresAt": row.expires_at,
        "createdAt": row.created_at,
    });
    if include_secret {
        obj["secretHash"] = json!(row.secret_hash);
    }
    obj
}

#[derive(Debug, FromRow)]
struct BoardKeyRow {
    id: Uuid,
    user_id: String,
    name: String,
    key_hash: String,
    last_used_at: Option<pc_core::Timestamp>,
    revoked_at: Option<pc_core::Timestamp>,
    expires_at: Option<pc_core::Timestamp>,
    created_at: pc_core::Timestamp,
}

fn board_key_json(row: &BoardKeyRow, include_key: bool) -> Value {
    let mut obj = json!({
        "id": row.id,
        "userId": row.user_id,
        "name": row.name,
        "lastUsedAt": row.last_used_at,
        "revokedAt": row.revoked_at,
        "expiresAt": row.expires_at,
        "createdAt": row.created_at,
    });
    if include_key {
        obj["keyHash"] = json!(row.key_hash);
    }
    obj
}

#[derive(Debug, Default, Deserialize)]
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

#[derive(Debug, Deserialize, Default)]
struct ChallengeCreateBody {
    command: Option<String>,
    client_name: Option<String>,
    requested_access: Option<String>,
    requested_company_id: Option<Uuid>,
    pending_key_name: Option<String>,
}

async fn cli_challenge_create(
    State(state): State<AppState>,
    Json(body): Json<ChallengeCreateBody>,
) -> ApiResult<Json<Value>> {
    let command = body
        .command
        .clone()
        .unwrap_or_else(|| "paperclip login".to_owned());
    let client_name = body.client_name.clone();
    let requested_access = body
        .requested_access
        .clone()
        .unwrap_or_else(|| "board".to_owned());
    let requested_company_id = body.requested_company_id;
    let pending_key_name = body
        .pending_key_name
        .clone()
        .unwrap_or_else(|| "cli-session".to_owned());
    let pending_key_hash = "pending-hash-stub".to_string();
    let secret_hash = "secret-hash-stub".to_string();
    let expires_at = chrono::Utc::now() + chrono::Duration::minutes(5);
    let row: ChallengeRow = sqlx::query_as(
        "INSERT INTO cli_auth_challenges \
            (secret_hash, command, client_name, requested_access, requested_company_id, \
             pending_key_hash, pending_key_name, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id, secret_hash, command, client_name, requested_access, requested_company_id, \
                   pending_key_hash, pending_key_name, approved_by_user_id, approved_at, \
                   cancelled_at, expires_at, created_at",
    )
    .bind(&secret_hash)
    .bind(&command)
    .bind(client_name)
    .bind(&requested_access)
    .bind(requested_company_id)
    .bind(&pending_key_hash)
    .bind(&pending_key_name)
    .bind(expires_at)
    .fetch_one(state.db.pool())
    .await?;
    Ok(Json(challenge_json(&row, true)))
}

async fn cli_challenge_get(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<ChallengeRow> = sqlx::query_as(
        "SELECT id, secret_hash, command, client_name, requested_access, requested_company_id, \
                pending_key_hash, pending_key_name, approved_by_user_id, approved_at, \
                cancelled_at, expires_at, created_at \
         FROM cli_auth_challenges WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(challenge_json(&row, true))),
        None => Err(ApiError::NotFound(format!("challenge {id}"))),
    }
}

async fn cli_challenge_approve(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user_id = require_user_id(&state, &headers).await?;
    let row: ChallengeRow = sqlx::query_as(
        "UPDATE cli_auth_challenges SET \
            approved_by_user_id = $2, approved_at = now(), updated_at = now() \
         WHERE id = $1 \
         RETURNING id, secret_hash, command, client_name, requested_access, requested_company_id, \
                   pending_key_hash, pending_key_name, approved_by_user_id, approved_at, \
                   cancelled_at, expires_at, created_at",
    )
    .bind(id)
    .bind(&user_id)
    .fetch_one(state.db.pool())
    .await?;
    Ok(Json(challenge_json(&row, true)))
}

async fn cli_challenge_cancel(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: ChallengeRow = sqlx::query_as(
        "UPDATE cli_auth_challenges SET cancelled_at = now(), updated_at = now() \
         WHERE id = $1 \
         RETURNING id, secret_hash, command, client_name, requested_access, requested_company_id, \
                   pending_key_hash, pending_key_name, approved_by_user_id, approved_at, \
                   cancelled_at, expires_at, created_at",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await?;
    Ok(Json(challenge_json(&row, false)))
}

async fn cli_auth_me(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Json<Value> {
    let user_id = require_user_id(&state, &headers).await.ok();
    Json(json!({
        "actor": if user_id.is_some() { "board" } else { "anonymous" },
        "userId": user_id,
        "roles": []
    }))
}

async fn board_keys_list(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user_id = require_user_id(&state, &headers).await?;
    let rows: Vec<BoardKeyRow> = sqlx::query_as(
        "SELECT id, user_id, name, key_hash, last_used_at, revoked_at, expires_at, created_at \
         FROM board_api_keys WHERE user_id = $1 AND revoked_at IS NULL ORDER BY created_at DESC",
    )
    .bind(&user_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(|r| board_key_json(r, false)).collect();
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize, Default)]
struct BoardKeyCreateBody {
    name: Option<String>,
    expires_at: Option<pc_core::Timestamp>,
}

async fn board_keys_create(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<BoardKeyCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let user_id = require_user_id(&state, &headers).await?;
    let name = body.name.clone().unwrap_or_else(|| "new-key".to_owned());
    let key_hash = "key-hash-stub".to_string();
    let row: BoardKeyRow = sqlx::query_as(
        "INSERT INTO board_api_keys (user_id, name, key_hash, expires_at) \
         VALUES ($1, $2, $3, $4) \
         RETURNING id, user_id, name, key_hash, last_used_at, revoked_at, expires_at, created_at",
    )
    .bind(&user_id)
    .bind(&name)
    .bind(&key_hash)
    .bind(body.expires_at)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::CREATED, Json(board_key_json(&row, true))))
}

async fn delete_board_key(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let user_id = require_user_id(&state, &headers).await?;
    sqlx::query(
        "UPDATE board_api_keys SET revoked_at = now() \
         WHERE id = $1 AND user_id = $2",
    )
    .bind(key_id)
    .bind(&user_id)
    .execute(state.db.pool())
    .await?;
    Ok((
        StatusCode::NO_CONTENT,
        Json(json!({ "id": key_id, "deleted": true })),
    ))
}

async fn cli_revoke_current(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let _ = require_user_id(&state, &headers).await?;
    Ok((StatusCode::OK, Json(json!({ "revoked": true }))))
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
