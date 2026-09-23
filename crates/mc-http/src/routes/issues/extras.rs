//! `/api/issues/:id` 的 reactions / metadata / properties（从 `issues.rs` 拆出，
//! R7 单文件 800 行上限）。

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::{not_found, require_workspace_member};
use crate::routes::properties::{property_err, property_repo, property_repo_err};
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_errors::Error;
use mc_repos::issue::IssueReactionRow;
use mc_repos::property::{
    actor_refs_in_value, bag_exceeds_limit, merge_bag, type_is_actor, validate_value,
};
use serde_json::Value as JsonValue;
use std::sync::Arc;

use super::context::{issue_repo, load_issue, parse_target_id, resolve_workspace, WorkspaceQuery};
use super::dto::{
    IssueReactionDto, MetadataResponse, PropertiesResponse, ReactionRequest,
    SetPropertyValueRequest, ValueRequest,
};
use super::helpers::{repo_err, validation};
use super::{METADATA_KEYS_MAX, METADATA_KEY_MAX_LEN};

// ---------------------------------------------------------------------------
// reactions / metadata / properties
pub(crate) fn reaction_dto(row: &IssueReactionRow) -> IssueReactionDto {
    IssueReactionDto {
        id: row.id.to_string(),
        issue_id: row.issue_id.to_string(),
        actor_type: row.actor_type.clone(),
        actor_id: row.actor_id.clone(),
        emoji: row.emoji.clone(),
        created_at: row.created_at.to_rfc3339(),
    }
}

/// `GET /api/issues/:id/reactions`
pub(crate) async fn list_reactions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<Vec<IssueReactionDto>>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    let rows = repo.list_reactions(issue.id()).await.map_err(repo_err)?;
    Ok(Json(rows.iter().map(reaction_dto).collect()))
}

/// `POST /api/issues/:id/reactions`
pub(crate) async fn add_reaction(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ReactionRequest>,
) -> ApiResult<Response> {
    let emoji = req.emoji.trim().to_string();
    if emoji.is_empty() {
        return Err(validation("emoji is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    // 0004 的 CHECK 只允许 user/agent/system（上游写 `member`）；X-Agent-ID 归 M3
    let row = repo
        .add_reaction(
            workspace_id,
            issue.id(),
            "user",
            &user.id().to_string(),
            &emoji,
        )
        .await
        .map_err(repo_err)?;
    Ok((StatusCode::CREATED, Json(reaction_dto(&row))).into_response())
}

/// `DELETE /api/issues/:id/reactions`（body `{"emoji": "👍"}`）
pub(crate) async fn remove_reaction(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ReactionRequest>,
) -> ApiResult<StatusCode> {
    let emoji = req.emoji.trim().to_string();
    if emoji.is_empty() {
        return Err(validation("emoji is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    repo.remove_reaction(issue.id(), "user", &user.id().to_string(), &emoji)
        .await
        .map_err(repo_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// body 解码（上游 `json.Decoder` 失败 ⇒ 400 `invalid request body`）。
fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Error> {
    serde_json::from_slice::<T>(body).map_err(|_| validation("invalid request body"))
}

pub(crate) fn validate_metadata_key(key: &str) -> Result<(), Error> {
    let trimmed = key.trim();
    if trimmed.is_empty() || trimmed.len() > METADATA_KEY_MAX_LEN {
        return Err(validation(format!(
            "metadata key must be 1..={METADATA_KEY_MAX_LEN} characters"
        )));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(validation(
            "metadata key may only contain letters, digits, '_', '-' and '.'",
        ));
    }
    Ok(())
}

pub(crate) fn validate_metadata_value(value: &JsonValue) -> Result<(), Error> {
    if value.is_array() || value.is_object() {
        return Err(validation(
            "metadata values must be primitives (string, number, boolean, null)",
        ));
    }
    Ok(())
}

/// `GET /api/issues/:id/metadata`
pub(crate) async fn get_metadata(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<MetadataResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    let metadata = repo
        .get_metadata(workspace_id, issue.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(MetadataResponse {
        metadata,
        issue_revision: None,
    }))
}

/// `PUT /api/issues/:id/metadata/:key`
pub(crate) async fn set_metadata_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, key)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ValueRequest>,
) -> ApiResult<Json<MetadataResponse>> {
    validate_metadata_key(&key)?;
    validate_metadata_value(&req.value)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let existing = current.metadata.as_object().map_or(0, serde_json::Map::len);
    let known = current
        .metadata
        .as_object()
        .is_some_and(|map| map.contains_key(&key));
    if !known && existing >= METADATA_KEYS_MAX {
        return Err(validation(format!("metadata cannot exceed {METADATA_KEYS_MAX} keys")).into());
    }

    let metadata = repo
        .set_metadata_key(workspace_id, current.id(), &key, &req.value)
        .await
        .map_err(repo_err)?;
    let row = repo
        .get(workspace_id, current.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(MetadataResponse {
        metadata,
        issue_revision: Some(row.revision),
    }))
}

/// `DELETE /api/issues/:id/metadata/:key`
pub(crate) async fn delete_metadata_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, key)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<MetadataResponse>> {
    validate_metadata_key(&key)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let metadata = repo
        .delete_metadata_key(workspace_id, current.id(), &key)
        .await
        .map_err(repo_err)?;
    Ok(Json(MetadataResponse {
        metadata,
        issue_revision: None,
    }))
}

/// `PUT /api/issues/:id/properties/:propertyId`
///
/// M2-E（LUM-1370）起值也存进 `issue.properties` JSONB（key = 定义的 UUID 文本），
/// 但写入前**先过定义面**（上游 `SetIssueProperty` 的顺序）：
/// 定义不存在 → 404、已归档 → 400、类型/config 不匹配 → 400（消息逐字对齐
/// `internal/issueproperty.ValidateValue`），actor 类值再解析成员引用。
pub(crate) async fn set_property(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, property_id)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<PropertiesResponse>> {
    let property_id = parse_target_id("property id", &property_id)?;
    let req: SetPropertyValueRequest = parse_body(&body)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;

    // 定义面：`definition_for_value` 已经把「未命中」与「已归档」分开（404 / 400）。
    // 注意校验顺序：上游是先取定义（404 / 已归档 400）再校验值，因此 body 里
    // 缺 `value` 的 400 落在这后面。
    let definitions = property_repo(&state);
    let definition = definitions
        .definition_for_value(workspace_id, property_id)
        .await
        .map_err(property_err)?;
    // 三态：缺字段 → `value is required`；显式 `null` → 交给 `validate_value` 出
    // `value cannot be null (use DELETE to unset a property)`（与上游一致）。
    let value = match req.value {
        None => return Err(validation("value is required").into()),
        Some(None) => JsonValue::Null,
        Some(Some(value)) => value,
    };
    let canonical = validate_value(&definition.property_type, &definition.config, &value)
        .map_err(validation)?;
    if type_is_actor(&definition.property_type) {
        let refs = actor_refs_in_value(&definition.property_type, &canonical);
        definitions
            .resolve_actor_refs(workspace_id, &refs)
            .await
            .map_err(property_err)?;
    }
    // 16KB 上限：DB 的 `issue_properties_size_limit` CHECK 是底线，这里先做一次
    // 等价预检，好把 CHECK 违反（会落成 500）换成上游的 400 文案。
    let key = property_id.to_string();
    if bag_exceeds_limit(&merge_bag(&current.properties, &key, &canonical)) {
        return Err(validation("issue properties exceed the 16KB size limit").into());
    }

    let properties = repo
        .set_property(workspace_id, current.id(), &key, &canonical)
        .await
        .map_err(repo_err)?;
    let row = repo
        .get(workspace_id, current.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(PropertiesResponse {
        properties,
        issue_revision: Some(row.revision),
    }))
}

/// `DELETE /api/issues/:id/properties/:propertyId`
///
/// 与写入同理：定义不存在 → 404（上游 `GetIssueProperty` 在删值前先读）；已归档的定义
/// **仍然可以删值**（上游注释：cleanup 不能被阻塞）。响应恒带 `issue_revision`
/// （上游 `DeleteIssuePropertyValue` 返回整行 issue）。
pub(crate) async fn delete_property(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, property_id)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<PropertiesResponse>> {
    let property_id = parse_target_id("property id", &property_id)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    if !property_repo(&state)
        .definition_exists(workspace_id, property_id)
        .await
        .map_err(property_repo_err)?
    {
        return Err(not_found("property").into());
    }
    let properties = repo
        .delete_property(workspace_id, current.id(), &property_id.to_string())
        .await
        .map_err(repo_err)?;
    let row = repo
        .get(workspace_id, current.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(PropertiesResponse {
        properties,
        issue_revision: Some(row.revision),
    }))
}
