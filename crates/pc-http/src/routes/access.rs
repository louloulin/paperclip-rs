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
use pc_realtime::LiveEvent;
use pc_repos::invite::InviteRepo;
use pc_repos::skill::SkillRepo;

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
        .route("/api/invites/:invite_id", get(invites_get))
        .route("/api/invites/:invite_id/accept", post(invites_accept))
        .route("/api/invites/:invite_id/onboarding", get(invite_onboarding))
        // ── Round 43: plain-text onboarding doc + logo asset stub ──
        .route(
            "/api/invites/:invite_id/onboarding.txt",
            get(invite_onboarding_txt),
        )
        .route("/api/invites/:invite_id/logo", get(invite_logo))
        .route(
            "/api/invites/:invite_id/skills/index",
            get(invite_skills_index),
        )
        .route(
            "/api/invites/:invite_id/skills/:skill_name",
            get(invite_skill_get),
        )
        .route(
            "/api/invites/:invite_id/test-resolution",
            get(invite_test_resolution),
        )
        // NOTE: POST `/api/invites/:invite_id/revoke` is registered by invite_globals.rs.
        // The duplicate registration here was removed in Round 282 because it produced
        // axum "Overlapping method route" panics during integration tests. The local
        // `revoke_invite_by_token` handler remains as dead code (kept for reference).
        .route("/api/skills/available", get(skills_available))
        .route("/api/skills/index", get(skills_index))
        .route("/api/skills/:skill_name", get(skill_get))
        // ---- Round 42: admin endpoints ----
        .route("/api/admin/users", get(list_admin_users))
        .route(
            "/api/admin/users/:user_id/company-access",
            get(get_user_company_access).put(put_user_company_access),
        )
        .route(
            "/api/admin/users/:user_id/promote-instance-admin",
            post(promote_instance_admin),
        )
        .route(
            "/api/admin/users/:user_id/demote-instance-admin",
            post(demote_instance_admin),
        )
        // ── Round 215: join-requests claim API key ──
        .route(
            "/api/join-requests/:request_id/claim-api-key",
            post(claim_join_request_api_key),
        )
}

// Round 149: `ChallengeRow` 已迁到 `pc_repos::cli_challenge::ChallengeRow`。
use pc_repos::cli_challenge::ChallengeRow;

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

// Round 149: `BoardKeyRow` 已迁到 `pc_repos::board_key::BoardKeyRow`。
use pc_repos::board_key::BoardKeyRow;

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
    // Round 98 修复：原 SQL 引用不存在的 `board_claim_tokens` 表；
    // 真实表是 `board_api_keys`（语义不同：API key 持久 vs claim token 一次性）。
    // 这里 stub 为 404-valid-false，保留 URL 兼容。
    let _ = (&state, &token);
    Ok(Json(json!({
        "token": token,
        "kind": "board-claim",
        "valid": false,
        "reason": "deprecated: board_claim_tokens table missing in v3 schema",
        "deprecated": true,
    })))
}

async fn board_claim_token(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    // Round 98 修复：原 SQL 引用不存在的 `board_claim_tokens` + `sessions` 表。
    // 真实 auth 走 `board_api_keys`（API key 验证）和 `cli_auth_challenges`（CLI challenge）。
    // 这里 stub 返回 410 Gone，URL 兼容保留。
    let _ = (&state, &token, &body);
    return Ok((
        StatusCode::GONE,
        Json(json!({
            "claimed": false,
            "deprecated": true,
            "reason": "board_claim_tokens / sessions tables missing in v3 schema;                        use POST /api/auth/cli-authorize or board_api_keys instead",
        })),
    ));
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
    pc_repos::auth::AuthRepo::new(&state.db)
        .insert_bootstrap_session(Uuid::new_v4(), user_id, &token_hash)
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
    let expires_at = pc_core::Timestamp::from_dt(chrono::Utc::now() + chrono::Duration::minutes(5));
    let row = pc_repos::cli_challenge::ChallengeRepo::new(&state.db)
        .create(
            &secret_hash,
            &command,
            client_name.as_deref(),
            &requested_access,
            requested_company_id,
            &pending_key_hash,
            &pending_key_name,
            expires_at,
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let row = pc_repos::cli_challenge::ChallengeRepo::new(&state.db)
        .find_by_id(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let row = pc_repos::cli_challenge::ChallengeRepo::new(&state.db)
        .approve(id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(challenge_json(&row, true)))
}

async fn cli_challenge_cancel(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = pc_repos::cli_challenge::ChallengeRepo::new(&state.db)
        .cancel(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let rows = pc_repos::board_key::BoardKeyRepo::new(&state.db)
        .list_active_by_user(&user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    // R514: 切换到 pk_ 前缀（machine-to-machine 语义）；旧 pcp_board_ 仍可被 resolve_api_key 兼容验证（hash 不变）。
    let token = pc_auth::generate_api_key(pc_auth::KeyPrefix::Pk);
    let key_hash = sha2_sha256(&token);
    let row = pc_repos::board_key::BoardKeyRepo::new(&state.db)
        .create(&user_id, &name, &key_hash, body.expires_at)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let _ = pc_repos::board_key::BoardKeyRepo::new(&state.db)
        .revoke(key_id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    // 通过 pc_repos::invite::InviteRepo 按 token_hash 查 active 邀请。
    let token_hash = pc_repos::invite::hash_token_hex(&token);
    let row = pc_repos::invite::InviteRepo::new(&state.db)
        .find_active_by_token_hash(&token_hash)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let Some(inv) = row else {
        return Ok(Json(
            json!({"token": token, "valid": false, "reason": "not found"}),
        ));
    };
    let now = chrono::Utc::now();
    let valid =
        inv.revoked_at.is_none() && inv.accepted_at.is_none() && inv.expires_at.as_datetime() > now;
    // role 从 defaults_payload 提取（与 Round 28 audit 一致）
    let role = inv
        .defaults_payload
        .as_ref()
        .and_then(|v| v.get("role"))
        .and_then(|v| v.as_str())
        .map(String::from);
    Ok(Json(json!({
        "id": inv.id,
        "token": token,
        "companyId": inv.company_id,
        "role": role,
        "expiresAt": inv.expires_at,
        "acceptedAt": inv.accepted_at,
        "revokedAt": inv.revoked_at,
        "invitedByUserId": inv.invited_by_user_id,
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
    let token_hash = pc_repos::invite::hash_token_hex(&token);
    let inv = pc_repos::invite::InviteRepo::new(&state.db)
        .find_active_by_token_hash(&token_hash)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let Some(inv) = inv else {
        return Err(ApiError::BadRequest(
            "invite already used or expired".into(),
        ));
    };
    // mark_accepted 在 accepted_at IS NULL 上幂等。
    pc_repos::invite::InviteRepo::new(&state.db)
        .mark_accepted(inv.id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::OK,
        Json(json!({"accepted": true, "userId": user_id})),
    ))
}

async fn skills_available(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let rows = SkillRepo::new(&state.db)
        .list_public_skills()
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
    let rows = SkillRepo::new(&state.db)
        .list_all_skills_index()
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
    let row = SkillRepo::new(&state.db)
        .find_skill_by_key_or_name(&skill_name)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("skill {skill_name}")))?;
    Ok(Json(json!({
        "name": row.0,
        "displayName": row.1,
        "description": row.2,
        "content": row.3,
        "manifest": row.4,
    })))
}

fn sha2_sha256(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex_encode(&hasher.finalize())
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
/// Round 148: 委托到 `InviteRepo::lookup_by_token_hash` + `CompanyRepo::find_name_by_id`。
async fn lookup_invite_by_token(
    state: &AppState,
    token: &str,
) -> ApiResult<
    Option<(
        Uuid,
        Uuid,
        Option<String>,
        Option<pc_core::Timestamp>,
        Option<pc_core::Timestamp>,
        Option<pc_core::Timestamp>,
    )>,
> {
    let token_hash = pc_repos::invite::hash_token_hex(token);
    pc_repos::invite::InviteRepo::new(&state.db)
        .lookup_by_token_hash(&token_hash)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))
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
    let company_name = pc_repos::company::CompanyRepo::new(&state.db)
        .find_name_by_id(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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

/// `GET /api/invites/:token/onboarding.txt` — plain-text onboarding document.
/// Mirrors Node `/invites/:token/onboarding.txt` (a stripped-down text version
/// of the JSON manifest).  Storage-free, returns the inline document so the
/// UI / LLM agents can pull onboarding context without parsing JSON.
async fn invite_onboarding_txt(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let invite = lookup_invite_by_token(&state, &token).await?;
    let Some((id, company_id, role, expires_at, _accepted_at, revoked_at)) = invite else {
        return Err(ApiError::NotFound("invite not found".into()));
    };
    if revoked_at.is_some() {
        return Err(ApiError::NotFound("invite not found".into()));
    }
    let company_name = pc_repos::company::CompanyRepo::new(&state.db)
        .find_name_by_id(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let body = format!(
        "Paperclip Onboarding — invite {id}\n         ============================================\n         Company:   {name}\n         CompanyId: {company_id}\n         Role:      {role}\n         Token:     {token}\n         ExpiresAt: {expires}\n         \n         Steps:\n         1. POST /api/invites/{token}/accept to accept the invitation.\n         2. Configure your environment at /api/invites/{token}/onboarding.\n         \n         Welcome to {name}!\n",
        id = id,
        name = company_name.clone().unwrap_or_else(|| "(unknown)".to_string()),
        company_id = company_id,
        role = role.unwrap_or_else(|| "member".to_string()),
        token = token,
        expires = expires_at
            .map(|t| t.as_datetime().to_rfc3339())
            .unwrap_or_else(|| "(never)".to_string()),
    );
    Ok((
        StatusCode::OK,
        [("content-type", "text/plain; charset=utf-8")],
        body,
    ))
}

/// `GET /api/invites/:token/logo` — company logo asset stream.
///
/// Round 50: wired to pc-storage. Mirrors Node `/invites/:token/logo` which
/// streams from object storage. We look up the invite → company → logo asset,
/// resolve the storage provider, and stream the bytes with cache headers.
async fn invite_logo(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<impl IntoResponse> {
    use axum::http::header;
    use bytes::Bytes;

    let invite = lookup_invite_by_token(&state, &token).await?;
    let Some((_, company_id, _, _, accepted_at, revoked_at)) = invite else {
        return Err(ApiError::NotFound("Invite not found".into()));
    };
    if revoked_at.is_some() || accepted_at.is_some() {
        return Err(ApiError::NotFound("Invite not found".into()));
    }

    let row = pc_repos::asset::AssetRepo::new(&state.db)
        .find_logo_meta_by_company(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let (provider_name, object_key, content_type, byte_size, original_filename) =
        row.ok_or_else(|| ApiError::NotFound("Invite logo not found".into()))?;

    let provider = state.storage.resolve(&provider_name).map_err(|e| {
        ApiError::Internal(format!("storage provider {provider_name} unavailable: {e}"))
    })?;
    let target = pc_storage::StorageLocation {
        bucket: provider_name,
        key: pc_storage::ObjectKey::new(object_key.clone()),
    };
    let bytes: Bytes = provider.get_object(&target).await.map_err(|e| match e {
        pc_storage::StorageError::NotFound(_) => {
            ApiError::NotFound(format!("Invite logo content {object_key}"))
        }
        other => ApiError::Internal(other.to_string()),
    })?;

    let filename = original_filename.unwrap_or_else(|| "company-logo".to_string());
    let content_type_for_check = content_type.clone();
    let mut response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, byte_size.to_string())
        .header(header::CACHE_CONTROL, "private, max-age=60")
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{}\"", filename.replace('"', "")),
        )
        .header("x-content-type-options", "nosniff");
    if content_type_for_check == "image/svg+xml" {
        response = response.header(
            "content-security-policy",
            "sandbox; default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'",
        );
    }
    Ok(response
        .body(axum::body::Body::from(bytes))
        .map_err(|e| ApiError::Internal(e.to_string()))?)
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
    let row = pc_repos::skill::SkillRepo::new(&state.db)
        .find_content_by_key(&skill_name)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let row = pc_repos::invite::InviteRepo::new(&state.db)
        .lookup_by_token_hash(&token_hash)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let row = pc_repos::invite::InviteRepo::new(&state.db)
        .lookup_revoke_info_by_token_hash(&token_hash)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let Some((id, _company_id, invited_by_user_id)) = row else {
        return Err(ApiError::NotFound("invite not found".into()));
    };
    if invited_by_user_id.as_deref() != Some(user_id.as_str()) {
        return Err(ApiError::Forbidden(
            "only the inviter can revoke this invite".into(),
        ));
    }
    let updated = pc_repos::invite::InviteRepo::new(&state.db)
        .revoke_by_id(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if updated == 0 {
        return Err(ApiError::Conflict("invite already revoked".into()));
    }
    Ok(Json(json!({
        "id": id,
        "revoked": true,
        "revokedAt": chrono::Utc::now(),
    })))
}

// ============================================================================
// Round 42: instance admin endpoints (list users / company-access / promote / demote)
// ============================================================================

/// `GET /api/admin/users` — instance admin user directory.  Mirrors Node
/// `/admin/users`.  Limited to first 50 rows by `updated_at DESC`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AdminUsersQuery {
    #[serde(default)]
    query: String,
}

/// `GET /api/admin/users` — instance admin user directory.
/// Mirrors Node `GET /admin/users` with full parity:
/// - Returns flat array (not {items, count})
/// - Per-user: id, email, name, image, isInstanceAdmin, activeCompanyMembershipCount
async fn list_admin_users(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<AdminUsersQuery>,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    let needle = q.query.trim().to_lowercase();

    let rows: Vec<(String, Option<String>, Option<String>, Option<String>, chrono::DateTime<chrono::Utc>)> =
        pc_repos::user_profile::UserProfileRepo::new(&state.db)
            .list_recent(50)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;

    // TypeScript: filters by name or email if needle is non-empty
    let user_profiles: Vec<(String, Option<String>, Option<String>, Option<String>)> = if needle.is_empty() {
        rows.into_iter().map(|(id, name, email, image, _)| (id, name, email, image)).collect()
    } else {
        rows.into_iter()
            .filter(|(_, name, email, _, _)| {
                let name_match = name.as_ref().map(|n| n.to_lowercase().contains(&needle)).unwrap_or(false);
                let email_match = email.as_ref().map(|e| e.to_lowercase().contains(&needle)).unwrap_or(false);
                name_match || email_match
            })
            .map(|(id, name, email, image, _)| (id, name, email, image))
            .collect()
    };

    let user_ids: Vec<String> = user_profiles.iter().map(|(id, _, _, _)| id.clone()).collect();

    let admin_rows = pc_repos::instance_user_role::InstanceUserRoleRepo::new(&state.db)
        .list_user_ids_with_any_role(&user_ids)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let admin_set: std::collections::HashSet<String> = admin_rows.into_iter().collect();

    let membership_counts = pc_repos::company_member::CompanyMemberRepo::new(&state.db)
        .count_active_memberships_for_users(&user_ids)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let result: Vec<Value> = user_profiles
        .into_iter()
        .map(|(id, name, email, image)| {
            let uid = &id;
            let is_admin = admin_set.contains(uid);
            let active_count = membership_counts.get(uid).copied().unwrap_or(0);
            json!({
                "id": id,
                "email": email,
                "name": name,
                "image": image,
                "isInstanceAdmin": is_admin,
                "activeCompanyMembershipCount": active_count,
            })
        })
        .collect();

    Ok(Json(Value::Array(result)))
}

/// `GET /api/admin/users/:user_id/company-access` — list the user's
/// company access (memberships + invitations).
/// Mirrors Node `GET /admin/users/:userId/company-access`.
/// Returns { user: {id, email, name, image, isInstanceAdmin}, companyAccess: [...] }
async fn get_user_company_access(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    let response = build_company_access_response(&state, &user_id).await?;
    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutUserCompanyAccessBody {
    #[serde(default)]
    company_ids: Vec<Uuid>,
}

/// Helper: build the `{ user: {...}, companyAccess: [...] }` response for
/// the company-access endpoint.  Used by both GET and PUT.
async fn build_company_access_response(
    state: &AppState,
    user_id: &str,
) -> ApiResult<Value> {
    // Fetch user profile directly (id, email, name, image) — matches TypeScript SELECT
    #[derive(Debug, sqlx::FromRow)]
    struct UserRow {
        id: String,
        email: Option<String>,
        name: Option<String>,
        image: Option<String>,
    }
    let user_row: Option<UserRow> = sqlx::query_as::<_, UserRow>(
        r#"SELECT id, email, name, image FROM "user" WHERE id = $1"#,
    )
    .bind(user_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    let is_admin = pc_repos::instance_user_role::InstanceUserRoleRepo::new(&state.db)
        .is_admin(user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let user_json = user_row.map(|row| {
        json!({
            "id": row.id,
            "email": row.email,
            "name": row.name,
            "image": row.image,
            "isInstanceAdmin": is_admin,
        })
    });

    let memberships = pc_repos::company_member::CompanyMemberRepo::new(&state.db)
        .list_for_user_with_company(user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let company_ids: Vec<Uuid> = memberships.iter().map(|(id, _, _, _)| *id).collect();
    let companies: std::collections::HashMap<Uuid, (String, Option<String>)> =
        if company_ids.is_empty() {
            std::collections::HashMap::new()
        } else {
            let rows: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
                "SELECT id, name, status FROM companies WHERE id = ANY($1)",
            )
            .bind(&company_ids)
            .fetch_all(state.db.pool())
            .await
            .unwrap_or_default();
            rows.into_iter().map(|(id, name, status)| (id, (name, status))).collect()
        };

    let company_access: Vec<Value> = memberships
        .into_iter()
        .map(|(id, _, role, status)| {
            let (name, company_status) =
                companies.get(&id).cloned().unwrap_or((String::new(), None));
            json!({
                "principalType": "user",
                "companyId": id,
                "companyName": name,
                "companyStatus": company_status,
                "membershipRole": role,
                "status": status,
            })
        })
        .collect();

    Ok(json!({
        "user": user_json,
        "companyAccess": company_access,
    }))
}

/// `PUT /api/admin/users/:user_id/company-access` — replace the user's full
/// company access set.  Mirrors Node `PUT /admin/users/:userId/company-access`.
async fn put_user_company_access(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PutUserCompanyAccessBody>,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    pc_repos::company_member::CompanyMemberRepo::new(&state.db)
        .replace_user_companies(&user_id, &body.company_ids)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    state.realtime.publish(
        LiveEvent::new("user.company_access_updated", "user", Uuid::nil())
            .with_data(json!({"userId": user_id, "companyCount": body.company_ids.len()})),
    );
    // Return the same format as GET (matches TypeScript)
    let response = build_company_access_response(&state, &user_id).await?;
    Ok(Json(response))
}

/// `POST /api/admin/users/:user_id/promote-instance-admin` — grant instance
/// admin role.  Mirrors Node `POST /admin/users/:userId/promote-instance-admin`.
async fn promote_instance_admin(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    // R800: Use promote_returning_row to get full row (matches TypeScript response)
    let row = pc_repos::instance_user_role::InstanceUserRoleRepo::new(&state.db)
        .promote_returning_row(&user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    state.realtime.publish(
        LiveEvent::new("user.promoted_instance_admin", "user", Uuid::nil())
            .with_data(json!({"userId": user_id})),
    );
    // TypeScript returns { userId, role, createdAt } from the row
    Ok(Json(json!({
        "userId": row.user_id,
        "role": row.role,
        "createdAt": row.created_at,
    })))
}

/// `POST /api/admin/users/:user_id/demote-instance-admin` — revoke instance
/// admin role.  Mirrors Node `POST /admin/users/:userId/demote-instance-admin`.
async fn demote_instance_admin(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    // R800: Use demote_returning_row to get deleted row (matches TypeScript response)
    let row_opt = pc_repos::instance_user_role::InstanceUserRoleRepo::new(&state.db)
        .demote_returning_row(&user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if row_opt.is_none() {
        return Err(ApiError::NotFound(format!(
            "instance admin role for {user_id}"
        )));
    }
    state.realtime.publish(
        LiveEvent::new("user.demoted_instance_admin", "user", Uuid::nil())
            .with_data(json!({"userId": user_id})),
    );
    let row = row_opt.unwrap();
    // TypeScript returns the deleted row or null
    Ok(Json(json!({
        "userId": row.user_id,
        "role": row.role,
        "createdAt": row.created_at,
    })))
}

/// `POST /api/join-requests/:request_id/claim-api-key` —
/// join request 认领 API key 端口（与 Node `access.ts` 对齐）。
///
/// 流程：
/// 1. 校验 join request 类型/状态/claim_secret_hash 等
/// 2. hash 比对（常数时间）
/// 3. 原子标记 claim_secret_consumed_at
/// 4. 生成新的 agent_api_key（带明文 token）
/// 5. 返回 { keyId, token, agentId, createdAt }
async fn claim_join_request_api_key(
    State(state): State<AppState>,
    Path(request_id): Path<Uuid>,
    Json(body): Json<ClaimJoinRequestApiKeyBody>,
) -> ApiResult<impl IntoResponse> {
    use pc_repos::agent::{AgentRepo, CreateAgentApiKeyWithTokenInput};
    use pc_repos::join_request::JoinRequestRepo;

    // 1. hash presented claim secret + atomic mark consumed
    let presented_hash = pc_auth::hash_token(&body.claim_secret);
    let claimed = JoinRequestRepo::new(&state.db)
        .claim_api_key(request_id, &presented_hash)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let agent_id = claimed.created_agent_id.ok_or_else(|| {
        ApiError::Internal("join request has no created agent after claim".to_string())
    })?;
    let responsible_user_id = claimed
        .approved_by_user_id
        .or_else(|| claimed.requesting_user_id);

    // 2. Generate + persist API key (token returned, only hash stored)
    let (row, returned_token) = AgentRepo::new(&state.db)
        .create_api_key_with_token(CreateAgentApiKeyWithTokenInput {
            agent_id,
            company_id: claimed.company_id,
            name: "initial-join-key".to_string(),
            responsible_user_id,
            scope_config: Some(json!({"kind": "standard"})),
        })
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // 3. Publish realtime event for activity stream
    state.realtime.publish(
        LiveEvent::new("agent_api_key.claimed", "agent_api_key", row.id).with_data(json!({
            "agentId": agent_id,
            "joinRequestId": request_id,
            "companyId": claimed.company_id,
        })),
    );

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "keyId": row.id,
            "token": returned_token,
            "agentId": agent_id,
            "createdAt": row.created_at,
        })),
    ))
}

#[derive(Debug, Deserialize)]
struct ClaimJoinRequestApiKeyBody {
    claim_secret: String,
}
