//! `POST /api/issues/:id/squad-evaluated`（1 条，M2-A 尾-补 / LUM-1793）。
//!
//! 上游 `server/internal/handler/squad.go:976 RecordSquadLeaderEvaluation`
//! （注册在 `server/cmd/server/router.go:2097`：`r.Post("/api/issues/{id}/squad-evaluated", …)`
//! —— **plain 注册**，不是 `Route(…)+Post("/")` ⇒ 只注册无尾斜杠形态，多注册一条就是
//! `EXTRA_ALIAS`）。
//!
//! # 这条键为什么值得单独立案
//!
//! 它是 `scripts/route_parity.py` 的 `known_gap` 里**唯一**没有 issue 承接的一条：fixture 的
//! owner 单元格写着 `M2-A`，但那是 `scripts/route-owners.tsv` 的兜底行 `^/api/issues → M2-A`
//! 按 first-match-wins 命中的产物（`/timeline`(M9)、`/attachments`(M3+)、`/quick-actions`(M3+)、
//! `/pull-requests`(M8)、`/wakeups`(M5) 等更具体的规则都排在兜底行之前 ⇒ 只有它漏到兜底）。
//! 本片除了实现，还把那条兜底命中改成**显式裁决**（`scripts/route-owners.tsv` 兜底行之前
//! 补一条带理由的规则 + `docs/22-ROUTE-PARITY.md` §7 的 rationale）。
//!
//! # 上游语义（逐条照抄，检查顺序是契约的一部分）
//!
//! | # | 上游动作 | 本地 |
//! | --- | --- | --- |
//! | 1 | `loadIssueForUser`：user → workspace → issue（非本 workspace 一律 404） | `AuthUser` + [`resolve_workspace`] + `load_issue`（非成员先 404，见 §偏离 D1） |
//! | 2 | `json.Decoder` 失败 ⇒ 400 `invalid request body` | [`parse_body`] |
//! | 3 | `outcome` 白名单外 ⇒ 400 | [`is_valid_outcome`] |
//! | 4 | `X-Task-ID` 解析失败 ⇒ 400 `invalid task id` | 同（**头**，不是 body 字段） |
//! | 5 | `GetAgentTaskInWorkspace` 失败或 `issue_id` 空 ⇒ 400 `task does not belong to issue` | `leader_task_in_workspace` 回 `None` |
//! | 6 | **闸门 1**：调用者必须是该任务的 agent，否则 403 | [`resolve_agent_actor`] + 逐字比对 |
//! | 7 | `task.issue_id != issue.id` ⇒ 400（可回显 task 的 issue id） | 同 |
//! | 8 | `!task.is_leader_task` ⇒ 400 `task is not a squad leader task` | 同 |
//! | 9 | `!task.squad_id.Valid` ⇒ 400 `leader task has no squad_id` + 一行 warn | 同（`tracing::warn!`） |
//! | 10 | `GetSquadInWorkspace` 失败 ⇒ 404 `squad not found` | `SquadRepo::find_in_workspace` → `None` |
//! | 11 | **闸门 2**：`actor_id != squad.leader_id` ⇒ 403 | 同 |
//! | 12 | `CreateActivity` 失败 ⇒ 500 `failed to record evaluation` | 同（不泄漏 DB 文案） |
//! | 13 | 201 `{id, action, created_at}` | 同 |
//!
//! **为什么闸门 1 必须先于第 7 条**（上游注释逐字，`squad.go:1043-1051`）：`GetAgentTask` 是
//! 按 id 的**全局**查询，所以「租户收窄（`GetAgentTaskInWorkspace`）+ 调用者就是这条任务的
//! agent」两道门都必须排在**任何会回显 task 派生 id 的拒绝**之前 —— 否则拿一个不相干的任务 id
//! 探一个自己能合法读的 issue，就能套出**别的 workspace** 的 issue id。本片保持这个顺序，
//! 并且 e2e 有一条专门打这个顺序的用例（§测试）。
//!
//! **授权按 TASK 行判，不按 `issue.assignee`**（上游 `MUL-6622` / `GH #7487`）：leader 合法地
//! 跑在**不是 squad 指派**的 issue 上（`@squad` mention、子 issue 上的 leader 任务），
//! 用 `issue.assignee_type == "squad"` 当闸门会让「记录判决」这条路在那些场景下**不可满足**，
//! 而 `no_action` 的规则又禁止用评论代替 ⇒ 判决会**一条痕迹都不留**。
//!
//! # 偏离（全文见 `docs/22-ROUTE-PARITY.md` §7）
//!
//! - **D1 错误信封**：本仓 `{"error":{code,message}}` + thiserror 前缀（`validation error: …`），
//!   上游是扁平 `{"error": msg}`。状态码与英文文案逐字对齐，信封沿用全仓约定。
//! - **D2 actor 解析**：上游 `resolveActor` 有三条分支（`X-Actor-Source: task_token` 盖章 /
//!   `X-Agent-ID`+`X-Task-ID` 自校验 / 其余 = member）。本仓 `/api/issues*` 面**没有**
//!   task-token 中间件（`AuthUser` 恒人类成员），所以只实现**第二条（会自己查库校验的）**
//!   分支：见 [`resolve_agent_actor`]。第一条**不实现**——它上游靠 Auth/DaemonAuth 中间件
//!   「剥掉客户端头再盖章」才成立，本地没有那层中间件，照抄等于给任何成员一个自封 agent 的
//!   开关（比第二条严格更弱）。⇒ **本地没有密码学意义的 agent 身份边界**；这条端点与上游的
//!   dev-mode 回退分支同级（上游注释自己写明：「这张回退**不是**安全边界」）。
//! - **D3 无 realtime 发布**：上游成功后 `h.publish(EventActivityCreated, …)`（事件常量本地已有：
//!   `mc-daemon-proto/src/events.rs:163`）。M2 面整波没有 realtime 发布通道（与 `pins.rs` 的
//!   D1 同款），本片不单开一条边；接线点与 payload 见 §7。
//! - **D4 抑制查询无消费者**：上游 `actor_id = task.agent_id` 是为了让
//!   `HasSquadLeaderNoActionEvaluationForTask` 能查到 `no_action` 行（好抑制 leader 的评论）。
//!   本仓没有那个 service 面 ⇒ 列里照上游放 task 的 agent，但**抑制本身不生效**。
//! - **D5 字段裁剪**：task 行只投影 handler 读的 5 列（谓词与 `JOIN agent` 逐字不变）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::squad::SquadRepo;
use mc_repos::squad_evaluation::{
    evaluation_details, is_valid_outcome, SquadEvaluationRepo, SquadLeaderTaskRow, OUTCOME_ERROR,
};
use mc_repos::RepoError;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::{not_found, require_workspace_member};
use crate::routes::issues::{
    issue_repo, load_issue, parse_target_id, resolve_workspace, validation, WorkspaceQuery,
};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 任务身份头（上游 `squad.go:1049` 的 `r.Header.Get("X-Task-ID")`）。
pub const TASK_ID_HEADER: &str = "x-task-id";
/// agent 身份头（上游 `resolveActor` 的 `r.Header.Get("X-Agent-ID")`）。
pub const AGENT_ID_HEADER: &str = "x-agent-id";

/// 「任务不属于这个 issue」（上游两句 400 共用这一段前缀，e2e 按它比对）。
pub const TASK_NOT_BELONG: &str = "task does not belong to issue";
/// 闸门 1 / 闸门 2 共用的 403 文案（上游两句一字不差，故意不区分是谁拦下的）。
pub const ONLY_LEADER_ERROR: &str = "only the squad leader agent can record evaluations";
/// `is_leader_task = false` 的 400（上游逐字）。
pub const NOT_LEADER_TASK_ERROR: &str = "task is not a squad leader task";
/// `squad_id` 为空的 400（上游逐字；`MUL-3730` 之前的行会走到这里）。
pub const NO_SQUAD_ID_ERROR: &str = "leader task has no squad_id";
/// 落库失败的 500（上游逐字，且**不回显 DB 文案**）。
pub const RECORD_FAILED_ERROR: &str = "failed to record evaluation";

// ---------------------------------------------------------------------------
// 请求 / 响应
// ---------------------------------------------------------------------------

/// 判决请求体（上游匿名 struct：`{"outcome": …, "reason": …}`）。
///
/// 两个字段都是 `Option<String>` + `#[serde(default)]`，因为上游是
/// `json.Decoder` 解进 Go 的 `string` 字段 —— **缺字段与显式 `null` 都变成空串**，
/// 也就是说三种输入（缺 / `null` / `""`）在上游落到同一条 `outcome` 白名单拒绝上。
/// 用 `String` 会让显式 `null` 在解码阶段就 400 `invalid request body`，
/// 文案与上游分叉（`docs/22` §7 D1 要求文案逐字）⇒ 这里用 [`SquadEvaluationRequest::outcome`]
/// / [`SquadEvaluationRequest::reason`] 显式还原 Go 的零值语义。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SquadEvaluationRequest {
    /// `action` | `no_action` | `failed`。
    pub outcome: Option<String>,
    /// leader 的短说明（可空）。
    pub reason: Option<String>,
}

impl SquadEvaluationRequest {
    /// Go 零值语义的 `outcome`（缺字段 / 显式 `null` ⇒ `""`）。
    #[must_use]
    pub fn outcome(&self) -> &str {
        self.outcome.as_deref().unwrap_or_default()
    }

    /// Go 零值语义的 `reason`（缺字段 / 显式 `null` ⇒ `""`）。
    #[must_use]
    pub fn reason(&self) -> &str {
        self.reason.as_deref().unwrap_or_default()
    }
}

/// 判决响应（上游 `map[string]string` ⇒ **三个字段全是字符串**）。
#[derive(Debug, Clone, Serialize)]
pub struct SquadEvaluationResponse {
    /// `activity_log.id`（`uuidToString`，所以是字符串而不是嵌套对象）。
    pub id: String,
    /// 恒 `squad_leader_evaluated`（取自回读的行，不是本地常量）。
    pub action: String,
    /// RFC3339（本仓全仓约定，见 `docs/63` §4 第 4 条）。
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// router
// ---------------------------------------------------------------------------

/// 本切片的 router（1 条注册键）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/api/issues/:id/squad-evaluated",
        post(record_squad_leader_evaluation),
    )
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `POST /api/issues/:id/squad-evaluated`（上游 `RecordSquadLeaderEvaluation`）。
///
/// 检查顺序见模块头的表：**第 6 条必须先于第 7 条**，否则会跨租户泄漏 issue id。
async fn record_squad_leader_evaluation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    // 1 — 身份 + 租户：workspace 解析 → 成员门槛（非成员 404，本仓 M2 约定）→ issue 载入。
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let issue = load_issue(&issue_repo(&state), workspace_id, &raw_id).await?;

    // 2 — body（上游 `json.Decoder` 失败 ⇒ 400 `invalid request body`）。
    let req: SquadEvaluationRequest = parse_body(&body)?;

    // 3 — outcome 白名单（在碰任何 task 之前；上游也是这个位置）。
    if !is_valid_outcome(req.outcome()) {
        return Err(validation(OUTCOME_ERROR).into());
    }

    // 4 — `X-Task-ID` 是硬要求（头，不是 body 字段）：缺失 / 非 uuid 一律 400。
    let raw_task_id = header_value(&headers, TASK_ID_HEADER).unwrap_or_default();
    let task_id = parse_target_id("task id", raw_task_id)?;

    // 5 — 租户收窄的 task 查询（上游 `GetAgentTaskInWorkspace`，`JOIN agent` 才是闸门）。
    let repo = SquadEvaluationRepo::new(state.db.clone());
    let Some(task) = repo
        .leader_task_in_workspace(task_id, workspace_id)
        .await
        .map_err(lookup_err)?
    else {
        return Err(validation(TASK_NOT_BELONG).into());
    };
    let Some(task_issue_id) = task.issue_id() else {
        // chat / quick-create 任务没有 issue ⇒ 上游 `!task.IssueID.Valid` 同一分支。
        return Err(validation(TASK_NOT_BELONG).into());
    };

    // 6 — 闸门 1：调用者必须就是这条任务入队给的 agent。
    //
    // 这一段**必须**停在第 7 条之前：它以下的拒绝才允许回显 task 派生的 id。
    let Some(actor_id) = resolve_agent_actor(&state, &headers, workspace_id, &task).await? else {
        return Err(forbidden(ONLY_LEADER_ERROR).into());
    };

    // 7 — 过了闸门 1，指名 task 自己的 issue 才是安全的，而且有用：被 stage barrier /
    //     「子任务完成」回唤醒的 leader 跑在**父** issue 上，顺手记在刚读过的**子** issue 上
    //     是这里最常见的错。所以文案里带上本该记的那个 issue id。
    if task_issue_id != issue.id() {
        return Err(validation(format!(
            "{TASK_NOT_BELONG}; record the evaluation on issue {task_issue_id} \
             (the issue this task is running on)"
        ))
        .into());
    }

    // 8 — 收紧：跑在 squad 指派的 issue 上的**非 leader** 任务不再被接受（上游 `MUL-6622`
    //     的显式收紧）。运行时只在 `taskIsSquadLeader(task)` 时才强制这次调用。
    if !task.is_leader_task() {
        return Err(validation(NOT_LEADER_TASK_ERROR).into());
    }

    // 9 — `MUL-3730` 之前的行可以是「leader 任务但没有盖 squad」：记 warn 而不是静默失败。
    let Some(squad_id) = task.squad_id() else {
        tracing::warn!(
            task_id = %task.id(),
            issue_id = %issue.id(),
            "squad leader evaluation: leader task has no squad_id"
        );
        return Err(validation(NO_SQUAD_ID_ERROR).into());
    };

    // 10 — squad 必须在本 workspace（上游 `GetSquadInWorkspace`，**不过滤 archived_at**）。
    let squad = SquadRepo::new(state.db.clone())
        .find_in_workspace(workspace_id, squad_id)
        .await
        .map_err(lookup_err)?;
    let Some(squad) = squad else {
        return Err(not_found("squad").into());
    };

    // 11 — 闸门 2：调用者**现在**仍是该 squad 的 leader。
    //
    //     `is_leader_task` 记的是**入队时**的意图，不等于 claim 实际交付的角色：leader 在
    //     入队与 claim 之间被换掉时，claim 路径会清掉 `resp.IsLeaderTask`、这一轮按普通 agent
    //     回合跑，而行上的 `is_leader_task` 仍留 `true`。只信行就会让这种「降级运行」写下
    //     leader 判决（并在 `no_action` 时抑制掉自己的评论）。已交付的角色尚未落库之前，
    //     这个活体判定是唯一能把两者分开的东西。
    //
    //     保守选择的代价：中途被轮换掉的 leader 会在这里被拒。那不再等于「什么都没有」——
    //     注入的规则现在要求记录调用失败的 leader 改用一条短评论交代。
    if actor_id != squad.leader_id() {
        return Err(forbidden(ONLY_LEADER_ERROR).into());
    }

    // 12 — 落库：一条 `activity_log` 行（**不是**专用表）。
    let details = evaluation_details(squad.id(), task_id, req.outcome(), req.reason());
    let row = repo
        .record_evaluation(workspace_id, issue.id(), actor_id, &details)
        .await
        .map_err(|err| record_failed(&err))?;

    // 13 — 201：三个字符串字段（上游 `writeJSON(w, http.StatusCreated, map[string]string{…})`）。
    Ok((
        StatusCode::CREATED,
        Json(SquadEvaluationResponse {
            id: row.id().to_string(),
            action: row.action.clone(),
            created_at: row.created_at.to_rfc3339(),
        }),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// actor 解析（上游 `resolveActor` 的本地可达分支）
// ---------------------------------------------------------------------------

/// 上游 `resolveActor` 的**自校验分支**：`X-Agent-ID` + `X-Task-ID` 都存在、agent 在目标
/// workspace 里真实存在、且这条 task 的 `agent_id` 就是它 ⇒ `Some(agent_id)`；否则 `None`
/// （= member，调用方据此 403）。
///
/// 顺序逐字照上游 `handler.go:847`：
///
/// 1. `X-Agent-ID` 缺失（或空串）⇒ member；
/// 2. `X-Agent-ID` 非 UUID ⇒ member；
/// 3. `GetAgent(id)` 查不到、或 `agent.workspace_id != workspace_id` ⇒ member
///    （上游用的是**不带** workspace 过滤的 `GetAgent`，本函数同款：查得到但跨 workspace 仍回 member）；
/// 4. `X-Task-ID` 缺失 / 非 UUID / 这条 task 的 `agent_id` 不等于 `X-Agent-ID` ⇒ member。
///
/// ⚠️ 上游 `resolveActor` 的那条 `X-Actor-Source: task_token` 快路径**有意不实现**（偏离 D2）：
/// 它凭的是中间件已经剥掉客户端头并盖章，本地没有那层中间件 ⇒ 照抄等于任何人自封 agent。
///
/// `X-Task-ID` 的第 4 步复用第 5 步已经取回的 `task` 行（`task.agent_id`）—— 上游
/// `resolveActor` 另查一次 `GetAgentTask` 只是为了拿到同一列，语义等价。
///
/// # Errors
///
/// 只可能来自「agent 行查询」的 DB 故障（此时按 fail-closed 返回错误，
/// 而不是静默降级成 member ⇒ 降级成 member 会让本该 403 的请求在 DB 抖动时变成 500，
/// 反之则会**绕过**闸门）。
async fn resolve_agent_actor(
    state: &AppState,
    headers: &HeaderMap,
    workspace_id: Id,
    task: &SquadLeaderTaskRow,
) -> Result<Option<Id>, Error> {
    let Some(raw_agent_id) = header_value(headers, AGENT_ID_HEADER) else {
        return Ok(None);
    };
    let Ok(agent_id) = Uuid::parse_str(raw_agent_id) else {
        tracing::debug!(agent_id = %raw_agent_id, "resolve actor: X-Agent-ID is not a uuid");
        return Ok(None);
    };

    // tenancy：agent 必须属于目标 workspace（上游 `GetAgent` + workspace 比对）。
    let found: Option<(Uuid,)> = sqlx::query_as("SELECT workspace_id FROM agent WHERE id = $1")
        .bind(agent_id)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|e| Error::Database(e.to_string()))?;
    match found {
        Some((agent_workspace,)) if agent_workspace == workspace_id.0 => {}
        _ => {
            tracing::debug!(
                agent_id = %agent_id,
                "resolve actor: X-Agent-ID rejected (agent missing or workspace mismatch)"
            );
            return Ok(None);
        }
    }

    // 这条任务必须就是入队给它的（上游 `task.AgentID != agentID` ⇒ member）。
    let agent_id = Id::from(agent_id);
    if agent_id != task.agent_id() {
        tracing::debug!(
            agent_id = %agent_id,
            "resolve actor: X-Task-ID rejected (task belongs to another agent)"
        );
        return Ok(None);
    }
    Ok(Some(agent_id))
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// 读一个请求头：缺失 / 非 UTF-8 都当缺失，**不 trim**（与 Go 的 `Header.Get` 同义 ——
/// 带空白的 id 在上游会解析失败，这里也必须失败，不能悄悄宽容）。
fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
}

/// body 解码（上游 `json.Decoder` 失败 ⇒ 400 `invalid request body`）。
fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Error> {
    serde_json::from_slice::<T>(body).map_err(|_| validation("invalid request body"))
}

/// 403（闸门 1 / 闸门 2 共用同一句文案）。
fn forbidden(message: &'static str) -> Error {
    Error::Forbidden {
        message: message.into(),
    }
}

/// 两处**只读查找**的仓储错误：不存在一律由 `Ok(None)` 表达（`fetch_optional`），
/// 所以 `NotFound` / `Conflict` 到不了这里；仍逐条映射，不留一个「什么都吞成 500」
/// 的默认分支（将来谁把这些查询改成 `fetch_one` 会立刻看见 `not_found` 而不是 500）。
fn lookup_err(err: RepoError) -> Error {
    match err {
        RepoError::NotFound => not_found("squad"),
        RepoError::Conflict => Error::Conflict {
            message: "row was modified concurrently; refetch and retry".into(),
        },
        RepoError::Db(message) => Error::Database(message),
    }
}

/// 落库失败的 500。
///
/// **有意不映射成 `Error::Database(message)`**：上游这里写的是不透明文案
/// `failed to record evaluation`，而 `Error::Database` 会把 SQL 细节（表名 / 约束 / 参数片段）
/// 放进响应体。本仓其它端点用 `Error::Database` 是个取舍，这里按上游收紧。
fn record_failed(err: &RepoError) -> Error {
    tracing::error!(error = %err, "squad leader evaluation: activity insert failed");
    Error::Internal(RECORD_FAILED_ERROR.into())
}

// ---------------------------------------------------------------------------
// 纯单测（无需 DB）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn router_builds_without_panicking() {
        // 同 path+method 重复注册会在 `Router::route` 处 panic ⇒ 这条用例是形态门。
        let _ = router();
    }

    /// `X-Task-ID` 缺失 ⇒ 空串 ⇒ `invalid task id`（**不是**默认放过）。
    #[test]
    fn missing_or_empty_headers_are_absent() {
        let mut headers = HeaderMap::new();
        assert_eq!(header_value(&headers, TASK_ID_HEADER), None);
        headers.insert(TASK_ID_HEADER, HeaderValue::from_static(""));
        assert_eq!(header_value(&headers, TASK_ID_HEADER), None, "空串当缺失");
        headers.insert(TASK_ID_HEADER, HeaderValue::from_static("abc"));
        assert_eq!(
            header_value(&headers, TASK_ID_HEADER),
            Some("abc"),
            "不取 trim：宽容会把上游解析失败的输入变成合法输入"
        );
        headers.insert(TASK_ID_HEADER, HeaderValue::from_static(" abc "));
        assert_eq!(header_value(&headers, TASK_ID_HEADER), Some(" abc "));
        assert!(Uuid::parse_str(" abc ").is_err(), "带空白在上游必失败");
    }

    /// 缺字段 / 显式 `null` 之后走的是同一条白名单拒绝（Go 零值语义）。
    #[test]
    fn request_defaults_to_empty_outcome() {
        let req: SquadEvaluationRequest = serde_json::from_str("{}").expect("空对象可解码");
        assert_eq!(req.outcome(), "");
        assert!(!is_valid_outcome(req.outcome()), "空 outcome 被白名单拒绝");

        // 显式 `null` 与缺字段在 Go 里是同一件事（都留零值）⇒ 本地也必须同归空串，
        // 否则文案会从 `outcome must be …` 分叉成 `invalid request body`。
        let null: SquadEvaluationRequest =
            serde_json::from_str(r#"{"outcome":null,"reason":null}"#).expect("null 也可解码");
        assert_eq!(null.outcome(), "");
        assert_eq!(null.reason(), "");
        let empty: SquadEvaluationRequest =
            serde_json::from_str(r#"{"outcome":"","reason":""}"#).expect("空串可解码");
        assert_eq!(empty.outcome(), "");
        // 三态（缺 / null / ""）落到同一条拒绝上。
        for body in ["{}", r#"{"outcome":null}"#, r#"{"outcome":""}"#] {
            let parsed: SquadEvaluationRequest = serde_json::from_str(body).expect("可解码");
            assert!(!is_valid_outcome(parsed.outcome()), "{body}");
        }
    }

    /// 未知字段不报错（上游 `json.Decoder` 非 strict）。
    #[test]
    fn unknown_fields_are_ignored() {
        let req: SquadEvaluationRequest =
            serde_json::from_str(r#"{"outcome":"failed","reason":"x","extra":1}"#).expect("解码");
        assert_eq!(req.outcome(), "failed");
        assert_eq!(req.reason(), "x");
    }

    #[test]
    fn parse_body_rejects_malformed_json() {
        let err = parse_body::<SquadEvaluationRequest>(&Bytes::from_static(b"{oops"))
            .expect_err("坏 body 必须 400");
        assert_eq!(err.message(), "validation error: invalid request body");
    }

    /// 响应体的三个键与上游 `map[string]string` 同形（都是字符串）。
    #[test]
    fn response_shape_is_three_strings() {
        let value = serde_json::to_value(SquadEvaluationResponse {
            id: Uuid::nil().to_string(),
            action: mc_repos::squad_evaluation::ACTION_SQUAD_LEADER_EVALUATED.to_string(),
            created_at: "2026-09-25T00:00:00+00:00".to_string(),
        })
        .expect("序列化");
        let object = value.as_object().expect("对象");
        assert_eq!(object.len(), 3);
        for key in ["id", "action", "created_at"] {
            assert!(
                object.get(key).is_some_and(serde_json::Value::is_string),
                "{key} 必须是字符串"
            );
        }
        assert_eq!(
            object["action"],
            serde_json::json!("squad_leader_evaluated")
        );
    }

    /// 403 文案两句一字不差（上游故意不区分是哪道闸门拦下的）。
    #[test]
    fn error_texts_match_upstream() {
        assert_eq!(
            ONLY_LEADER_ERROR,
            "only the squad leader agent can record evaluations"
        );
        assert_eq!(TASK_NOT_BELONG, "task does not belong to issue");
        assert_eq!(NOT_LEADER_TASK_ERROR, "task is not a squad leader task");
        assert_eq!(NO_SQUAD_ID_ERROR, "leader task has no squad_id");
        assert_eq!(RECORD_FAILED_ERROR, "failed to record evaluation");
    }

    /// 500 不泄漏 DB 文案（有意不用 `Error::Database`）。
    #[test]
    fn record_failure_hides_database_text() {
        let err = record_failed(&RepoError::Db(
            "relation \"activity_log\" does not exist".into(),
        ));
        assert_eq!(err.http_status(), 500);
        assert_eq!(err.message(), "internal error: failed to record evaluation");
        assert!(!err.message().contains("activity_log"));
        assert_eq!(err.code(), "internal_error");
    }

    /// 闸门 1/2 的 403 是 `forbidden`，不是 404/400。
    #[test]
    fn gate_rejections_are_forbidden() {
        assert_eq!(forbidden(ONLY_LEADER_ERROR).http_status(), 403);
        assert_eq!(forbidden(ONLY_LEADER_ERROR).code(), "forbidden");
    }
}
