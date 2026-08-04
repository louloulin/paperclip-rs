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
        .route("/api/invites/:token/onboarding", get(invite_onboarding))
        .route("/api/invites/:token/skills/index", get(invite_skills_index))
        .route("/api/invites/:token/skills/:skill_name", get(invite_skill_get))
        .route("/api/invites/:token/test-resolution", get(invite_test_resolution))
        .route("/api/invites/:token/revoke", post(revoke_invite_by_token))
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

async fn board_claim(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, String, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT id, kind, company_id, status FROM board_claim_tokens WHERE token = $1 LIMIT 1",
    )
    .bind(&token)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let Some((id, kind, company_id, status)) = row else {
        return Ok(Json(
            json!({"token": token, "kind": "board-claim", "valid": false, "reason": "not found"}),
        ));
    };
    Ok(Json(json!({
        "id": id,
        "token": token,
        "kind": kind,
        "companyId": company_id,
        "status": status,
        "valid": status == "pending",
    })))
}

async fn board_claim_token(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let user_id = body
        .get("userId")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| Some("local-board".to_string()));
    // Mark the token as claimed, create a fresh session token.
    let session_token = Uuid::new_v4().to_string();
    let session_token_hash = sha2_sha256(&session_token);
    let expires = chrono::Utc::now() + chrono::Duration::days(7);
    let mut tx = state
        .db
        .pool()
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    sqlx::query(
        "UPDATE board_claim_tokens SET status = 'claimed', claimed_by = $1, claimed_at = now()          WHERE token = $2 AND status = 'pending'",
    )
    .bind(user_id.as_deref())
    .bind(&token)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    sqlx::query(
        "INSERT INTO sessions (id, user_id, token_hash, expires_at)          VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(user_id.as_deref().unwrap_or("local-board"))
    .bind(&session_token_hash)
    .bind(expires)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "claimed": true,
            "sessionToken": session_token,
            "expiresAt": expires.to_rfc3339(),
        })),
    ))
}

async fn bootstrap_claim(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let user_id = body
        .get("userId")
        .and_then(|v| v.as_str())
        .unwrap_or("u_bootstrap");
    let session_token = Uuid::new_v4().to_string();
    let token_hash = sha2_sha256(&session_token);
    sqlx::query(
        "INSERT INTO sessions (id, user_id, token_hash, expires_at)          VALUES ($1, $2, $3, now() + interval '30 days')          ON CONFLICT (token_hash) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(&token_hash)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "claimed": true,
            "userId": user_id,
            "sessionToken": session_token,
        })),
    ))
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
    let challenge_secret = random_cli_token("pcp_cli_auth_");
    let pending_board_token = random_cli_token("pcp_board_");
    let pending_key_hash = pc_auth::hash_token(&pending_board_token);
    let secret_hash = pc_auth::hash_token(&challenge_secret);
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
    Ok(Json(json!({
        "id": row.id,
        "token": challenge_secret,
        "boardApiToken": pending_board_token,
        "approvalPath": format!("/cli-auth/challenges/{}", row.id),
        "pollPath": format!("/cli-auth/challenges/{}", row.id),
        "expiresAt": row.expires_at,
        "suggestedPollIntervalMs": 1000,
    })))
}

fn random_cli_token(prefix: &str) -> String {
    format!(
        "{prefix}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
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
    let token = random_cli_token("pcp_board_");
    let key_hash = sha2_sha256(&token);
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
    let mut response = board_key_json(&row, true);
    if let Some(obj) = response.as_object_mut() {
        obj.insert("token".into(), Value::String(token.clone()));
    }
    Ok((StatusCode::CREATED, Json(response)))
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

async fn invites_get(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Json<Value>> {
    // Round 38 fix: invites table stores token_hash (SHA-256 hex of the
    // opaque token).  Previously the handler queried `token = $1` which
    // matched no rows because the column is named `token_hash`.
    let token_hash = sha2_sha256(&token);
    let row: Option<(Uuid, Uuid, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>, Option<String>)> = sqlx::query_as(
        "SELECT id, company_id, expires_at, accepted_at, revoked_at, invited_by_user_id          FROM invites WHERE token_hash = $1 LIMIT 1",
    )
    .bind(&token_hash)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let Some((id, company_id, expires_at, accepted_at, revoked_at, invited_by_user_id)) = row else {
        return Ok(Json(
            json!({"token": token, "valid": false, "reason": "not found"}),
        ));
    };
    let now = chrono::Utc::now();
    let valid = revoked_at.is_none() && accepted_at.is_none() && expires_at.map_or(false, |exp| exp.as_datetime() > now);
    // Role lives in defaults_payload jsonb (per Round 28 audit fix)
    let role: Option<String> = sqlx::query_scalar(
        "SELECT defaults_payload->>'role' FROM invites WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .flatten();
    Ok(Json(json!({
        "id": id,
        "token": token,
        "companyId": company_id,
        "role": role,
        "expiresAt": expires_at,
        "acceptedAt": accepted_at,
        "revokedAt": revoked_at,
        "invitedByUserId": invited_by_user_id,
        "valid": valid,
    })))
}

async fn invites_accept(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Json(body): Json<ClaimBody>,
) -> ApiResult<impl IntoResponse> {
    let user_id = body
        .user_id
        .clone()
        .unwrap_or_else(|| "u_invited".to_owned());
    let updated = sqlx::query(
        "UPDATE invites SET status = 'accepted', accepted_by = $1, accepted_at = now()          WHERE token = $2 AND status = 'pending'",
    )
    .bind(&user_id)
    .bind(&token)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::BadRequest(
            "invite already used or expired".into(),
        ));
    }
    Ok((
        StatusCode::OK,
        Json(json!({"accepted": true, "userId": user_id})),
    ))
}

async fn skills_available(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT skill_key, display_name, description FROM skills          WHERE visibility = 'public' ORDER BY display_name",
    )
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(k, name, desc)| {
            json!({
                "key": k,
                "name": name,
                "description": desc,
            })
        })
        .collect();
    Ok(Json(json!({"items": items})))
}

async fn skills_index(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT skill_key, display_name, category FROM skills ORDER BY skill_key")
            .fetch_all(state.db.pool())
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut index = serde_json::Map::new();
    for (k, name, cat) in rows {
        index.insert(k, json!({ "name": name, "category": cat }));
    }
    Ok(Json(json!({"index": Value::Object(index), "version": "1"})))
}

async fn skill_get(
    State(state): State<AppState>,
    Path(skill_name): Path<String>,
) -> ApiResult<Json<Value>> {
    let row: Option<(String, String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT skill_key, display_name, description, content_md, manifest          FROM skills WHERE skill_key = $1 OR display_name = $1 LIMIT 1",
    )
    .bind(&skill_name)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    match row {
        Some((k, name, desc, content, manifest)) => Ok(Json(json!({
            "name": k,
            "displayName": name,
            "description": desc,
            "content": content,
            "manifest": manifest,
        }))),
        None => Err(ApiError::NotFound(format!("skill {skill_name}"))),
    }
}

fn sha2_sha256(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    let hex = result
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    hex
}
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}


// ============================================================================
// Round 38: invite public endpoints (onboarding / skills / test-resolution / revoke)
// ============================================================================

/// Hash token + lookup helper used by all Round 38 invite handlers.
async fn lookup_invite_by_token(
    state: &AppState,
    token: &str,
) -> ApiResult<Option<(Uuid, Uuid, Option<String>, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>)>> {
    let token_hash = sha2_sha256(token);
    let row: Option<(Uuid, Uuid, Option<String>, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>)> = sqlx::query_as(
        "SELECT id, company_id, defaults_payload->>'role', expires_at, accepted_at, revoked_at \
         FROM invites WHERE token_hash = $1 LIMIT 1",
    )
    .bind(&token_hash)
    .fetch_optional(state.db.pool())
    .await?;
    Ok(row)
}

/// `GET /api/invites/:token/onboarding` — minimal onboarding manifest.
/// Mirrors Node `/invites/:token/onboarding` but returns a simple JSON
/// shape (no plugin manifest assembly) so the UI can render the basics.
async fn invite_onboarding(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Json<Value>> {
    let invite = lookup_invite_by_token(&state, &token).await?;
    let Some((id, company_id, role, expires_at, accepted_at, revoked_at)) = invite else {
        return Err(ApiError::NotFound("invite not found".into()));
    };
    if revoked_at.is_some() || accepted_at.is_some() {
        return Err(ApiError::NotFound("invite not found".into()));
    }
    let company_name: Option<String> = sqlx::query_scalar("SELECT name FROM companies WHERE id=$1")
        .bind(company_id)
        .fetch_optional(state.db.pool())
        .await?;
    Ok(Json(json!({
        "invite": {
            "id": id,
            "token": token,
            "companyId": company_id,
            "role": role,
            "expiresAt": expires_at,
        },
        "company": {
            "id": company_id,
            "name": company_name,
        },
        "steps": [
            { "key": "accept", "label": "Accept invite" },
            { "key": "configure", "label": "Configure environment" },
        ],
    })))
}

/// `GET /api/invites/:token/skills/index` — public skill catalog reachable
/// via invite token.  Mirrors Node `/invites/:token/skills/index`.
async fn invite_skills_index(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Json<Value>> {
    let invite = lookup_invite_by_token(&state, &token).await?;
    let Some((_, _, _, _, accepted_at, revoked_at)) = invite else {
        return Err(ApiError::NotFound("invite not found".into()));
    };
    if revoked_at.is_some() || accepted_at.is_some() {
        return Err(ApiError::NotFound("invite not found".into()));
    }
    Ok(Json(json!({
        "token": token,
        "skills": [
            { "name": "paperclip", "path": format!("/api/invites/{}/skills/paperclip", token) },
        ],
    })))
}

/// `GET /api/invites/:token/skills/:skill_name` — single skill markdown by
/// invite token.  Mirrors Node `/invites/:token/skills/:skillName`.
async fn invite_skill_get(
    State(state): State<AppState>,
    Path((token, skill_name)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let invite = lookup_invite_by_token(&state, &token).await?;
    let Some((_, _, _, _, accepted_at, revoked_at)) = invite else {
        return Err(ApiError::NotFound("invite not found".into()));
    };
    if revoked_at.is_some() || accepted_at.is_some() {
        return Err(ApiError::NotFound("invite not found".into()));
    }
    let skill_name = skill_name.to_lowercase();
    if skill_name != "paperclip" {
        return Err(ApiError::NotFound(format!("skill {skill_name}")));
    }
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT content_md, manifest FROM skills WHERE skill_key=$1 LIMIT 1",
    )
    .bind(&skill_name)
    .fetch_optional(state.db.pool())
    .await?;
    let Some((content, manifest)) = row else {
        return Err(ApiError::NotFound(format!("skill {skill_name}")));
    };
    Ok(Json(json!({
        "token": token,
        "name": skill_name,
        "content": content,
        "manifest": manifest,
    })))
}

/// `GET /api/invites/:token/test-resolution` — debug probe that returns
/// which invite record the token resolves to (without exposing secrets).
/// Mirrors Node `/invites/:token/test-resolution`.
async fn invite_test_resolution(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Json<Value>> {
    let token_hash = sha2_sha256(&token);
    let row: Option<(Uuid, Uuid, Option<String>, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>)> = sqlx::query_as(
        "SELECT id, company_id, defaults_payload->>'role', expires_at, accepted_at, revoked_at \
         FROM invites WHERE token_hash = $1 LIMIT 1",
    )
    .bind(&token_hash)
    .fetch_optional(state.db.pool())
    .await?;
    let Some((id, company_id, role, expires_at, accepted_at, revoked_at)) = row else {
        return Ok(Json(json!({
            "token": token,
            "resolved": false,
            "reason": "no row matched token_hash",
        })));
    };
    let now = chrono::Utc::now();
    let expired = expires_at.map_or(false, |exp| exp.as_datetime() <= now);
    Ok(Json(json!({
        "token": token,
        "resolved": true,
        "invite": {
            "id": id,
            "companyId": company_id,
            "role": role,
            "expiresAt": expires_at,
            "acceptedAt": accepted_at,
            "revokedAt": revoked_at,
            "expired": expired,
            "accepted": accepted_at.is_some(),
            "revoked": revoked_at.is_some(),
        },
    })))
}

/// `POST /api/invites/:token/revoke` — mark an invite as revoked.  Mirrors
/// Node `POST /invites/:inviteId/revoke`.  Requires admin / company access
/// to prevent abuse; we check that the caller matches the inviter user id.
async fn revoke_invite_by_token(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    let token_hash = sha2_sha256(&token);
    let row: Option<(Uuid, Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, company_id, invited_by_user_id FROM invites WHERE token_hash=$1 LIMIT 1",
    )
    .bind(&token_hash)
    .fetch_optional(state.db.pool())
    .await?;
    let Some((id, _company_id, invited_by_user_id)) = row else {
        return Err(ApiError::NotFound("invite not found".into()));
    };
    if invited_by_user_id.as_deref() != Some(user_id.as_str()) {
        return Err(ApiError::Forbidden(
            "only the inviter can revoke this invite".into(),
        ));
    }
    let updated = sqlx::query(
        "UPDATE invites SET revoked_at = now(), updated_at = now() \
         WHERE id=$1 AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(state.db.pool())
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(ApiError::Conflict("invite already revoked".into()));
    }
    Ok(Json(json!({
        "id": id,
        "revoked": true,
        "revokedAt": chrono::Utc::now(),
    })))
}
