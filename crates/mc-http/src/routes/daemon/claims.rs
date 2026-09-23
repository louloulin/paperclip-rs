//! claim 面（M3-7 / LUM-1438）。
//!
//! 覆盖 7 条路由：
//!
//! | 路由 | upstream |
//! |---|---|
//! | `POST /api/daemon/runtimes/:runtimeId/tasks/claim` | `ClaimTaskByRuntime` `daemon.go:3707` |
//! | `POST /api/daemon/tasks/claim` | `ClaimTasksByRuntime` `daemon.go:1706` |
//! | `POST /api/daemon/claim` | 同上（同一 handler，历史别名） |
//! | `POST …/tasks/:taskId/prepare-lease` | `ExtendTaskPrepareLease` `daemon.go:4043` |
//! | `POST …/tasks/:taskId/skill-bundles/resolve` | `ResolveTaskSkillBundles` `daemon.go:3908` |
//! | `GET …/tasks/pending` | `ListPendingTasksByRuntime` `daemon.go:4013` |
//! | `POST …/recover-orphans` | `RecoverOrphanedTasks` `task_lifecycle.go:27` |
//!
//! ## 单条 claim 是 `{"task": …}`，批量是 `{"tasks": […]}`（不是笔误）
//!
//! upstream 两条路径**故意**不同形：`ClaimTaskByRuntime` 回 `{"task": resp}` /
//! `{"task": null}`，`ClaimTasksByRuntime` 回 `{"tasks": [...]}`（空也回空数组；
//! `max_tasks == 0` 时直接短路、不打库）。daemon 侧两个解析器都只认自己那个键。
//!
//! ## 发 token 是 claim 的唯一凭据副作用
//!
//! claim 成功后签一枚 24h 的 `mat_` task token，绑定 `(agent, task, workspace,
//! runtime owner)` 落 `task_token`；daemon 把它注入 agent 进程。因此：
//!
//! - runtime `owner_id IS NULL` ⇒ **不发**（否则会发一枚无 scope 的凭据，MUL-3292），
//!   先把任务取消掉再回 500 `runtime owner required to mint task token`；
//! - token 生成 / 落库失败 ⇒ 把**这一次** claim 放回队列
//!   （`requeue_task_after_claim_failure`），绝不留下一条 `dispatched` 而没人跑的孤行；
//! - 任务终结（complete / fail / ack-cancel）时删掉它的全部 token。
//!
//! ## 本切片不做的部分（逐条登记在 `docs/32` 的偏离表）
//!
//! - `delivered_comment_ids` 的**投递回执落库**：上游由更宽的 claim builder
//!   （`buildClaimedTaskResponse` / `finalizeClaimDelivery`）在同一事务里算并写回；
//!   本切片不建 comment plan，因此回执 = 行上已有的值（响应与库一致）。
//! - `remote_mcp_daemon_token`：插件面恒禁用 ⇒ 恒空串。
//! - stale comment plan 修复（`repairStaleCommentPlanIfNeeded`）。
//! - `issue_wakeup` 的 revision 活性门（见 `claim_next_task_for_runtime` 的 SQL 注释）。
//! - `recover-orphans` 的后续流水线只做到「清 task token」；上游还会回滚 agent 状态并
//!   触发 auto-retry（在 RuntimeSweeper 面）。

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::Json;
use mc_core::Id;
use mc_repos::daemon::{DaemonRepo, SkillBundleRow};
use mc_repos::runtime::AgentRuntimeRow;
use mc_repos::task::TaskRow;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

use super::dto::{decode_body, BatchClaimRequest, DaemonTaskResponse};
use super::scope::{
    conflict, db_err, forbidden, internal, not_found, require_runtime_access, require_task_access,
    validation, DaemonActor, DaemonAuth,
};
use super::skills::{build_bundle, SOURCE_WORKSPACE};
use crate::error::ApiResult;
use crate::state::AppState;

/// prepare lease 的续租窗口（秒）—— upstream `prepareLeaseTTL` 同值 90s。
const PREPARE_LEASE_SECS: i64 = 90;
/// claim 对 runtime 新鲜度的要求（秒）：心跳期 30s 的三倍。
const RUNTIME_STALE_SECS: i64 = 90;
/// task token 有效期（upstream `24 * time.Hour`）。
const TASK_TOKEN_TTL_SECS: i64 = 24 * 60 * 60;
/// 批量 claim 上限（upstream `claimBatchMaxTasksCap`）：daemon 传再大也只给这么多。
const CLAIM_BATCH_MAX_TASKS_CAP: i64 = 32;

// ---------------------------------------------------------------------------
// task token
// ---------------------------------------------------------------------------

/// upstream `auth.GenerateAgentTaskToken`：`"mat_" + hex(20 random bytes)`。
fn generate_task_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("mat_{}", hex::encode(bytes))
}

/// upstream `auth.HashToken`：`hex(sha256(token))`。明文只回给 daemon 一次。
fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// claim 成功后发 token；`Ok(None)` 表示 runtime 无 owner（调用方要取消任务）。
async fn mint_task_token(
    repo: &DaemonRepo,
    row: &TaskRow,
    workspace_id: Id,
    runtime_owner: Option<Id>,
) -> Result<Option<String>, mc_repos::RepoError> {
    let Some(owner) = runtime_owner else {
        return Ok(None);
    };
    let token = generate_task_token();
    repo.insert_task_token(
        &hash_token(&token),
        row.id(),
        row.agent_id(),
        workspace_id,
        owner,
        TASK_TOKEN_TTL_SECS,
    )
    .await?;
    Ok(Some(token))
}

// ---------------------------------------------------------------------------
// 单条 claim
// ---------------------------------------------------------------------------

/// `POST /api/daemon/runtimes/:runtimeId/tasks/claim`（upstream `ClaimTaskByRuntime`）。
pub(crate) async fn claim_for_runtime(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(runtime_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let repo = DaemonRepo::new(&state.db);
    let runtime = require_runtime_access(&state, &auth, &runtime_id, "runtime not found").await?;
    let workspace_id = runtime.workspace_id;

    let claimed = repo
        .claim_next_task_for_runtime(runtime.id, PREPARE_LEASE_SECS, RUNTIME_STALE_SECS)
        .await
        .map_err(db_err)?;
    let Some(row) = claimed else {
        // 空手而归不是错误：上游回 200 `{"task": null}`，daemon 据此继续轮询。
        return Ok(Json(json!({ "task": Value::Null })));
    };

    // 防御性复核：任务解析出的 workspace 必须就是这台 runtime 的（上游同一行）。
    if repo.task_workspace_id(row.id()).await.map_err(db_err)? != Some(workspace_id) {
        let _ = repo.requeue_task_after_claim_failure(row.id()).await;
        return Err(not_found("task not found"));
    }

    let token = match mint_task_token(&repo, &row, workspace_id, runtime.owner_id).await {
        Ok(Some(token)) => token,
        Ok(None) => {
            tracing::error!(task_id = %row.id(), "claim: runtime has no owner; cancelling task");
            let _ = repo.cancel_task(row.id()).await;
            return Err(internal("runtime owner required to mint task token"));
        }
        Err(e) => {
            // 放回队列后 5xx：daemon 重试能救回来，而一条没人跑的 `dispatched` 会
            // 一直占着 issue 的并发槽直到 prepare lease 过期。
            let _ = repo.requeue_task_after_claim_failure(row.id()).await;
            return Err(db_err(e));
        }
    };

    let mut resp = DaemonTaskResponse::from_row(&row, &workspace_id.to_string());
    resp.auth_token = token;
    Ok(Json(json!({ "task": resp })))
}

// ---------------------------------------------------------------------------
// 批量 claim
// ---------------------------------------------------------------------------

/// `POST /api/daemon/tasks/claim` 与 `POST /api/daemon/claim`（同一 handler）。
pub(crate) async fn claim_batch(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let repo = DaemonRepo::new(&state.db);
    let req: BatchClaimRequest = decode_body(&body)?;

    if req.daemon_id.is_empty() {
        return Err(validation("daemon_id is required"));
    }
    // `mdt_` token 自带 daemon 身份 ⇒ 必须与 body 一致，否则一枚同 workspace 的
    // token 能替别的机器领任务。
    if let Some(token_daemon) = auth.actor.daemon_id() {
        if token_daemon != req.daemon_id.as_str() {
            return Err(forbidden("daemon_id does not match token"));
        }
    }
    if req.max_tasks < 0 {
        return Err(validation("max_tasks must not be negative"));
    }
    // 显式 0 = 明确不领（不折算成 1），且不打库。
    if req.max_tasks == 0 {
        return Ok(Json(json!({ "tasks": [] })));
    }
    let max_tasks = req.max_tasks.min(CLAIM_BATCH_MAX_TASKS_CAP) as usize;

    // 坏 id / 未知 id 一律**跳过**而不是整批 400：daemon 会捎上本地已知的全部
    // runtime，其中属于别人的、已被删除的很常见。
    let mut ids: Vec<Id> = Vec::new();
    let mut seen = HashSet::new();
    for raw in &req.runtime_ids {
        if let Ok(parsed) = uuid::Uuid::parse_str(raw.trim()) {
            if seen.insert(parsed) {
                ids.push(Id::from(parsed));
            }
        }
    }
    if ids.is_empty() {
        return Ok(Json(json!({ "tasks": [] })));
    }

    let found = repo.list_runtimes_by_ids(&ids).await.map_err(db_err)?;
    let by_id: HashMap<Id, AgentRuntimeRow> =
        found.into_iter().map(|rt| (rt.id, rt)).collect();

    let mut authorized: Vec<AgentRuntimeRow> = Vec::new();
    for id in &ids {
        let Some(rt) = by_id.get(id) else { continue };
        if !workspace_allowed(&state, &auth, rt.workspace_id).await? {
            continue;
        }
        // 钉在别的 daemon 上的 runtime 不能被本机 claim；`daemon_id IS NULL`
        // （云端 runtime）不受机器钉定，保持可领。
        if let Some(bound) = &rt.daemon_id {
            if bound != &req.daemon_id {
                continue;
            }
        }
        authorized.push(rt.clone());
    }
    if authorized.is_empty() {
        return Ok(Json(json!({ "tasks": [] })));
    }

    let mut out: Vec<DaemonTaskResponse> = Vec::new();
    'outer: for rt in authorized {
        while out.len() < max_tasks {
            let claimed = repo
                .claim_next_task_for_runtime(rt.id, PREPARE_LEASE_SECS, RUNTIME_STALE_SECS)
                .await
                .map_err(db_err)?;
            let Some(row) = claimed else { break };
            if repo.task_workspace_id(row.id()).await.map_err(db_err)? != Some(rt.workspace_id) {
                let _ = repo.requeue_task_after_claim_failure(row.id()).await;
                continue;
            }
            let token = match mint_task_token(&repo, &row, rt.workspace_id, rt.owner_id).await {
                Ok(Some(token)) => token,
                Ok(None) => {
                    tracing::error!(task_id = %row.id(), "batch claim: runtime has no owner; cancelling task");
                    let _ = repo.cancel_task(row.id()).await;
                    continue;
                }
                Err(e) => {
                    let _ = repo.requeue_task_after_claim_failure(row.id()).await;
                    return Err(db_err(e));
                }
            };
            let mut resp = DaemonTaskResponse::from_row(&row, &rt.workspace_id.to_string());
            resp.auth_token = token;
            out.push(resp);
            if out.len() >= max_tasks {
                break 'outer;
            }
        }
    }
    Ok(Json(json!({ "tasks": out })))
}

/// 该 runtime 的 workspace 是否对该调用者可见（daemon token 只认自己的，用户查 member）。
async fn workspace_allowed(
    state: &AppState,
    auth: &DaemonAuth,
    workspace_id: Id,
) -> ApiResult<bool> {
    match &auth.actor {
        DaemonActor::Daemon {
            workspace_id: token_ws,
            ..
        } => Ok(*token_ws == workspace_id),
        DaemonActor::User { user_id, .. } => DaemonRepo::new(&state.db)
            .is_workspace_member(workspace_id, *user_id)
            .await
            .map_err(db_err),
    }
}

// ---------------------------------------------------------------------------
// prepare-lease / pending / recover-orphans
// ---------------------------------------------------------------------------

/// `POST /api/daemon/runtimes/:runtimeId/tasks/:taskId/prepare-lease`。
pub(crate) async fn prepare_lease(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path((runtime_id, task_id)): Path<(String, String)>,
) -> ApiResult<Json<DaemonTaskResponse>> {
    let repo = DaemonRepo::new(&state.db);
    let runtime = require_runtime_access(&state, &auth, &runtime_id, "runtime not found").await?;
    let (task, task_workspace) = require_task_access(&state, &auth, &task_id, "task not found").await?;
    if task_workspace != runtime.workspace_id
        || task.runtime_id != Some(runtime.id.as_uuid())
    {
        return Err(not_found("task not found"));
    }
    let updated = repo
        .extend_prepare_lease(task.id(), runtime.id, PREPARE_LEASE_SECS)
        .await
        .map_err(db_err)?;
    // SQL 的 CAS（`status IN ('dispatched','waiting_local_directory') AND started_at IS NULL`）
    // 不命中 ⇒ 上游回 400 `err.Error()`（即 CAS 的报错文案）。
    let Some(updated) = updated else {
        return Err(validation("task is not preparing"));
    };
    Ok(Json(DaemonTaskResponse::from_row(
        &updated,
        &runtime.workspace_id.to_string(),
    )))
}

/// `GET /api/daemon/runtimes/:runtimeId/tasks/pending` —— 响应是**裸数组**（无包层）。
pub(crate) async fn list_pending(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(runtime_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let repo = DaemonRepo::new(&state.db);
    let runtime = require_runtime_access(&state, &auth, &runtime_id, "runtime not found").await?;
    let workspace = runtime.workspace_id.to_string();
    let rows = repo.list_pending_tasks(runtime.id).await.map_err(db_err)?;
    let out: Vec<DaemonTaskResponse> = rows
        .iter()
        .map(|row| DaemonTaskResponse::from_row(row, &workspace))
        .collect();
    Ok(Json(serde_json::to_value(out).unwrap_or(Value::Array(vec![]))))
}

/// `POST /api/daemon/runtimes/:runtimeId/recover-orphans`。
pub(crate) async fn recover_orphans(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(runtime_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let repo = DaemonRepo::new(&state.db);
    let runtime = require_runtime_access(&state, &auth, &runtime_id, "runtime not found").await?;
    let rows = repo.recover_orphaned_tasks(runtime.id).await.map_err(db_err)?;
    let orphaned = rows.len();
    for row in &rows {
        // 上一次进程留下的 task token 必须立刻作废：否则它会活到 24h 过期，且
        // 绑的是已经不存在的执行。
        let _ = repo.delete_task_tokens_by_task(row.id()).await;
    }
    // `retried` 恒 0：auto-retry 属 RuntimeSweeper 面（`docs/32` 偏离表）。
    Ok(Json(json!({ "orphaned": orphaned, "retried": 0 })))
}

// ---------------------------------------------------------------------------
// skill-bundles/resolve
// ---------------------------------------------------------------------------

/// upstream `resolveSkillBundlesRequest` 里的一项 ref。
#[derive(Debug, Clone, Default, Deserialize)]
struct ResolveRef {
    #[serde(default)]
    id: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    hash: String,
}

/// upstream `resolveSkillBundlesRequest`。
#[derive(Debug, Clone, Default, Deserialize)]
struct ResolveRequest {
    #[serde(default)]
    skills: Vec<ResolveRef>,
}

/// `POST /api/daemon/runtimes/:runtimeId/tasks/:taskId/skill-bundles/resolve`。
pub(crate) async fn resolve_skill_bundles(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path((runtime_id, task_id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let repo = DaemonRepo::new(&state.db);
    let runtime = require_runtime_access(&state, &auth, &runtime_id, "runtime not found").await?;
    let (task, task_workspace) =
        require_task_access(&state, &auth, &task_id, "task not found").await?;
    if task_workspace != runtime.workspace_id
        || task.runtime_id != Some(runtime.id.as_uuid())
    {
        return Err(not_found("task not found"));
    }
    if task.status != "dispatched" && task.status != "waiting_local_directory" {
        return Err(conflict("task is not preparing"));
    }

    let req: ResolveRequest = decode_body(&body)?;
    if req.skills.is_empty() {
        return Ok(Json(json!({ "bundles": [] })));
    }
    // 先校验再读：坏 ref 历来就是 400，不该先付一次读的代价。
    let mut wanted = Vec::with_capacity(req.skills.len());
    for r in &req.skills {
        if r.id.is_empty() || r.source.is_empty() || r.hash.is_empty() {
            return Err(validation("invalid skill ref"));
        }
        let Ok(parsed) = uuid::Uuid::parse_str(r.id.trim()) else {
            return Err(validation("invalid skill ref"));
        };
        wanted.push(Id::from(parsed));
    }

    let loaded: Vec<SkillBundleRow> = repo
        .skill_bundles_for_agent(task.agent_id(), &wanted)
        .await
        .map_err(|e| {
            // 5xx 而不是部分答案：daemon 的 resolve 重试能救回瞬时读失败，而一个
            // 「读失败拼出来的」bundle 会通过客户端校验并被当成完整缓存下来。
            tracing::error!(task_id = %task_id, error = %e, "resolve skill bundles failed");
            internal("failed to load skill bundles")
        })?;

    let mut by_id: HashMap<Uuid, (mc_repos::daemon::SkillRow, Vec<(String, String)>)> = loaded
        .into_iter()
        .map(|b| (b.skill.id, (b.skill, b.files)))
        .collect();

    let mut resolved = Vec::with_capacity(req.skills.len());
    for r in &req.skills {
        let key = uuid::Uuid::parse_str(r.id.trim()).unwrap_or_default();
        let Some((skill, files)) = by_id.remove(&key) else {
            return Err(not_found("skill bundle not found"));
        };
        // 本仓只实现 `workspace` 源：builtin / plugin 的 ref 与「不存在」同一处理。
        // 由此，上游对插件源的那道 pinned-hash 校验在本切片**不可达**（已登记在
        // `docs/32` 偏离表），不伪造一个恒不成立的比较。
        if r.source != SOURCE_WORKSPACE {
            return Err(not_found("skill bundle not found"));
        }
        let bundle = build_bundle(&SkillBundleRow { skill, files });
        resolved.push(serde_json::to_value(&bundle).unwrap_or(Value::Null));
    }
    Ok(Json(json!({ "bundles": resolved })))
}
