//! daemon 生命周期路由：register / deregister / heartbeat / ws /
//! workspaces / repos / runtime-profiles（R7 拆分自 `daemon.rs`）。
//!
//! 上游落点：`server/internal/handler/daemon.go`（register L405 / deregister L939 /
//! heartbeat L1089 / ws `daemon_ws.go:12`）、`daemon_workspace.go:27`、
//! `runtime_profile.go:640`、`daemon_rpc.go:45`。

use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_core::Id;
use mc_daemon_proto::capabilities::SERVER_HEARTBEAT_CAPABILITIES;
use mc_daemon_proto::messages::daemon::{
    DaemonHeartbeatAckPayload, DaemonHeartbeatPendingLocalSkillImport,
    DaemonHeartbeatPendingLocalSkills, DaemonHeartbeatPendingModelList,
    DaemonHeartbeatPendingUpdate, HEARTBEAT_STATUS_RUNTIME_GONE,
};
use mc_errors::Error;
use mc_repos::daemon::{DaemonRepo, RuntimeUpsert, UpsertRuntime};
use mc_ws::identity::ClientIdentity;
use serde_json::{json, Value};
use std::sync::Arc;

use super::dto::{
    decode_body, DeregisterRequest, HeartbeatRequest, RegisterRequest, RegisterRuntime,
};
use super::scope::{
    db_err, internal, normalize_provider, not_found, parse_path_id, require_workspace_access,
    validation, DaemonActor, DaemonAuth,
};
use crate::daemon_requests::RequestKind;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// `X-Client-Capabilities`：逗号分隔，去空（upstream `requestClientCapabilities`）。
fn client_capabilities(headers: &HeaderMap) -> Vec<String> {
    headers
        .get("x-client-capabilities")
        .and_then(|v| v.to_str().ok())
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// upstream `requireWorkspaceMember(w, r, ws, "workspace not found")` 的本地等价物：
/// 返回 `Some(user_id)`（owner）或 `None`（已写过响应 / 非成员）。
async fn member_owner(
    state: &AppState,
    user_id: Id,
    workspace_id: Id,
) -> Result<Option<Id>, ApiError> {
    let repo = DaemonRepo::new(&state.db);
    let is_member = repo
        .is_workspace_member(workspace_id, user_id)
        .await
        .map_err(db_err)?;
    Ok(is_member.then_some(user_id))
}

// ---------------------------------------------------------------------------
// POST /api/daemon/register
// ---------------------------------------------------------------------------

/// upstream `DaemonRegister`（`daemon.go:405`）。
#[allow(clippy::too_many_lines)] // 注册是本面最长的一条：校验 + upsert + 重建 + 迁移 + 唤醒
pub(crate) async fn register(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let mut req: RegisterRequest = decode_body(&body)?;
    req.workspace_id = req.workspace_id.trim().to_string();
    req.daemon_id = req.daemon_id.trim().to_string();
    req.device_name = req.device_name.trim().to_string();

    if req.daemon_id.is_empty() {
        return Err(validation("daemon_id is required"));
    }
    if req.workspace_id.is_empty() {
        return Err(validation("workspace_id is required"));
    }
    if req.runtimes.is_empty() && req.failed_profiles.is_empty() {
        return Err(validation(
            "at least one runtime or failed profile is required",
        ));
    }
    let workspace_id = parse_path_id("workspace_id", &req.workspace_id)?;

    // workspace 门 + owner 解析。daemon token 直接证明 workspace（owner 留空，
    // upsert 的 COALESCE 会保留既有 owner）；用户身份要过 member 表并把 owner 带上。
    let owner_id = match auth.actor.clone() {
        DaemonActor::Daemon {
            workspace_id: token_ws,
            ..
        } => {
            if token_ws != workspace_id {
                return Err(not_found("workspace not found"));
            }
            None
        }
        DaemonActor::User { user_id, .. } => Some(
            member_owner(&state, user_id, workspace_id)
                .await?
                .ok_or_else(|| not_found("workspace not found"))?,
        ),
    };

    let repo = DaemonRepo::new(&state.db);
    let workspace = repo
        .workspace_repos(workspace_id)
        .await
        .map_err(db_err)?
        .ok_or_else(|| not_found("workspace not found"))?;

    let capabilities = client_capabilities(&headers);
    let mut responses: Vec<Value> = Vec::with_capacity(req.runtimes.len());

    for runtime in &req.runtimes {
        let mut provider = normalize_provider(&runtime.kind);
        if provider.is_empty() {
            provider = "unknown".into();
        }
        let name = runtime_display_name(runtime, &req.device_name, &provider);
        let device_info = device_info(&req.device_name, &runtime.version);
        let status = if runtime.status == "offline" {
            "offline"
        } else {
            "online"
        };
        let metadata = registration_metadata(&runtime.version, &req, &capabilities, None, None);

        let is_custom = !runtime.profile_id.trim().is_empty();
        let upsert = if is_custom {
            let profile_id = parse_path_id("profile_id", runtime.profile_id.trim())?;
            let result = repo
                .upsert_runtime_with_profile(&UpsertRuntime {
                    workspace_id,
                    daemon_id: req.daemon_id.clone(),
                    name,
                    provider: provider.clone(),
                    runtime_mode: "local".into(),
                    status: status.into(),
                    device_info,
                    metadata,
                    owner_id,
                    profile_id: Some(profile_id),
                })
                .await;
            match result {
                Ok(out) => {
                    provider.clone_from(&out.row.provider);
                    out
                }
                // 未知 profile → 400（upstream `pgx.ErrNoRows` 分支）。
                Err(mc_repos::RepoError::NotFound) => {
                    return Err(validation(format!(
                        "unknown runtime profile: {}",
                        runtime.profile_id
                    )))
                }
                // profile 被停用 → 409（upstream `errRuntimeProfileDisabled`）。
                Err(mc_repos::RepoError::Conflict) => {
                    return Err(crate::error::ApiError(Error::Conflict {
                        message: format!("runtime profile is disabled: {}", runtime.profile_id),
                    }))
                }
                Err(e) => return Err(register_failure(&e)),
            }
        } else {
            repo.upsert_runtime(&UpsertRuntime {
                workspace_id,
                daemon_id: req.daemon_id.clone(),
                name,
                provider: provider.clone(),
                runtime_mode: "local".into(),
                status: status.into(),
                device_info,
                metadata,
                owner_id,
                profile_id: None,
            })
            .await
            .map_err(|e| register_failure(&e))?
        };

        let upsert = inherit_machine_custom_name(&repo, upsert).await?;

        // 只有「内置 runtime」参与 hostname-derived 身份迁移：profile 实例没有
        // hostname 祖先，且 merge 只按 provider 配对，会把内置行折进同 provider 的自定义行。
        if !is_custom {
            repo.merge_legacy_runtimes(
                workspace_id,
                &provider,
                upsert.row.id,
                &req.legacy_daemon_ids,
            )
            .await
            .map_err(db_err)?;
        }

        responses.push(to_runtime_json(&upsert.row));
    }

    // 失败 profile 的行写在成功 runtime 之后 ⇒ 在首个心跳之前它的 `last_seen_at` 最新，
    // 而 worktree 门禁读的就是最新行；capabilities 必须一并带上，否则那个窗口里这台
    // 机器看起来"什么都没声明"。
    for failed in &req.failed_profiles {
        let profile_id_raw = failed.profile_id.trim();
        if profile_id_raw.is_empty() {
            continue;
        }
        let profile_id = parse_path_id("profile_id", profile_id_raw)?;
        let reason = if failed.reason.trim().is_empty() {
            "custom runtime command could not be resolved"
        } else {
            failed.reason.trim()
        };
        let command_name = failed.command_name.trim();
        let profile_meta = repo
            .runtime_profile_meta(workspace_id, profile_id)
            .await
            .map_err(db_err)?;
        let display_name = profile_meta
            .as_ref()
            .map(|(display, _)| display.clone())
            .unwrap_or_default();
        let name = if req.device_name.is_empty() {
            display_name
        } else {
            format!("{display_name} ({})", req.device_name)
        };
        let resolved_command = if command_name.is_empty() {
            profile_meta.map(|(_, command)| command).unwrap_or_default()
        } else {
            command_name.to_string()
        };
        let metadata = registration_metadata(
            "",
            &req,
            &capabilities,
            Some((reason, &resolved_command)),
            None,
        );
        let result = repo
            .upsert_runtime_with_profile(&UpsertRuntime {
                workspace_id,
                daemon_id: req.daemon_id.clone(),
                name,
                provider: String::new(),
                runtime_mode: "local".into(),
                status: "offline".into(),
                device_info: req.device_name.clone(),
                metadata,
                owner_id,
                profile_id: Some(profile_id),
            })
            .await;
        match result {
            Ok(out) => {
                // 失败 profile 的行同样要跟住机器名，否则会把机器标题拖回 hostname。
                inherit_machine_custom_name(&repo, out).await?;
            }
            // 未知 / 已删 profile：只记日志（上游 `slog.Warn` + `continue`）。
            Err(e) => {
                tracing::warn!(
                    workspace_id = %req.workspace_id,
                    daemon_id = %req.daemon_id,
                    profile_id = profile_id_raw,
                    error = %e,
                    "failed to record runtime profile registration failure"
                );
            }
        }
    }

    // 开头已取过同一行（缺失即 404），这里不重复查库：注册不会改动 repos 行。
    let repos = workspace;

    Ok(Json(json!({
        "runtimes": responses,
        "repos": repos.repos,
        "repos_version": repos.repos_version,
        "settings": repos.settings.unwrap_or_else(|| json!({})),
    })))
}

/// upstream `AgentRuntimeResponse`（`runtime.go:25`）的 JSON 投影 —— 复用 M3-4 的
/// `AgentRuntimeDto`（同一 DTO 已在 `/api/runtimes` 台账面上线，口径不会漂）。
fn to_runtime_json(row: &mc_repos::runtime::AgentRuntimeRow) -> Value {
    serde_json::to_value(crate::routes::runtimes::dto::AgentRuntimeDto::from_row(row))
        .unwrap_or(Value::Null)
}

fn register_failure(e: &mc_repos::RepoError) -> ApiError {
    match e {
        mc_repos::RepoError::Db(message) => {
            internal(format!("failed to register runtime: {message}"))
        }
        other => internal(format!("failed to register runtime: {other:?}")),
    }
}

/// upstream `inheritMachineCustomName`（MUL-4217）：新插入且本机已有共享名时继承之。
async fn inherit_machine_custom_name(
    repo: &DaemonRepo,
    upsert: RuntimeUpsert,
) -> Result<RuntimeUpsert, ApiError> {
    if !upsert.inserted {
        return Ok(upsert);
    }
    let Some(daemon_id) = upsert.row.daemon_id.clone() else {
        return Ok(upsert);
    };
    let shared = repo
        .shared_daemon_custom_name(upsert.row.workspace_id, &daemon_id, upsert.row.id)
        .await
        .map_err(db_err)?;
    if let Some(name) = shared {
        repo.set_runtime_custom_name(upsert.row.id, &name)
            .await
            .map_err(db_err)?;
        let mut row = upsert.row;
        row.custom_name = Some(name);
        return Ok(RuntimeUpsert {
            row,
            inserted: upsert.inserted,
        });
    }
    Ok(upsert)
}

/// `name` 回落：空名 → provider（带机器名时 `"provider (device)"`）。
fn runtime_display_name(runtime: &RegisterRuntime, device_name: &str, provider: &str) -> String {
    let name = runtime.name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    if device_name.is_empty() {
        provider.to_string()
    } else {
        format!("{provider} ({device_name})")
    }
}

/// `device_info` 回落：`"<device>"`，有版本时 `"<device> · <version>"`。
fn device_info(device_name: &str, version: &str) -> String {
    let device = device_name.trim();
    if !version.is_empty() && !device.is_empty() {
        format!("{device} · {version}")
    } else if !version.is_empty() {
        version.to_string()
    } else {
        device.to_string()
    }
}

/// 注册元数据（上游两处 `json.Marshal(map[string]any{...})`）。
fn registration_metadata(
    version: &str,
    req: &RegisterRequest,
    capabilities: &[String],
    failure: Option<(&str, &str)>,
    _unused: Option<()>,
) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("version".into(), Value::String(version.to_string()));
    map.insert("cli_version".into(), Value::String(req.cli_version.clone()));
    map.insert("launched_by".into(), Value::String(req.launched_by.clone()));
    map.insert(
        "capabilities".into(),
        Value::Array(
            capabilities
                .iter()
                .map(|c| Value::String(c.clone()))
                .collect(),
        ),
    );
    if let Some((reason, command_name)) = failure {
        map.insert(
            "runtime_profile_registration_error".into(),
            Value::Bool(true),
        );
        map.insert(
            "runtime_profile_failure_reason".into(),
            Value::String(reason.to_string()),
        );
        map.insert(
            "command_name".into(),
            Value::String(command_name.to_string()),
        );
    }
    Value::Object(map)
}

// ---------------------------------------------------------------------------
// POST /api/daemon/deregister
// ---------------------------------------------------------------------------

/// upstream `DaemonDeregister`（`daemon.go:939`）：把给定 runtime 置为 `offline`。
///
/// 契约（`docs/16` §6.1）：`200 {"status":"ok"}`；400 `invalid request body`/
/// `runtime_ids is required`/`invalid runtime_ids`；500 `failed to load runtimes`。
///
/// **单条失败不影响整批**：上游对「行不在」「不属于本 workspace」「置离线写失败」都是
/// `slog.Warn` + `continue`，只有批量读出错才 500。daemon 停机时不该因为一台机器的行
/// 被删掉就让整次下线请求失败——那些 runtime 会由 liveness sweep 兜底。
pub(crate) async fn deregister(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let req: DeregisterRequest = decode_body(&body)?;
    if req.runtime_ids.is_empty() {
        return Err(validation("runtime_ids is required"));
    }
    let repo = DaemonRepo::new(&state.db);

    // 先整体校验 uuid（上游 `parseUUIDSliceOrBadRequest`：坏 id = 整批 400，不跳过）。
    let mut targets: Vec<(String, Id)> = Vec::new();
    for raw in &req.runtime_ids {
        let id = Id::parse(raw.trim()).map_err(|_| validation("invalid runtime_ids"))?;
        // 上游按 canonical uuid 去重后再批量查；本地保持「原始 id 单独查」以避免
        // 一次多余的全表比价，同时用 canonical id 去重（重复 id 不重复通知）。
        if !targets.iter().any(|(_, existing)| *existing == id) {
            targets.push((raw.clone(), id));
        }
    }

    let mut offline = 0usize;
    for (raw, runtime_id) in targets {
        // 上游批量读一次再逐条判；本地逐条读（N+1，偏离 D-6），但语义一致：
        // 读出错 = 500 `failed to load runtimes`，行不在 = warn + 跳过。
        let runtime = match repo.runtime_by_id(runtime_id).await {
            Ok(Some(runtime)) => runtime,
            Ok(None) => {
                tracing::warn!(runtime_id = %raw, "deregister: runtime not found");
                continue;
            }
            Err(_) => return Err(internal("failed to load runtimes")),
        };
        if !super::scope::workspace_allowed(&state, &auth, runtime.workspace_id).await? {
            tracing::warn!(runtime_id = %raw, "deregister: workspace mismatch");
            continue;
        }
        // 上游用**请求原文 id**（不是 canonical uuid）去 `offline_reasons` 里取值。
        let reason = req
            .offline_reasons
            .get(&raw)
            .filter(|value| !value.is_null());
        let updated = match reason {
            Some(reason) => {
                repo.set_runtime_offline_with_reason(runtime.workspace_id, runtime_id, reason)
                    .await
            }
            None => {
                repo.set_runtimes_offline(runtime.workspace_id, &[runtime_id])
                    .await
            }
        };
        match updated {
            Ok(rows) if rows.is_empty() => {
                tracing::warn!(runtime_id = %raw, "deregister: runtime not found");
            }
            Ok(rows) => {
                for id in rows {
                    // `notify_runtime_gone` 内部同时摘掉每条连接心跳 scope 里的这个 runtime。
                    state.daemon_hub.notify_runtime_gone(&id.to_string());
                    offline += 1;
                }
            }
            Err(err) => {
                tracing::warn!(runtime_id = %raw, error = %err, "deregister: failed to set offline");
            }
        }
    }

    tracing::info!(runtime_ids = ?req.runtime_ids, offline, "daemon deregistered");
    Ok(Json(json!({ "status": "ok" })))
}

// ---------------------------------------------------------------------------
// POST /api/daemon/heartbeat
// ---------------------------------------------------------------------------

/// upstream `DaemonHeartbeat`（`daemon.go:1089`）。
///
/// **HTTP ack 与 WS ack 形状不同**（`docs/16` §6.4）：HTTP 版不带 `runtime_id`
/// 与 `server_capabilities`，因为调用方已经知道自己问的是哪台。
pub(crate) async fn heartbeat(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let req: HeartbeatRequest = decode_body(&body)?;
    if req.runtime_id.trim().is_empty() {
        return Err(validation("runtime_id is required"));
    }
    let runtime = super::scope::require_runtime_access(
        &state,
        &auth,
        req.runtime_id.trim(),
        "runtime not found",
    )
    .await?;

    let repo = DaemonRepo::new(&state.db);
    let touched = repo
        .touch_runtime_heartbeat(runtime.id)
        .await
        .map_err(|_| internal("heartbeat failed"))?;
    if touched.is_none() {
        return Err(not_found("runtime not found"));
    }

    let ack = heartbeat_ack(&state, runtime.id, req.supports_batch_import);
    Ok(Json(http_ack_value(&ack)))
}

/// 心跳 ack 的**唯一**生产者：HTTP 与 WS 两条腿共用，避免字段集在两处漂移
/// （upstream `processHeartbeat`，`daemon.go:1371`）。
///
/// 取走四类待处理请求并写进 ack（`omitempty` 语义：字段缺席 = 没有待办）。本地 store
/// 是单节点内存实现，没有上游 `HasPending` 探测 / `PopPending` 取走的两段式 —— 也就
/// 没有"探测超时"那一支（偏离见 `docs/32`）。
pub(crate) fn heartbeat_ack(
    state: &AppState,
    runtime_id: Id,
    supports_batch_import: bool,
) -> DaemonHeartbeatAckPayload {
    let store = &state.daemon_requests;
    let mut ack = DaemonHeartbeatAckPayload {
        runtime_id: runtime_id.to_string(),
        status: "ok".into(),
        server_capabilities: SERVER_HEARTBEAT_CAPABILITIES
            .iter()
            .map(|c| (*c).to_string())
            .collect(),
        ..DaemonHeartbeatAckPayload::default()
    };

    if let Some(pending) = store.pop_pending(RequestKind::Update, runtime_id) {
        // `target_version` **无条件**序列化（`messages.go:423`），结果上报的 body 里也
        // 不回传它 ⇒ 只有 store 记得住它，必须在这里带出去。
        ack.pending_update = Some(DaemonHeartbeatPendingUpdate {
            id: pending.id.to_string(),
            target_version: pending.target_version.clone().unwrap_or_default(),
        });
    }
    if let Some(pending) = store.pop_pending(RequestKind::ModelList, runtime_id) {
        ack.pending_model_list = Some(DaemonHeartbeatPendingModelList {
            id: pending.id.to_string(),
        });
    }
    if let Some(pending) = store.pop_pending(RequestKind::LocalSkills, runtime_id) {
        ack.pending_local_skills = Some(DaemonHeartbeatPendingLocalSkills {
            id: pending.id.to_string(),
        });
    }
    if supports_batch_import {
        let batch =
            store.pop_pending_batch(RequestKind::LocalSkillImport, runtime_id, MAX_IMPORT_BATCH);
        // 单数键 = 第一条（老 daemon 不认复数键，必须仍然拿到一条）；复数键 = 全部。
        // 部分失败也要把已认领的条目发出去：它们已经在 store 里转成 `running` 了，
        // 扣住不发只会让请求悬挂到超时。
        ack.pending_local_skill_import = batch.first().map(skill_import_pending);
        ack.pending_local_skill_imports = batch.iter().map(skill_import_pending).collect();
    } else if let Some(pending) = store.pop_pending(RequestKind::LocalSkillImport, runtime_id) {
        ack.pending_local_skill_import = Some(skill_import_pending(&pending));
    }
    ack
}

fn skill_import_pending(
    pending: &crate::daemon_requests::PendingRequest,
) -> DaemonHeartbeatPendingLocalSkillImport {
    DaemonHeartbeatPendingLocalSkillImport {
        id: pending.id.to_string(),
        skill_key: pending.skill_key.clone().unwrap_or_default(),
    }
}

/// upstram `runtimeGoneHeartbeatAck`（`daemon.go:1240`）：runtime 行已消失。
///
/// 带 `runtime_gone: true`，且**不带** `server_capabilities`（连协议协商都免了）。
pub(crate) fn runtime_gone_ack(runtime_id: &str) -> DaemonHeartbeatAckPayload {
    DaemonHeartbeatAckPayload {
        runtime_id: runtime_id.to_string(),
        status: HEARTBEAT_STATUS_RUNTIME_GONE.into(),
        runtime_gone: true,
        ..DaemonHeartbeatAckPayload::default()
    }
}

/// WS ack → HTTP ack 的投影（upstream `DaemonHeartbeat` 结尾，`daemon.go:1185`）：
///
/// - 去掉 `runtime_id`：调用方已经知道自己问的是哪台，带上只是噪声；
/// - 去掉 `server_capabilities`：上游 HTTP 腿从不发它（协议协商只走 WS）；
/// - 其余 `pending_*` 靠 `skip_serializing_if` 自然缺席。
fn http_ack_value(ack: &DaemonHeartbeatAckPayload) -> Value {
    let Ok(mut value) = serde_json::to_value(ack) else {
        return json!({ "status": "ok" });
    };
    if let Value::Object(obj) = &mut value {
        obj.remove("runtime_id");
        obj.remove("server_capabilities");
    }
    value
}

/// upstream `maxLocalSkillImportBatch = 10`。
const MAX_IMPORT_BATCH: usize = 10;

// ---------------------------------------------------------------------------
// GET /api/daemon/ws
// ---------------------------------------------------------------------------

/// upstream `DaemonWebSocket`（`daemon_ws.go:12`）→ `Hub::handle_websocket`。
///
/// 身份在这里构造：daemon token 给出 `daemon_id` + 该机器已登记的全部 runtime id；
/// 用户身份给出 `user_id` + 全部 membership 工作区。上游在 upgrade 时做批量鉴权并把
/// 结果存成连接租约，本地不缓存（`ClientIdentity` 的 D4 说明 + `docs/32` 偏离表）。
///
/// `runtime_ids` 与 `user_id` 都为空时 `Hub` 会回 400 且**不升级**。
pub(crate) async fn ws(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    ws: WebSocketUpgrade,
) -> Response {
    let repo = DaemonRepo::new(&state.db);
    let identity = match &auth.actor {
        DaemonActor::Daemon {
            workspace_id,
            daemon_id,
        } => match repo.runtime_ids_for_daemon(*workspace_id, daemon_id).await {
            Ok(ids) => ClientIdentity {
                daemon_id: daemon_id.clone(),
                workspace_id: workspace_id.to_string(),
                runtime_ids: ids.iter().map(ToString::to_string).collect(),
                ..ClientIdentity::default()
            },
            Err(e) => return db_err(e).into_response(),
        },
        DaemonActor::User {
            user_id,
            daemon_id: _,
        } => {
            let workspaces = match repo.list_workspaces_for_user(*user_id).await {
                Ok(rows) => rows,
                Err(e) => return db_err(e).into_response(),
            };
            ClientIdentity {
                // 用户连接**不带**机器标识：上游只从 daemon 中间件（`mdt_` token）填
                // `ClientIdentity.DaemonID`（`daemon_ws.go:44`）。dev-mode 的 `X-Daemon-Id`
                // 是 HTTP 腿的偏离 D-1，不能让它在 WS 的 RPC 分发里冒充 daemon 身份。
                daemon_id: String::new(),
                user_id: user_id.to_string(),
                workspace_ids: workspaces
                    .into_iter()
                    .map(|(id, _)| id.to_string())
                    .collect(),
                ..ClientIdentity::default()
            }
        }
    };
    // 注入 WS 的两条 handler（心跳 / RPC）。放这里而不是启动期：`mount_slice_daemon()`
    // 拿不到 `Arc<AppState>`，而 handler 只在有人真的升级 WS 时才被用到；`OnceLock`
    // 保证只装一次。
    super::ws::install(&state);
    state.daemon_hub.handle_websocket(ws, identity)
}

// ---------------------------------------------------------------------------
// GET /api/daemon/workspaces
// ---------------------------------------------------------------------------

/// upstream `ListDaemonWorkspaces`（`daemon_workspace.go:27`）。
///
/// daemon token 只看得到 token 绑定的那一个 workspace；用户身份看全部 membership。
pub(crate) async fn list_workspaces(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
) -> ApiResult<Json<Value>> {
    let repo = DaemonRepo::new(&state.db);
    let rows = match &auth.actor {
        DaemonActor::Daemon { workspace_id, .. } => {
            let name = repo
                .workspace_name(*workspace_id)
                .await
                .map_err(db_err)?
                .ok_or_else(|| not_found("workspace not found"))?;
            vec![(*workspace_id, name)]
        }
        DaemonActor::User { user_id, .. } => repo
            .list_workspaces_for_user(*user_id)
            .await
            .map_err(db_err)?,
    };
    let workspaces: Vec<Value> = rows
        .into_iter()
        .map(|(id, name)| json!({ "id": id.to_string(), "name": name }))
        .collect();
    Ok(Json(json!({ "workspaces": workspaces })))
}

// ---------------------------------------------------------------------------
// GET /api/daemon/workspaces/:workspaceId/repos
// ---------------------------------------------------------------------------

/// upstream `GetDaemonWorkspaceRepos`（`daemon.go:905`）。
pub(crate) async fn workspace_repos(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(workspace_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let workspace_id = parse_path_id("workspace_id", &workspace_id)?;
    require_workspace_access(&state, &auth, workspace_id, "workspace not found").await?;
    let repos = DaemonRepo::new(&state.db)
        .workspace_repos(workspace_id)
        .await
        .map_err(db_err)?
        .ok_or_else(|| not_found("workspace not found"))?;
    Ok(Json(json!({
        "repos": repos.repos,
        "repos_version": repos.repos_version,
        "settings": repos.settings.unwrap_or_else(|| json!({})),
    })))
}

// ---------------------------------------------------------------------------
// GET /api/daemon/workspaces/:workspaceId/runtime-profiles
// ---------------------------------------------------------------------------

/// upstream `DaemonListRuntimeProfiles`（`runtime_profile.go:640`）。
///
/// 只回**启用**的 profile（daemon 拿它来决定要不要拉起自定义命令）。
pub(crate) async fn runtime_profiles(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(workspace_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let workspace_id = parse_path_id("workspace_id", &workspace_id)?;
    require_workspace_access(&state, &auth, workspace_id, "workspace not found").await?;
    let profiles = DaemonRepo::new(&state.db)
        .list_runtime_profiles(workspace_id)
        .await
        .map_err(db_err)?;
    Ok(Json(json!({ "runtime_profiles": profiles })))
}
