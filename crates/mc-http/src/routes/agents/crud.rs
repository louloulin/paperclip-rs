//! `/api/agents*` 的 CRUD + 归档/恢复 + 取消任务 + 任务列表（上游 `agent.go`）。
//!
//! | method | path | 上游 |
//! |---|---|---|
//! | GET | `/api/agents/` | `ListAgents` L1110 |
//! | POST | `/api/agents/` | `CreateAgent` L1379（201） |
//! | GET | `/api/agents/:id/` | `GetAgent` L1223 |
//! | PUT | `/api/agents/:id/` | `UpdateAgent` L1878 |
//! | POST | `/api/agents/:id/archive` | `ArchiveAgent` L2541 |
//! | POST | `/api/agents/:id/restore` | `RestoreAgent` L2599 |
//! | POST | `/api/agents/:id/cancel-tasks` | `CancelAgentTasks` L2651 |
//! | GET | `/api/agents/:id/tasks` | `ListAgentTasks` L2673 |
//!
//! 有意偏离（见 `docs/40-M3-5-AGENTS.md` §5）：不广播 WS 事件（M3-7）、
//! 不校验 `thinking_level` / `service_tier` 的 provider 枚举（M3-2/M3-4）、
//! 跨 provider 绑定时不清空旧 `model`、`avatar_url` 原样透传、`skills` 恒 `[]`。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::de::DeserializeOwned;
use serde_json::{Map as JsonMap, Value as JsonValue};

use mc_errors::Error;
use mc_repos::agent::{
    AgentInvocationTargetRow, AgentRow, AgentUpdatePatch, NewAgent, NullableAgentField,
    DEFAULT_MAX_CONCURRENT_TASKS, VISIBILITY_PRIVATE,
};

use super::dto::{
    derive_legacy_visibility, normalise_composio_allowlist, normalise_conversation_starters,
    parse_permission_input, preserve_masked_gateway_token, AgentDto, AgentTaskDto, CancelTasksDto,
    CreateAgentRequest, ResolvedPermission, UpdateAgentRequest,
};
use super::{bad_request, parse_uuid, repo_err, AgentScope};
use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 请求体解码
// ---------------------------------------------------------------------------

/// 上游 `decodeJSONBodyWithRawFields`：空 body / 形状不符 → 400 `invalid request body`；
/// 同时保留原始字段表，供「字段是否存在」与「显式 null」判定。
fn decode_body<T: DeserializeOwned + Default>(
    body: &Bytes,
) -> Result<(T, JsonMap<String, JsonValue>), Error> {
    let value: JsonValue =
        serde_json::from_slice(body).map_err(|_| bad_request("invalid request body"))?;
    if value.is_null() {
        // Go 对 `null` body 静默退化为「所有字段缺失」。
        return Ok((T::default(), JsonMap::new()));
    }
    let JsonValue::Object(raw) = value else {
        return Err(bad_request("invalid request body"));
    };
    let typed: T = serde_json::from_value(JsonValue::Object(raw.clone()))
        .map_err(|_| bad_request("invalid request body"))?;
    Ok((typed, raw))
}

// ---------------------------------------------------------------------------
// GET /api/agents/
// ---------------------------------------------------------------------------

/// `GET /api/agents/`（上游 `ListAgents`）。
pub(super) async fn list_agents(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<AgentDto>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let include_archived = query.get("include_archived").map(String::as_str) == Some("true");
    let rows = scope
        .repo
        .list(scope.workspace_id, include_archived)
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    let ids: Vec<_> = rows.iter().map(|a| a.id).collect();
    let targets = scope.targets_by_agent(&ids).await?;
    let visible = scope.filter_accessible(rows, &targets);
    Ok(Json(
        visible
            .iter()
            .map(|row| {
                let t = targets.get(&row.id).cloned().unwrap_or_default();
                AgentDto::from_row(row, &scope, &t)
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// POST /api/agents/
// ---------------------------------------------------------------------------

/// `POST /api/agents/`（上游 `CreateAgent`，成功 201）。
pub(super) async fn create_agent(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<AgentDto>)> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let (req, raw) = decode_body::<CreateAgentRequest>(&body)?;

    if req.name.is_empty() {
        return Err(bad_request("name is required").into());
    }
    if req.description.chars().count() > mc_repos::agent::MAX_DESCRIPTION_LEN {
        return Err(bad_request(format!(
            "description must be {} characters or fewer",
            mc_repos::agent::MAX_DESCRIPTION_LEN
        ))
        .into());
    }
    let Some(runtime_id_raw) = req.runtime_id.as_deref().filter(|v| !v.is_empty()) else {
        return Err(bad_request("runtime_id is required").into());
    };
    let starters =
        normalise_conversation_starters(&req.conversation_starters).map_err(bad_request)?;
    // 上游：`visibility == "" → "private"`，然后 `parsePermissionInput` 在
    // legacy 回退路径上读到它，因此这里恒有 resolved 权限。
    let legacy_visibility = req
        .visibility
        .clone()
        .unwrap_or_else(|| VISIBILITY_PRIVATE.to_string());
    let has_targets = raw.contains_key("invocation_targets");
    let has_permission_mode = raw.contains_key("permission_mode");
    let permission = parse_permission_input(
        scope.workspace_id.0,
        req.permission_mode.as_deref(),
        has_permission_mode,
        &req.invocation_targets,
        has_targets,
        Some(legacy_visibility.as_str()),
    )
    .map_err(bad_request)?
    .ok_or_else(|| bad_request("invalid request body"))?;

    let max_concurrent_tasks =
        default_and_validate_max_concurrent_tasks(&raw, req.max_concurrent_tasks)?;

    let runtime_uuid = parse_uuid(runtime_id_raw, "runtime_id")?;
    let runtime = scope.runtime_binding(runtime_uuid).await?;
    if !scope.can_use_runtime(&runtime) {
        return Err(Error::Forbidden {
            message: "this runtime is private; only its owner can create agents on it".into(),
        }
        .into());
    }
    // 掩码哨兵在 create 上没有旧值可恢复 —— 直接丢弃（上游同样处理）。
    let mut runtime_config = req
        .runtime_config
        .clone()
        .unwrap_or_else(|| JsonValue::Object(JsonMap::new()));
    preserve_masked_gateway_token(&mut runtime_config, &JsonValue::Null);

    let new = NewAgent {
        workspace_id: scope.workspace_id,
        name: req.name.clone(),
        description: req.description.clone(),
        instructions: req.instructions.clone(),
        avatar_url: req.avatar_url.clone(),
        runtime_mode: runtime.runtime_mode.clone(),
        runtime_id: Some(runtime.id),
        runtime_config: Some(runtime_config),
        visibility: permission.legacy_visibility(),
        permission_mode: permission.mode.clone(),
        max_concurrent_tasks: Some(max_concurrent_tasks),
        owner_id: Some(scope.user_id.0),
        custom_env: Some(json_object_from(req.custom_env.as_ref())),
        custom_args: Some(req.custom_args.clone().map_or_else(
            || JsonValue::Array(Vec::new()),
            |v| serde_json::to_value(v).unwrap_or(JsonValue::Array(Vec::new())),
        )),
        mcp_config: req.mcp_config.clone(),
        model: non_empty_opt(req.model.as_deref()),
        thinking_level: non_empty_opt(req.thinking_level.as_deref()),
        service_tier: non_empty_opt(req.service_tier.as_deref()),
        conversation_starters: Some(
            serde_json::to_value(&starters).unwrap_or(JsonValue::Array(Vec::new())),
        ),
        composio_toolkit_allowlist: req
            .composio_toolkit_allowlist
            .as_ref()
            .map(|v| normalise_composio_allowlist(v)),
    };

    let created = scope
        .repo
        .create(&new)
        .await
        .map_err(|e| create_conflict(e, &req.name))?;
    scope
        .repo
        .replace_invocation_targets(created.id(), Some(scope.user_id.0), &permission.targets)
        .await
        .map_err(|e| repo_err(e, "agent"))?;

    let targets = scope.targets_of(created.id()).await?;
    Ok((
        StatusCode::CREATED,
        Json(AgentDto::from_row(&created, &scope, &targets)),
    ))
}

/// `custom_env` 为 `None` 时上游落 `{}`。
fn json_object_from(env: Option<&BTreeMap<String, String>>) -> JsonValue {
    match env {
        Some(map) => {
            serde_json::to_value(map).unwrap_or_else(|_| JsonValue::Object(JsonMap::new()))
        }
        None => JsonValue::Object(JsonMap::new()),
    }
}

fn non_empty_opt(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.is_empty()).map(str::to_string)
}

/// 上游 `agent_workspace_name_unique` 冲突 → 409；其余按仓储错误映射。
fn create_conflict(err: mc_repos::RepoError, name: &str) -> Error {
    match err {
        mc_repos::RepoError::Conflict => Error::Conflict {
            message: format!("an agent named {name:?} already exists in this workspace"),
        },
        other => repo_err(other, "agent"),
    }
}

/// 上游 `defaultAndValidateAgentMaxConcurrentTasks`：字段缺失/`null` → 默认 6。
fn default_and_validate_max_concurrent_tasks(
    raw: &JsonMap<String, JsonValue>,
    submitted: Option<i32>,
) -> Result<i32, Error> {
    let value = match raw.get("max_concurrent_tasks") {
        None | Some(JsonValue::Null) => DEFAULT_MAX_CONCURRENT_TASKS,
        Some(_) => submitted.unwrap_or(DEFAULT_MAX_CONCURRENT_TASKS),
    };
    mc_repos::agent::validate_max_concurrent_tasks(value).map_err(bad_request)?;
    Ok(value)
}

// ---------------------------------------------------------------------------
// GET /api/agents/{id}/
// ---------------------------------------------------------------------------

/// `GET /api/agents/:id/`（上游 `GetAgent`，私有 agent 对无权限成员 403）。
pub(super) async fn get_agent(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<AgentDto>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    let targets = scope.targets_of(agent.id()).await?;
    scope.require_can_access_private(&agent, &targets)?;
    Ok(Json(AgentDto::from_row(&agent, &scope, &targets)))
}

// ---------------------------------------------------------------------------
// PUT /api/agents/{id}/
// ---------------------------------------------------------------------------

/// `PUT /api/agents/:id/`（上游 `UpdateAgent`）。
pub(super) async fn update_agent(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<AgentDto>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let existing = scope.load_agent(&id).await?;
    scope.require_can_manage(&existing)?;
    let (req, raw) = decode_body::<UpdateAgentRequest>(&body)?;

    // upstream 硬拒 `custom_env`（防止「以为轮换了密钥」的静默丢字段）。
    if raw.contains_key("custom_env") {
        return Err(bad_request(
            "custom_env is no longer accepted on this endpoint; use PUT /api/agents/{id}/env \
             (or `multica agent env set`)",
        )
        .into());
    }

    let mut patch = AgentUpdatePatch {
        id: existing.id(),
        ..Default::default()
    };
    // 上游那四个 `ClearAgent*` 查询：需要写 NULL 的列在这里排队，
    // `update` 之后逐列执行（COALESCE 更新无法表达「显式置空」）。
    let mut clears: Vec<NullableAgentField> = Vec::new();
    apply_scalar_fields(&mut patch, &mut clears, &req, &raw, &existing)?;
    apply_composio_allowlist(&mut patch, &mut clears, &scope, &existing, &req, &raw);
    apply_runtime(&scope, &mut patch, &req).await?;
    let replace_targets = apply_permission(&scope, &mut patch, &existing, &req, &raw).await?;

    let updated = scope.repo.update(&patch).await.map_err(|e| match e {
        mc_repos::RepoError::Conflict => Error::Conflict {
            message: format!(
                "an agent named {:?} already exists in this workspace",
                req.name.clone().unwrap_or_default()
            ),
        },
        other => repo_err(other, "agent"),
    })?;
    let mut updated = updated;
    for field in clears {
        updated = scope
            .repo
            .clear_nullable(updated.id(), field)
            .await
            .map_err(|e| repo_err(e, "agent"))?;
    }
    if let Some(permission) = replace_targets {
        scope
            .repo
            .replace_invocation_targets(updated.id(), Some(scope.user_id.0), &permission.targets)
            .await
            .map_err(|e| repo_err(e, "agent"))?;
    }

    let targets = scope.targets_of(updated.id()).await?;
    Ok(Json(AgentDto::from_row(&updated, &scope, &targets)))
}

/// 普通字段（含 `mcp_config` 的 null 语义）。
fn apply_scalar_fields(
    patch: &mut AgentUpdatePatch,
    clears: &mut Vec<NullableAgentField>,
    req: &UpdateAgentRequest,
    raw: &JsonMap<String, JsonValue>,
    existing: &AgentRow,
) -> Result<(), Error> {
    if let Some(name) = req.name.clone() {
        patch.name = Some(name);
    }
    if let Some(description) = req.description.clone() {
        if description.chars().count() > mc_repos::agent::MAX_DESCRIPTION_LEN {
            return Err(bad_request(format!(
                "description must be {} characters or fewer",
                mc_repos::agent::MAX_DESCRIPTION_LEN
            )));
        }
        patch.description = Some(description);
    }
    if let Some(instructions) = req.instructions.clone() {
        patch.instructions = Some(instructions);
    }
    if let Some(starters) = req.conversation_starters.as_ref() {
        let normalised = normalise_conversation_starters(starters).map_err(bad_request)?;
        patch.conversation_starters =
            Some(serde_json::to_value(&normalised).unwrap_or(JsonValue::Array(Vec::new())));
    }
    if let Some(avatar) = req.avatar_url.clone() {
        patch.avatar_url = Some(avatar);
    }
    if req.runtime_config.is_some() {
        let mut incoming = req.runtime_config.clone().unwrap_or(JsonValue::Null);
        // `***` 往返保护：拿库里的真实 token 还原提交上来的掩码哨兵，
        // 否则 UI 的 GET→PUT 会把 `***` 当成真 token 写进库（上游 issue #3260）。
        preserve_masked_gateway_token(&mut incoming, &existing.runtime_config);
        patch.runtime_config = Some(incoming);
    }
    if let Some(args) = req.custom_args.clone() {
        patch.custom_args =
            Some(serde_json::to_value(&args).unwrap_or(JsonValue::Array(Vec::new())));
    }
    if let Some(status) = req.status.clone() {
        patch.status = Some(status);
    }
    if let Some(max) = req.max_concurrent_tasks {
        mc_repos::agent::validate_max_concurrent_tasks(max).map_err(bad_request)?;
        patch.max_concurrent_tasks = Some(max);
    }
    if let Some(model) = req.model.clone() {
        patch.model = Some(model);
    }
    // tri-state：缺失不动；null → 清空；对象 → 覆盖（上游用 rawFields 判 null）。
    match req.mcp_config.clone() {
        Some(Some(value)) => patch.mcp_config = Some(Some(value)),
        // 显式 `null`（`double_option` 的 `Some(None)`）或 `raw` 里存在该键但没解出值
        // → 清空该列。
        Some(None) => clears.push(NullableAgentField::McpConfig),
        None => {
            if raw.contains_key("mcp_config") {
                clears.push(NullableAgentField::McpConfig);
            }
        }
    }
    apply_optional_overrides(patch, clears, req);
    Ok(())
}

/// `thinking_level` / `service_tier` 的三态处理。
///
/// 上游在这里还要按 provider 枚举校验 `thinking_level` / `service_tier`；本片
/// 不做（provider 目录属 M3-2/M3-4），只保留「空串 = 清空、非空 = 置值」的契约。
fn apply_optional_overrides(
    patch: &mut AgentUpdatePatch,
    clears: &mut Vec<NullableAgentField>,
    req: &UpdateAgentRequest,
) {
    if let Some(level) = req.thinking_level.clone() {
        if level.is_empty() {
            clears.push(NullableAgentField::ThinkingLevel);
        } else {
            patch.thinking_level = Some(Some(level));
        }
    }
    if let Some(tier) = req.service_tier.clone() {
        if tier.is_empty() {
            clears.push(NullableAgentField::ServiceTier);
        } else {
            patch.service_tier = Some(Some(tier));
        }
    }
}

/// `composio_toolkit_allowlist`：**owner-only 写入**（admin 不能改别人的集成白名单），
/// 三态同上游：缺失不动 / `null` 清空 / 列表整体替换。
///
/// 偏离：上游先看 `composioMCPAppsEnabled` 特性开关，关闭时直接丢弃写入；
/// 本仓以列本身为准，没有这个开关（docs/40 §5）。
fn apply_composio_allowlist(
    patch: &mut AgentUpdatePatch,
    clears: &mut Vec<NullableAgentField>,
    scope: &AgentScope,
    existing: &AgentRow,
    req: &UpdateAgentRequest,
    raw: &JsonMap<String, JsonValue>,
) {
    if !raw.contains_key("composio_toolkit_allowlist") || !scope.is_agent_owner(existing) {
        return;
    }
    match req.composio_toolkit_allowlist.clone() {
        None | Some(None) => clears.push(NullableAgentField::ComposioToolkitAllowlist),
        Some(Some(list)) => {
            patch.composio_toolkit_allowlist = Some(normalise_composio_allowlist(&list));
        }
    }
}

/// 上游 `UpdateAgent` 的 runtime 换绑分支。
async fn apply_runtime(
    scope: &AgentScope,
    patch: &mut AgentUpdatePatch,
    req: &UpdateAgentRequest,
) -> Result<(), Error> {
    let Some(raw_runtime) = req.runtime_id.as_deref() else {
        return Ok(());
    };
    let runtime_uuid = parse_uuid(raw_runtime, "runtime_id")?;
    let runtime = scope.runtime_binding(runtime_uuid).await?;
    if !scope.can_use_runtime(&runtime) {
        return Err(Error::Forbidden {
            message: "this runtime is private; only its owner can move agents onto it".into(),
        });
    }
    patch.runtime_id = Some(runtime.id);
    patch.runtime_mode = Some(runtime.runtime_mode.clone());
    Ok(())
}

/// 上游的 invocation 权限写入（owner-only，非 owner 的真实改动 → 403）。
async fn apply_permission(
    scope: &AgentScope,
    patch: &mut AgentUpdatePatch,
    existing: &AgentRow,
    req: &UpdateAgentRequest,
    raw: &JsonMap<String, JsonValue>,
) -> Result<Option<ResolvedPermission>, Error> {
    let has_permission_mode = raw.contains_key("permission_mode");
    let has_targets = raw.contains_key("invocation_targets");
    if !(has_permission_mode || has_targets || req.visibility.is_some()) {
        return Ok(None);
    }
    let targets = scope.targets_of(existing.id()).await?;
    if !scope.is_agent_owner(existing) {
        let changed = permission_input_changes_agent(
            existing,
            &targets,
            req,
            has_permission_mode,
            has_targets,
        );
        if changed {
            return Err(Error::Forbidden {
                message:
                    "only the agent owner can change access (permission_mode / invocation_targets)"
                        .into(),
            });
        }
        return Ok(None);
    }
    let submitted = req.invocation_targets.clone().unwrap_or_default();
    let resolved = parse_permission_input(
        existing.workspace_id,
        req.permission_mode.as_deref(),
        has_permission_mode,
        &submitted,
        has_targets,
        req.visibility.as_deref(),
    )
    .map_err(bad_request)?;
    let Some(resolved) = resolved else {
        return Ok(None);
    };
    patch.permission_mode = Some(resolved.mode.clone());
    patch.visibility = Some(resolved.legacy_visibility());
    Ok(Some(resolved))
}

/// 上游 `permissionInputChangesAgent`：非 owner 的「无改动重放」放行，真实改动 403。
fn permission_input_changes_agent(
    existing: &AgentRow,
    current: &[AgentInvocationTargetRow],
    req: &UpdateAgentRequest,
    has_permission_mode: bool,
    has_targets: bool,
) -> bool {
    if !has_permission_mode && !has_targets {
        let Some(visibility) = req.visibility.as_deref() else {
            return false;
        };
        let submitted = if visibility == "workspace" {
            "workspace"
        } else {
            "private"
        };
        let derived = derive_legacy_visibility(&existing.permission_mode, current);
        return submitted != derived;
    }
    let submitted = req.invocation_targets.clone().unwrap_or_default();
    let Ok(Some(want)) = parse_permission_input(
        existing.workspace_id,
        req.permission_mode.as_deref(),
        has_permission_mode,
        &submitted,
        has_targets,
        req.visibility.as_deref(),
    ) else {
        // 无法解析 / 实际上没有权限字段 → 视为无改动（上游同样处理）。
        return false;
    };
    if want.mode != existing.permission_mode {
        return true;
    }
    let want: Vec<String> = want
        .targets
        .iter()
        .map(|(t, id)| format!("{t}:{id}"))
        .collect();
    let have: Vec<String> = current
        .iter()
        .map(|row| format!("{}:{}", row.target_type, row.target_id))
        .collect();
    want.len() != have.len() || want.iter().any(|k| !have.contains(k))
}

// ---------------------------------------------------------------------------
// archive / restore / cancel-tasks / tasks
// ---------------------------------------------------------------------------

/// `POST /api/agents/:id/archive`（上游 `ArchiveAgent`）。
pub(super) async fn archive_agent(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<AgentDto>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage(&agent)?;
    if agent.is_archived() {
        return Err(Error::Conflict {
            message: "agent is already archived".into(),
        }
        .into());
    }
    if agent.system_key.as_deref().is_some_and(|k| !k.is_empty()) {
        return Err(bad_request("this agent is built into Multica and cannot be archived").into());
    }
    let archived = scope
        .repo
        .archive(agent.id(), Some(scope.user_id.0))
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    // 归档连带取消在飞任务（上游 `CancelTasksForArchivedAgent`）；失败不阻断归档。
    if let Err(err) = scope.repo.cancel_tasks(agent.id()).await {
        tracing::warn!(error = %err, "cancel agent tasks on archive failed");
    }
    let targets = scope.targets_of(archived.id()).await?;
    Ok(Json(AgentDto::from_row(&archived, &scope, &targets)))
}

/// `POST /api/agents/:id/restore`（上游 `RestoreAgent`）。
pub(super) async fn restore_agent(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<AgentDto>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage(&agent)?;
    if !agent.is_archived() {
        return Err(Error::Conflict {
            message: "agent is not archived".into(),
        }
        .into());
    }
    let restored = scope
        .repo
        .restore(agent.id())
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    let targets = scope.targets_of(restored.id()).await?;
    Ok(Json(AgentDto::from_row(&restored, &scope, &targets)))
}

/// `POST /api/agents/:id/cancel-tasks`（上游 `CancelAgentTasks`）。
pub(super) async fn cancel_agent_tasks(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<CancelTasksDto>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage(&agent)?;
    let cancelled = scope
        .repo
        .cancel_tasks(agent.id())
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    Ok(Json(CancelTasksDto { cancelled }))
}

/// `GET /api/agents/:id/tasks`（上游 `ListAgentTasks`）。
pub(super) async fn list_agent_tasks(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<AgentTaskDto>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    let targets = scope.targets_of(agent.id()).await?;
    scope.require_can_access_private(&agent, &targets)?;
    // 上游在此校验 `include_usage`；本片不水合用量（M3-6），但仍保留参数契约。
    match query.get("include_usage").map(String::as_str) {
        None | Some("" | "false" | "true") => {}
        Some(_) => return Err(bad_request("include_usage must be true or false").into()),
    }
    let tasks = scope
        .repo
        .list_tasks(agent.id())
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    Ok(Json(tasks.iter().map(AgentTaskDto::from_row).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_body_is_rejected_like_upstream() {
        let err = decode_body::<UpdateAgentRequest>(&Bytes::new()).unwrap_err();
        assert!(matches!(err, Error::Validation { .. }));
    }

    #[test]
    fn null_body_degrades_to_defaults() {
        let (req, raw) = decode_body::<UpdateAgentRequest>(&Bytes::from_static(b"null")).unwrap();
        assert!(req.name.is_none());
        assert!(raw.is_empty());
    }

    #[test]
    fn max_concurrent_tasks_defaults_and_validates() {
        let raw = JsonMap::new();
        assert_eq!(
            default_and_validate_max_concurrent_tasks(&raw, None).unwrap(),
            DEFAULT_MAX_CONCURRENT_TASKS
        );
        let mut provided = JsonMap::new();
        provided.insert("max_concurrent_tasks".into(), JsonValue::from(12));
        assert_eq!(
            default_and_validate_max_concurrent_tasks(&provided, Some(12)).unwrap(),
            12
        );
        assert!(default_and_validate_max_concurrent_tasks(&provided, Some(0)).is_err());
        assert!(default_and_validate_max_concurrent_tasks(&provided, Some(51)).is_err());
    }

    #[test]
    fn explicit_null_max_concurrent_tasks_falls_back_to_default() {
        let mut raw = JsonMap::new();
        raw.insert("max_concurrent_tasks".into(), JsonValue::Null);
        assert_eq!(
            default_and_validate_max_concurrent_tasks(&raw, None).unwrap(),
            DEFAULT_MAX_CONCURRENT_TASKS
        );
    }

    #[test]
    fn conflict_message_quotes_the_name() {
        let err = create_conflict(mc_repos::RepoError::Conflict, "bot");
        match err {
            Error::Conflict { message } => {
                assert_eq!(
                    message,
                    "an agent named \"bot\" already exists in this workspace"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
