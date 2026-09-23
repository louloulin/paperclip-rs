//! 9 条 runtime 台账路由（上游 `handler/runtime.go` L936-1188、L1196-1386）。
//!
//! | method | path | 上游 |
//! |---|---|---|
//! | GET | `/api/runtimes/`（+ 无斜杠别名） | `ListAgentRuntimes` |
//! | PATCH | `/api/runtimes/:runtimeId/`（+ 无斜杠别名） | `UpdateAgentRuntime` |
//! | DELETE | `/api/runtimes/:runtimeId/`（+ 无斜杠别名） | `DeleteAgentRuntime` |
//! | POST | `/api/runtimes/:runtimeId/unbind-agents-and-delete` | `UnbindAgentsAndDeleteRuntime` |
//! | POST | `/api/runtimes/:runtimeId/archive-agents-and-delete` | 同上（旧客户端入口） |
//!
//! 这一族**没有**路由级角色中间件（上游同款）：handler 自己判成员 + `canEditRuntime`。
//! 三条 `PATCH`/`DELETE` 的尾斜杠别名与上游 chi 的「集合/单资源两种写法都命中」一致
//! （`matchit 0.7` 下 `/x` 与 `/x/` 是两条不同的路由，只注册一种会让另一种 404，
//! 而 ⑦ 的 `route_parity.py` 会把两种写法折叠成一条、看不见这个故障）。
//!
//! ## 权限的三个层次（别合并它们）
//!
//! - **看列表**：成员即可；`?owner=me` → 只要自己的；owner/admin → 全部；其余成员 →
//!   自己的 + `public`。治理可见 ≠ 可用。
//! - **改 / 删**：`canEditRuntime` = owner/admin 任何行，普通成员只有自己的。
//! - **翻可见性**：只有 owner 本人（`canSetRuntimeVisibility`）。admin 若能翻，等于把
//!   `canUseRuntimeForAgent` 里取消掉的豁免又拿回来 —— 跑在别人机器上花的是他的凭据。
//!
//! ## 有意偏离
//!
//! 不广播 WS 事件（`EventDaemonRegister` / `NotifyRuntimeGone` / teardown 扇出属 M3-7）；
//! 删除路径把「活跃 agent 拒绝」放在事务内一次判定（上游先预检、再在锁内复查），
//! 状态码与拒绝体一致。详见 `docs/39-M3-4-RUNTIME-PROFILES.md` §4。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::runtime::{
    AgentRuntimeRepo, AgentRuntimeRow, DeleteRuntimeError, RuntimeListFilter, RuntimeProfileRepo,
    RuntimeProfileRow,
};
use serde::Deserialize;

use super::access::{
    decode_body, forbidden, load_member, load_member_for_runtime, not_found, parse_path_id,
    repo_err, resolve_workspace, validation, WorkspaceQuery,
};
use super::dto::{AgentRuntimeDto, UnbindAgentsAndDeleteRequest, UpdateRuntimeRequest};
use super::refusals::{self, DeleteScope, InstanceBlockers};
use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// upstream `maxRuntimeCustomNameLen`：`custom_name` 的**字符**（非字节）上限。
const MAX_CUSTOM_NAME_LEN: usize = 100;

fn repo(state: &AppState) -> AgentRuntimeRepo {
    AgentRuntimeRepo::new(state.db.clone())
}

/// `GET /api/runtimes/` 的查询串：workspace 选择器（header 优先）+ `?owner=me`。
#[derive(Debug, Default, Deserialize)]
pub(super) struct ListQuery {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_slug: Option<String>,
    #[serde(default)]
    owner: Option<String>,
}

// ---------------------------------------------------------------------------
// GET /api/runtimes/
// ---------------------------------------------------------------------------

/// upstream `ListAgentRuntimes`：回**裸 JSON 数组**（不是 `{"runtimes":[...]}`），
/// 按 `created_at ASC`。
pub(super) async fn list_runtimes(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<AgentRuntimeDto>>> {
    let workspace_id = resolve_workspace(
        &state,
        &headers,
        &WorkspaceQuery {
            workspace_id: query.workspace_id,
            workspace_slug: query.workspace_slug,
        },
    )
    .await?;
    let member = load_member(&state, workspace_id, auth.id()).await?;
    let filter = if query.owner.as_deref() == Some("me") {
        RuntimeListFilter::Owner(member.user_id)
    } else if member.is_admin() {
        RuntimeListFilter::All
    } else {
        RuntimeListFilter::Visible(member.user_id)
    };
    let rows = repo(&state)
        .list(workspace_id, filter)
        .await
        .map_err(|e| repo_err(e, "runtime"))?;
    Ok(Json(rows.iter().map(AgentRuntimeDto::from_row).collect()))
}

// ---------------------------------------------------------------------------
// PATCH / PUT /api/runtimes/:runtimeId/
// ---------------------------------------------------------------------------

/// upstream `UpdateAgentRuntime`：`visibility` + `custom_name`（+ `apply_to_machine`）。
/// PATCH-as-PUT 容错：原样回显未变的 `visibility` 会被**丢弃**而不是 403。
pub(super) async fn update_runtime(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(runtime): Path<String>,
    body: Bytes,
) -> ApiResult<Json<AgentRuntimeDto>> {
    let mut rt = super::access::load_runtime(&state, &runtime).await?;
    let member = load_member_for_runtime(&state, &rt, auth.id()).await?;
    if !member.can_edit_runtime(&rt) {
        return Err(forbidden("you can only edit your own runtimes").into());
    }
    let req: UpdateRuntimeRequest = decode_body(&body)?;

    // 任何变更落地之前先把**全部**字段校验完：一个字段非法不能让 PATCH 半应用。
    let mut new_visibility = None;
    if let Some(visibility) = req.visibility.as_deref() {
        if visibility != "private" && visibility != "public" {
            return Err(validation("visibility must be 'private' or 'public'").into());
        }
        // 只有「真的变了」才受权限门约束。
        if visibility != rt.visibility {
            if !member.can_set_visibility(&rt) {
                return Err(forbidden("only the runtime owner can change its visibility").into());
            }
            new_visibility = Some(visibility.to_string());
        }
    }
    let new_custom_name = match req.custom_name.as_deref() {
        Some(name) => {
            let trimmed = name.trim();
            if trimmed.chars().count() > MAX_CUSTOM_NAME_LEN {
                return Err(validation("custom name is too long").into());
            }
            // 空 / 全空白 = 清掉覆盖值（回落到 daemon 提的 `name`）。
            Some(if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            })
        }
        None => None,
    };

    let repo = repo(&state);
    if let Some(visibility) = new_visibility {
        rt = repo
            .set_visibility(rt.id, &visibility)
            .await
            .map_err(|e| repo_err(e, "runtime"))?;
    }
    if let Some(custom_name) = new_custom_name {
        rt = apply_custom_name(
            &repo,
            &rt,
            custom_name.as_deref(),
            req.apply_to_machine,
            &member,
        )
        .await?;
    }
    Ok(Json(AgentRuntimeDto::from_row(&rt)))
}

/// `custom_name` 的单行 / 整机两条写入路径（上游 `UpdateAgentRuntimeCustomName*`）。
///
/// 整机改名只在 `apply_to_machine && daemon_id` 都成立时走；普通成员只能用
/// `owner_filter` 改自己在同一 `daemon_id` 下的行。
async fn apply_custom_name(
    repo: &AgentRuntimeRepo,
    rt: &AgentRuntimeRow,
    value: Option<&str>,
    apply_to_machine: bool,
    member: &super::access::RuntimeMember,
) -> Result<AgentRuntimeRow, Error> {
    let daemon_id = rt.daemon_id.clone().filter(|_| apply_to_machine);
    let Some(daemon_id) = daemon_id else {
        return repo
            .set_custom_name(rt.id, value)
            .await
            .map_err(|e| repo_err(e, "runtime"));
    };
    let owner_filter = if member.is_admin() {
        None
    } else {
        Some(member.user_id)
    };
    let rows = repo
        .set_custom_name_by_daemon(rt.workspace_id, &daemon_id, owner_filter, value)
        .await
        .map_err(|e| repo_err(e, "runtime"))?;
    // actor 一定在更新集合里（它 own 或 admin 着 `:id` 那行）；防御性重取一次，
    // 免得回一个过期行。
    if let Some(row) = rows.into_iter().find(|row| row.id == rt.id) {
        return Ok(row);
    }
    repo.get(rt.id)
        .await
        .map_err(|e| repo_err(e, "runtime"))?
        .ok_or_else(|| not_found("runtime"))
}

// ---------------------------------------------------------------------------
// DELETE /api/runtimes/:runtimeId/
// ---------------------------------------------------------------------------

/// upstream `DeleteAgentRuntime`：**严格**删除 —— 有活跃 agent 就是 409，
/// 用户确认走 `unbind-agents-and-delete`。成功体是 `{"status":"ok"}`。
pub(super) async fn delete_runtime(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(runtime): Path<String>,
) -> ApiResult<Response> {
    let rt = super::access::load_runtime(&state, &runtime).await?;
    let member = load_member_for_runtime(&state, &rt, auth.id()).await?;
    if !member.can_edit_runtime(&rt) {
        return Err(forbidden("you can only delete your own runtimes").into());
    }

    if let Some(profile) = live_profile(&state, &rt).await? {
        return Ok(refusals::profile_instance_delete_unsupported(
            &rt,
            &profile,
            &instance_blockers(&repo(&state), rt.id).await,
        ));
    }
    if rt.profile_id.is_some() {
        // profile 行已经没了（孤儿实例）：仍可删，但这是数据异常的信号。
        tracing::warn!(
            runtime_id = %rt.id.as_string(),
            "deleting orphaned profile-backed runtime instance"
        );
    }

    match repo(&state).delete_strict(rt.id).await {
        Ok(_) => Ok(Json(serde_json::json!({ "status": "ok" })).into_response()),
        Err(DeleteRuntimeError::HasActiveAgents(agents)) => {
            Ok(refusals::runtime_has_active_agents(&agents))
        }
        Err(DeleteRuntimeError::PlanChanged(agents)) => {
            Ok(refusals::runtime_delete_plan_changed(&agents))
        }
        Err(DeleteRuntimeError::NotDrained) => {
            Ok(refusals::runtime_delete_not_drained(DeleteScope::Runtime))
        }
        Err(DeleteRuntimeError::WorkspaceMismatch) => Ok(
            refusals::runtime_delete_workspace_mismatch(DeleteScope::Runtime),
        ),
        Err(DeleteRuntimeError::NotFound) => Err(not_found("runtime").into()),
        Err(DeleteRuntimeError::Db(message)) => Err(Error::Database(message).into()),
    }
}

// ---------------------------------------------------------------------------
// POST /api/runtimes/:runtimeId/{unbind,archive}-agents-and-delete
// ---------------------------------------------------------------------------

/// upstream `UnbindAgentsAndDeleteRuntime`：确认后的级联删除。
///
/// `expected_active_agent_ids` 是用户刚在弹窗里确认过的快照，服务端在事务内与实时
/// 集合比对 —— 队友在「打开弹窗 → 确认」之间动了 agent 就回
/// `runtime_delete_plan_changed` + 最新清单，**保证用户批准的就是即将被解绑的那一组**。
///
/// 与旧文案不同，这里**不再归档**：agent 解绑后存活（只是需要重新绑一个 runtime），
/// 只有 system agent 会被硬删。响应里的 `agents_archived` 是给旧客户端的兼容镜像。
pub(super) async fn unbind_agents_and_delete(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(runtime): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    // 顺序与上游逐字一致：路径 → body → expected ids → 取行 → 成员 → 权限 → profile。
    let runtime_id = parse_path_id("runtime_id", &runtime)?;
    let req: UnbindAgentsAndDeleteRequest = decode_body(&body)?;
    let expected = parse_expected_active_agent_ids(&req.expected_active_agent_ids)?;

    let rt = repo(&state)
        .get(runtime_id)
        .await
        .map_err(|e| repo_err(e, "runtime"))?
        .ok_or_else(|| not_found("runtime"))?;
    let member = load_member_for_runtime(&state, &rt, auth.id()).await?;
    if !member.can_edit_runtime(&rt) {
        return Err(forbidden("you can only delete your own runtimes").into());
    }

    if let Some(profile) = live_profile(&state, &rt).await? {
        return Ok(refusals::profile_instance_delete_unsupported(
            &rt,
            &profile,
            &instance_blockers(&repo(&state), rt.id).await,
        ));
    }
    if rt.profile_id.is_some() {
        tracing::warn!(
            runtime_id = %rt.id.as_string(),
            "deleting orphaned profile-backed runtime instance via cascade"
        );
    }

    match repo(&state)
        .unbind_agents_and_delete(rt.id, &expected)
        .await
    {
        Ok(outcome) => Ok(Json(serde_json::json!({
            "status": "ok",
            "agents_unbound": outcome.agents_unbound,
            "tasks_cancelled": outcome.tasks_cancelled,
            "autopilots_paused": outcome.autopilots_paused,
            // 已废弃的 `agents_unbound` 镜像：装在旧契约上的客户端读这个键。
            "agents_archived": outcome.agents_unbound,
        }))
        .into_response()),
        Err(DeleteRuntimeError::PlanChanged(agents)) => {
            Ok(refusals::runtime_delete_plan_changed(&agents))
        }
        Err(DeleteRuntimeError::NotDrained) => {
            Ok(refusals::runtime_delete_not_drained(DeleteScope::Runtime))
        }
        Err(DeleteRuntimeError::HasActiveAgents(agents)) => {
            Ok(refusals::runtime_has_active_agents(&agents))
        }
        // 确认路径上游把 workspace mismatch 当 500 处理；本地仓储把它与 strict 路径
        // 统一成枚举，这里按 strict 同款 409 回（拒绝体语义更准确，见 docs/39-M3-4-RUNTIME-PROFILES.md §4）。
        Err(DeleteRuntimeError::WorkspaceMismatch) => Ok(
            refusals::runtime_delete_workspace_mismatch(DeleteScope::Runtime),
        ),
        Err(DeleteRuntimeError::NotFound) => Err(not_found("runtime").into()),
        Err(DeleteRuntimeError::Db(message)) => Err(Error::Database(message).into()),
    }
}

// ---------------------------------------------------------------------------
// profile 实例判定
// ---------------------------------------------------------------------------

/// upstream `runtimeLiveProfile`：`profile_id` 指向的 profile 行还在 → 返回它。
///
/// 行**不在**了（迁移 120 去掉了 DB 的 `ON DELETE CASCADE`，孤儿行是已知历史）→
/// 视作普通 runtime 继续删（MUL-4158）。
async fn live_profile(
    state: &AppState,
    rt: &AgentRuntimeRow,
) -> Result<Option<RuntimeProfileRow>, Error> {
    let Some(profile_id) = rt.profile_id else {
        return Ok(None);
    };
    RuntimeProfileRepo::new(state.db.clone())
        .get(rt.workspace_id, profile_id)
        .await
        .map_err(|e| repo_err(e, "runtime profile"))
}

/// upstream `profileInstanceRefusalBlockers`：GC 受阻的两项证据。任一读失败 → `known = false`。
///
/// GC 不只看活跃 agent：它还要跨**全部** user agent（含归档）检查 drain，所以
/// 「只报活跃集合」会许下一个 sweeper 会跳过的承诺。
async fn instance_blockers(repo: &AgentRuntimeRepo, runtime_id: Id) -> InstanceBlockers {
    let Ok(agents) = repo.list_active_agents(runtime_id).await else {
        return InstanceBlockers::default();
    };
    let Ok(undrained_tasks) = repo.count_undrained_tasks(runtime_id).await else {
        return InstanceBlockers::default();
    };
    InstanceBlockers {
        agents,
        undrained_tasks,
        known: true,
    }
}

/// upstream `parseExpectedActiveAgentIDs`：空数组是合法计划（「我确认没有活跃 agent」），
/// 任一非法 UUID → 400（**不是**静默忽略，否则会去比对另一组集合）。
fn parse_expected_active_agent_ids(raw: &[String]) -> Result<Vec<Id>, Error> {
    raw.iter().try_fold(
        Vec::with_capacity(raw.len()),
        |mut acc, value| match Id::parse(value) {
            Ok(id) => {
                acc.push(id);
                Ok(acc)
            }
            Err(_) => Err(validation(
                "expected_active_agent_ids must be a list of valid UUIDs",
            )),
        },
    )
}
