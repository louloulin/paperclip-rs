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
//!   触发 auto-retry（在 `RuntimeSweeper` 面）。

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::Json;
use mc_core::Id;
use mc_repos::daemon::DaemonRepo;
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
    validation, workspace_allowed, DaemonAuth,
};
use super::skills::{
    build_agent_bundle, build_builtin_bundle, parse_source, SkillBundleData, SOURCE_BUILTIN,
};
use crate::error::ApiResult;
use crate::state::AppState;
use mc_core::skill::SkillSource;
use mc_repos::skill::binding::SkillBindingRepo;

/// prepare lease 的续租窗口（秒）—— upstream `prepareLeaseTTL` 同值 90s。
const PREPARE_LEASE_SECS: i64 = 90;
/// claim 对 runtime 新鲜度的要求（秒）：心跳期 30s 的三倍。
const RUNTIME_STALE_SECS: i64 = 90;
/// task token 有效期（upstream `24 * time.Hour`）。
const TASK_TOKEN_TTL_SECS: i64 = 24 * 60 * 60;
/// 批量 claim 上限（upstream `claimBatchMaxTasksCap`）：daemon 传再大也只给这么多。
const CLAIM_BATCH_MAX_TASKS_CAP: i64 = 32;
/// poll-hint 查询用的 runtime 新鲜度（upstream `service.RuntimeClaimFreshnessSeconds`）。
const RUNTIME_CLAIM_FRESHNESS_SECS: f64 = 150.0;
/// poll-hint 的最小延迟（upstream `claimPollHintMinDelay = time.Second`）。
const CLAIM_POLL_HINT_MIN_DELAY_MS: i64 = 1_000;

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
///
/// 本函数只做 HTTP 特有的那部分：从 `X-Client-Capabilities` 头取能力串。业务语义全在
/// [`claim_batch_core`]，WS 面（[`super::ws`]）以连接身份的能力串调同一个核心。
pub(crate) async fn claim_batch(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let req: BatchClaimRequest = decode_body(&body)?;
    // 头缺失/非法 utf8 ⇒ 空串（`requestClientCapabilities` 对坏头也只看到空）。
    let capabilities = headers
        .get("x-client-capabilities")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let out = claim_batch_core(&state, Some(&auth), &capabilities, req).await?;
    Ok(Json(out))
}

/// 批量 claim 的**唯一**实现：HTTP 腿与 WS RPC 腿共用（见 `docs/16` §6.4）。
///
/// `auth = None` = 连接没有任何可鉴权主体（只声明了 `runtime_ids`），此时没有任何
/// runtime 可见 —— 与上游逐个 runtime 走 `requireDaemonWorkspaceAccess` 失败的最终结果
/// 相同（200 空列表），但少付 N 次 member 查询。
///
/// `capabilities` 是 `X-Client-Capabilities` 原文（WS 面 = 连接身份的能力串）。
#[allow(clippy::too_many_lines)] // 上游单函数顺序照搬：mismatch → 空领短路 → 逐条认领 → 组装
pub(crate) async fn claim_batch_core(
    state: &AppState,
    auth: Option<&DaemonAuth>,
    capabilities: &str,
    req: BatchClaimRequest,
) -> ApiResult<Value> {
    let repo = DaemonRepo::new(&state.db);

    if req.daemon_id.is_empty() {
        return Err(validation("daemon_id is required"));
    }
    // `mdt_` token 自带 daemon 身份 ⇒ 必须与 body 一致，否则一枚同 workspace 的
    // token 能替别的机器领任务。
    if let Some(token_daemon) = auth.and_then(|a| a.actor.daemon_id()) {
        if token_daemon != req.daemon_id.as_str() {
            return Err(forbidden("daemon_id does not match token"));
        }
    }
    if req.max_tasks < 0 {
        return Err(validation("max_tasks must not be negative"));
    }
    // 显式 0 = 明确不领（不折算成 1），且不打库；上游这条短路也**不带** poll-hint 字段。
    if req.max_tasks == 0 {
        return Ok(json!({ "tasks": [] }));
    }
    // 负数在上一条已判 400 ⇒ 落进这里的值域是 `[0, 32]`，转换不会丢符号也不会截断。
    let max_tasks = usize::try_from(req.max_tasks.min(CLAIM_BATCH_MAX_TASKS_CAP)).unwrap_or(0);

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
        return Ok(json!({ "tasks": [] }));
    }

    let found = repo.list_runtimes_by_ids(&ids).await.map_err(db_err)?;
    let by_id: HashMap<Id, AgentRuntimeRow> = found.into_iter().map(|rt| (rt.id, rt)).collect();

    let mut authorized: Vec<AgentRuntimeRow> = Vec::new();
    for id in &ids {
        let Some(rt) = by_id.get(id) else { continue };
        if let Some(auth) = auth {
            if !workspace_allowed(state, auth, rt.workspace_id).await? {
                continue;
            }
        } else {
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
        return Ok(json!({ "tasks": [] }));
    }
    let authorized_ids: Vec<Id> = authorized.iter().map(|rt| rt.id).collect();

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

    // 安全轮询提示（upstream `daemon.go:1941-1952`）：只在**没领满**且调用方声明了
    // `claim-poll-hints-v1` 时才查一次「最近的 deferred 任务 fire_at」。查询失败只记日志、
    // 省略支持位（daemon 保守地退回自己的 `PollInterval`），绝不让一次提示查询把整次
    // claim 变成 5xx —— 任务已经发出去了，回 5xx 只会让 daemon 重复 claim。
    let mut response = json!({ "tasks": out });
    let claimed = response["tasks"].as_array().map_or(0, Vec::len);
    if claimed < max_tasks
        && mc_daemon_proto::capabilities::request_has_client_capability(
            capabilities,
            mc_daemon_proto::capabilities::DAEMON_CAPABILITY_CLAIM_POLL_HINTS_V1,
        )
    {
        match repo
            .next_deferred_task_fire_at(&authorized_ids, RUNTIME_CLAIM_FRESHNESS_SECS)
            .await
        {
            Ok(next) => {
                response["claim_poll_hint_supported"] = json!(true);
                if let Some(fire_at) = next {
                    response["next_deferred_task_after_ms"] =
                        json!(claim_poll_hint_delay_ms(chrono::Utc::now(), fire_at));
                }
            }
            Err(err) => tracing::warn!(
                error = %err,
                "batch claim: next deferred task lookup failed; retaining short client poll"
            ),
        }
    }
    Ok(response)
}

/// upstream `claimPollHintDelay`（`daemon.go:1954`）：不得低于 `claimPollHintMinDelay`
/// （1s），否则「立刻到期」的 deferred 任务会让 daemon 打成紧轮询。
fn claim_poll_hint_delay_ms(
    now: chrono::DateTime<chrono::Utc>,
    fire_at: chrono::DateTime<chrono::Utc>,
) -> i64 {
    let delay = (fire_at - now).num_milliseconds();
    delay.max(CLAIM_POLL_HINT_MIN_DELAY_MS)
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
    let (task, task_workspace) =
        require_task_access(&state, &auth, &task_id, "task not found").await?;
    if task_workspace != runtime.workspace_id || task.runtime_id != Some(runtime.id.as_uuid()) {
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
    Ok(Json(
        serde_json::to_value(out).unwrap_or(Value::Array(vec![])),
    ))
}

/// `POST /api/daemon/runtimes/:runtimeId/recover-orphans`。
pub(crate) async fn recover_orphans(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(runtime_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let repo = DaemonRepo::new(&state.db);
    let runtime = require_runtime_access(&state, &auth, &runtime_id, "runtime not found").await?;
    let rows = repo
        .recover_orphaned_tasks(runtime.id)
        .await
        .map_err(db_err)?;
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
    let runtime = require_runtime_access(&state, &auth, &runtime_id, "runtime not found").await?;
    let (task, task_workspace) =
        require_task_access(&state, &auth, &task_id, "task not found").await?;
    if task_workspace != runtime.workspace_id || task.runtime_id != Some(runtime.id.as_uuid()) {
        return Err(not_found("task not found"));
    }
    if task.status != "dispatched" && task.status != "waiting_local_directory" {
        return Err(conflict("task is not preparing"));
    }

    let req: ResolveRequest = decode_body(&body)?;
    if req.skills.is_empty() {
        return Ok(Json(json!({ "bundles": [] })));
    }

    // `invalid skill ref` 只覆盖**空字段**（上游 handler 的那道门）；id 的形态由下面
    // 的按源解析决定 —— 校验与解析分开，才能让 `builtin:<name>`（不是 uuid）可解析。
    for r in &req.skills {
        if r.id.is_empty() || r.source.is_empty() || r.hash.is_empty() {
            return Err(validation("invalid skill ref"));
        }
    }

    // workspace / plugin 共用一次 `ListAgentSkillsByIDs`（同一张表、同一条授权谓词）；
    // builtin 不查库（清单在编译期就定下了）。
    let wanted: Vec<Uuid> = req
        .skills
        .iter()
        .filter(|r| r.source != SOURCE_BUILTIN)
        .filter_map(|r| Uuid::parse_str(&r.id).ok())
        .collect();

    let mut resolved: HashMap<String, SkillBundleData> = HashMap::with_capacity(req.skills.len());
    if !wanted.is_empty() {
        let loaded = SkillBindingRepo::new(state.db.clone())
            .skill_bundles_for_agent(task.agent_id(), &wanted)
            .await
            .map_err(|e| {
                // 5xx 而不是部分答案：daemon 的 resolve 重试能救回瞬时读失败，而一个
                // 「读失败拼出来的」bundle 会通过客户端校验并被当成完整缓存下来。
                tracing::error!(task_id = %task_id, error = %e, "resolve skill bundles failed");
                internal("failed to load skill bundles")
            })?;
        for row in &loaded {
            let bundle = build_agent_bundle(row);
            let source = if row.is_plugin() {
                SkillSource::Plugin
            } else {
                SkillSource::Workspace
            };
            resolved.insert(
                mc_skill::builtin::agent_skill_bundle_key(source, &bundle.id),
                bundle,
            );
        }
    }

    let mut out = Vec::with_capacity(req.skills.len());
    for r in &req.skills {
        // 三个常量之外的源没有服务端生产者（上游 `LoadRequestedAgentSkillBundles` 的
        // `switch` 只认 builtin / workspace）⇒ 与「查不到」同一处理。
        let Some(source) = parse_source(&r.source) else {
            return Err(not_found("skill bundle not found"));
        };
        let bundle = if source == SkillSource::Builtin {
            build_builtin_bundle(&r.id)
        } else {
            // 键用**原始** ref id（不是规范化后的 uuid 文本）：上游也是用 ref.ID 去查
            // `resolved` 这张按行 id 建的 map，所以非规范形态的 id 落 not-found。
            resolved.remove(&mc_skill::builtin::agent_skill_bundle_key(source, &r.id))
        };
        let Some(bundle) = bundle else {
            return Err(not_found("skill bundle not found"));
        };
        // 插件的 pinned hash 是**客户端缓存键**：装了插件的那台机器按安装时的版本缓存
        // 过内容，服务端换了版本必须让它重新拉，而不是悄悄喂给它一份不同内容。
        // 上游 `ResolveTaskSkillBundles` 里这道门一直存在，只是它前面的 `switch`
        // 没有 plugin 分支 ⇒ 在 M6-4 让 plugin 可解析之前，程序里没有路径能走到它。
        if source == SkillSource::Plugin && bundle.hash != r.hash {
            return Err(conflict("pinned plugin skill bundle hash mismatch"));
        }
        out.push(serde_json::to_value(&bundle).unwrap_or(Value::Null));
    }
    Ok(Json(json!({ "bundles": out })))
}
