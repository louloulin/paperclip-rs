//! workspace 级 wakeup 面（`GET /api/issue-wakeups` / `GET /api/issue-wakeup-summaries`）
//! —— 外加 issue 面（`issues/wakeups.rs`）共用的鉴权 / 可见性 / 错误映射。
//!
//! - **写者**：M5-6（`docs/44` §3.2）。
//! - **上游**：`handler/issue_wakeup.go` 的 `ListWorkspaceWakeups`(81) 与
//!   `ListWorkspaceWakeupSummaries`(27)，注册在 `router.go:2319-2320`（workspace 级两条）。
//! - **M5-0 anchor** 只注册了 `GET /api/issue-wakeups` 的 501 占位；本片补上
//!   `GET /api/issue-wakeup-summaries`（`docs/44` §1.1 第 29 行判给 M5-6）——它是本波**唯一**
//!   真 `known_gap` 的 wakeup 键 ⇒ 门 ⑦ 的 `local` **+1**（新增键永不触发 parity 回归）。
//!
//! # 本仓的「actor」只有一种形态（偏离 D-actor）
//!
//! 上游 `resolveActor` 有三条分支（`X-Actor-Source: task_token` / `X-Agent-ID`+`X-Task-ID` /
//! 其余 = member）。本仓 `/api/issues*` 面没有 agent actor（`routes/agents.rs` 文件头已登记同一偏离），
//! `AuthUser` 恒是人类成员 ⇒ 本片固定 `actor_type = "member"`、`actor_id = user_id`、
//! `source_task_id = None`；`invokeOriginatorFromRequest` 对 member 返回自身 ⇒ 「发起人」就是调用者。
//! 于是上游的两条 agent 分支（`in.AgentID = actorID`、`wakeupSourceTaskID`）本地不可达。
//!
//! # 非成员 = 403（与上游 404 的小偏离）
//!
//! 上游 `workspaceMember` 对非成员写 404（不泄漏 workspace 是否存在）。本 issue 的 `DoD` 明写
//! 「非成员 403 / 跨 workspace 404」⇒ 本片按 403 `wakeup permission denied` 交付，
//! 并在交付注释登记；「wakeup 不在本 workspace」仍是 404（`GetIssueWakeup(id, workspace_id)`）。
//!
//! # 错误信封
//!
//! 上游写**扁平** `{"error": msg, "code": code}`；本仓 `ApiError` 是嵌套
//! `{"error": {"code", "message"}}`。本片沿用本仓标准信封（与 M2/M3 各片一致），
//! 只保证**状态码 + code** 与上游对齐：`wakeup_capacity_exceeded`(400) /
//! `wakeup_source_busy`(409) 用 [`WakeupHttpError::Coded`]，其余走 `ApiError` 的映射。

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::agent::{role_is_admin, AgentRepo};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::agents::AgentScope;
use super::auth_user::AuthUser;
use super::issues::{resolve_workspace, WorkspaceQuery};
use crate::error::ApiError;
use crate::state::AppState;
use mc_autopilot::wakeup::WakeupError;
use mc_repos::wakeup::listing::{
    list_workspace_wakeup_summaries, list_workspace_wakeups, WorkspaceWakeupQuery,
};
use mc_repos::wakeup::WakeupSummaryRow;
use serde_json::Value as JsonValue;

/// workspace 级 wakeup 列表的查询串（上游 `ListWorkspaceWakeups` 的 `r.URL.Query()`）。
///
/// `limit` / `offset` 收成字符串再手工解析：上游是 `strconv.Atoi`，**Atoi 失败也是 400
/// `invalid pagination`**（而不是 axum `Query` 缺省拒绝的另一种错误体）。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct WorkspaceWakeupParams {
    /// workspace 选择器（与 `/api/issues*` 同一套解析规则）。
    pub workspace_id: Option<String>,
    /// workspace slug（header 缺失时的兜底）。
    pub workspace_slug: Option<String>,
    /// `active | all | disabled | ended`（空 ⇒ `active`）。
    pub scope: Option<String>,
    /// `all | event | at | recurring`（空 ⇒ `all`）。
    pub kind: Option<String>,
    /// 页大小（1..=100，默认 50）。
    pub limit: Option<String>,
    /// 偏移（0..=1000000，默认 0）。
    pub offset: Option<String>,
    /// 搜索串（trim 后 ≤256 字节）。
    pub search: Option<String>,
    /// agent 过滤（非法 uuid ⇒ 400 `invalid agent id`）。
    pub agent_id: Option<String>,
}

impl WorkspaceWakeupParams {
    /// 交给 [`resolve_workspace`] 的两个选择器。
    fn workspace_query(&self) -> WorkspaceQuery {
        WorkspaceQuery {
            workspace_id: self.workspace_id.clone(),
            workspace_slug: self.workspace_slug.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// wakeup 面的 HTTP 错误：本仓标准信封 + 上游两个固定 code 的特例。
pub(crate) enum WakeupHttpError {
    /// 本仓标准 `ApiError` 信封。
    Standard(ApiError),
    /// 上游 `writeErrorCode` 的形态（状态码 + 固定 code + 固定文案）。
    Coded {
        /// HTTP 状态码。
        status: u16,
        /// 机器可读 code。
        code: &'static str,
        /// 人类可读文案（容量场景是库原文）。
        message: String,
    },
}

impl From<ApiError> for WakeupHttpError {
    fn from(value: ApiError) -> Self {
        Self::Standard(value)
    }
}

impl From<Error> for WakeupHttpError {
    fn from(value: Error) -> Self {
        Self::Standard(ApiError(value))
    }
}

impl IntoResponse for WakeupHttpError {
    fn into_response(self) -> Response {
        match self {
            Self::Standard(api) => api.into_response(),
            Self::Coded {
                status,
                code,
                message,
            } => {
                let body = serde_json::json!({ "error": { "code": code, "message": message } });
                let status =
                    StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                (status, Json(body)).into_response()
            }
        }
    }
}

/// wakeup 面 handler 的返回类型。
pub(crate) type WakeupResult<T> = Result<T, WakeupHttpError>;

/// 403 `wakeup permission denied`（上游 `wakeupError` 的 forbidden 分支）。
pub(crate) fn wakeup_forbidden() -> WakeupHttpError {
    Error::Forbidden {
        message: "wakeup permission denied".into(),
    }
    .into()
}

/// 上游 `wakeupError`：把领域错误折算成 HTTP 状态码 + code。
pub(crate) fn wakeup_error(err: WakeupError) -> WakeupHttpError {
    match err {
        // `issue_wakeup_active_limit` 触发器（`530` 的 `RAISE EXCEPTION` 原文）。
        WakeupError::Capacity(message) => WakeupHttpError::Coded {
            status: 400,
            code: "wakeup_capacity_exceeded",
            message,
        },
        // `55P03`：`LockWakeupSourceTask` 的 `FOR UPDATE NOWAIT` 没抢到锁。
        WakeupError::SourceBusy => WakeupHttpError::Coded {
            status: 409,
            code: "wakeup_source_busy",
            message: "source run is changing; retry registration".into(),
        },
        WakeupError::Conflict => Error::Conflict {
            message: "wakeup changed; refresh and retry".into(),
        }
        .into(),
        WakeupError::Input(message) => Error::Validation {
            message: format!("invalid wakeup: {message}"),
            details: Vec::new(),
        }
        .into(),
        WakeupError::Forbidden => wakeup_forbidden(),
        WakeupError::NotFound => Error::NotFound {
            resource: "wakeup".into(),
        }
        .into(),
        // `NotDispatchable` 与 `Db` 都落 500（上游 `default: could not save wakeup`）。
        other => Error::Database(other.to_string()).into(),
    }
}

/// 仓储层错误（本片没有领域语义，只有库故障）折算成 500。
pub(crate) fn repo_error(err: mc_repos::RepoError) -> WakeupHttpError {
    match err {
        mc_repos::RepoError::NotFound => Error::NotFound {
            resource: "wakeup".into(),
        }
        .into(),
        mc_repos::RepoError::Conflict => Error::Conflict {
            message: "wakeup changed; refresh and retry".into(),
        }
        .into(),
        mc_repos::RepoError::Db(message) => Error::Database(message).into(),
    }
}

// ---------------------------------------------------------------------------
// 鉴权 / 可见性（issue 面共用）
// ---------------------------------------------------------------------------

/// 成员角色（`member` 表的 `role`）；非成员 ⇒ `None`（由调用方决定 403 还是 404）。
pub(crate) async fn member_role(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> WakeupResult<Option<String>> {
    mc_repos::wakeup::lookup::member_role(state.db.pool(), workspace_id.0, user_id.0)
        .await
        .map_err(repo_error)
}

/// 成员角色，非成员 ⇒ 403 `wakeup permission denied`（见文件头「非成员 = 403」）。
pub(crate) async fn require_member_role(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> WakeupResult<String> {
    member_role(state, workspace_id, user_id)
        .await?
        .ok_or_else(wakeup_forbidden)
}

/// 上游 `accessibleAgentIDs`：调用者可见的 agent 集合（list 的掩码用）。
///
/// 上游对 `actorType != "member"` 直接放行全部；本仓只有 member（见文件头）⇒ 恒走
/// `memberAllowedToViewAgent` 过滤。过滤本身复用 `routes/agents.rs` 的
/// [`AgentScope::filter_accessible`]（上游同一个函数的既有实现），**不在这里重写一遍**。
///
/// `ListAllAgents`（`kind='user'`，**不过滤** `archived_at`）对应 `AgentRepo::list(_, true)`：
/// 归档与否由 `memberAllowedToViewAgent` 之外的掩码语义决定，这里不额外收窄。
pub(crate) async fn accessible_agent_ids(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
    role: &str,
) -> WakeupResult<Vec<Uuid>> {
    let scope = AgentScope {
        workspace_id,
        user_id,
        role: role.to_string(),
        repo: AgentRepo::new(state.db.clone()),
    };
    let agents = scope.repo.list(workspace_id, true).await.map_err(repo_error)?;
    let agent_ids = agents.iter().map(|a| a.id).collect::<Vec<_>>();
    let targets = scope
        .repo
        .list_invocation_targets_for_agents(&agent_ids)
        .await
        .map_err(repo_error)?;
    let mut by_agent: HashMap<Uuid, Vec<mc_repos::agent::AgentInvocationTargetRow>> = HashMap::new();
    for target in targets {
        by_agent.entry(target.agent_id).or_default().push(target);
    }
    Ok(scope
        .filter_accessible(agents, &by_agent)
        .into_iter()
        .map(|a| a.id)
        .collect())
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `GET /api/issue-wakeups`：workspace 级 wakeup 列表（分页 / scope / kind / search / agent）。
async fn list_workspace_wakeups_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<WorkspaceWakeupParams>,
    user: AuthUser,
) -> WakeupResult<Json<JsonValue>> {
    let workspace_id = resolve_workspace(&state, &headers, &params.workspace_query()).await?;
    let role = require_member_role(&state, workspace_id, user.id()).await?;

    let scope = normalise_scope(params.scope.as_deref())?;
    let kind = normalise_kind(params.kind.as_deref())?;
    let (page_limit, page_offset) = parse_pagination(params.limit.as_deref(), params.offset.as_deref())?;
    let search = params.search.as_deref().unwrap_or("").trim().to_string();
    if search.len() > 256 {
        return Err(Error::Validation {
            message: "search too long".into(),
            details: Vec::new(),
        }
        .into());
    }
    let agent_id = params.agent_id.as_deref().unwrap_or("");
    let agent_id = if agent_id.is_empty() {
        String::new()
    } else {
        Id::parse(agent_id.trim())
            .map_err(|_| Error::Validation {
                message: "invalid agent id".into(),
                details: Vec::new(),
            })?
            .0
            .to_string()
    };

    let agent_ids = accessible_agent_ids(&state, workspace_id, user.id(), &role).await?;
    let query = WorkspaceWakeupQuery {
        workspace_id: workspace_id.0,
        agent_ids,
        // 「发起人」= 调用者（见文件头 D-actor）。
        member_id: Some(user.id().0),
        is_admin: role_is_admin(&role),
        scope,
        kind,
        agent_id,
        search,
        page_limit,
        page_offset,
    };
    let result = list_workspace_wakeups(state.db.pool(), &query)
        .await
        .map_err(repo_error)?;
    Ok(Json(result))
}

/// `GET /api/issue-wakeup-summaries`：每 issue 最多 3 条预览 + 全量计数（#29，本片新增注册）。
async fn list_workspace_wakeup_summaries_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<WorkspaceQuery>,
    user: AuthUser,
) -> WakeupResult<Json<Vec<WakeupSummaryRow>>> {
    let workspace_id = resolve_workspace(&state, &headers, &params).await?;
    let role = require_member_role(&state, workspace_id, user.id()).await?;
    let agent_ids = accessible_agent_ids(&state, workspace_id, user.id(), &role).await?;
    let rows = list_workspace_wakeup_summaries(state.db.pool(), workspace_id.0, &agent_ids)
        .await
        .map_err(repo_error)?;
    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// 参数归一化
// ---------------------------------------------------------------------------

/// `scope` 默认 `active`；不在四值内 ⇒ 400 `invalid wakeup scope`。
fn normalise_scope(raw: Option<&str>) -> WakeupResult<String> {
    let scope = match raw {
        None | Some("") => "active",
        Some(value) => value,
    };
    if matches!(scope, "active" | "all" | "disabled" | "ended") {
        Ok(scope.to_string())
    } else {
        Err(Error::Validation {
            message: "invalid wakeup scope".into(),
            details: Vec::new(),
        }
        .into())
    }
}

/// `kind` 默认 `all`；不在四值内 ⇒ 400 `invalid wakeup kind`。
fn normalise_kind(raw: Option<&str>) -> WakeupResult<String> {
    let kind = match raw {
        None | Some("") => "all",
        Some(value) => value,
    };
    if matches!(kind, "all" | "event" | "at" | "recurring") {
        Ok(kind.to_string())
    } else {
        Err(Error::Validation {
            message: "invalid wakeup kind".into(),
            details: Vec::new(),
        }
        .into())
    }
}

/// 上游的 `limit`/`offset` 解析：非数字 / 越界一律 400 `invalid pagination`。
///
/// 返回 `(limit, offset)`，与 `WorkspaceWakeupQuery` 的字段顺序一致。
fn parse_pagination(limit_raw: Option<&str>, offset_raw: Option<&str>) -> WakeupResult<(i32, i32)> {
    let bad = || -> WakeupHttpError {
        Error::Validation {
            message: "invalid pagination".into(),
            details: Vec::new(),
        }
        .into()
    };
    let mut limit = 50_i32;
    if let Some(raw) = limit_raw.filter(|raw| !raw.is_empty()) {
        let value = raw.parse::<i32>().map_err(|_| bad())?;
        if !(1..=100).contains(&value) {
            return Err(bad());
        }
        limit = value;
    }
    let mut offset = 0_i32;
    if let Some(raw) = offset_raw.filter(|raw| !raw.is_empty()) {
        let value = raw.parse::<i32>().map_err(|_| bad())?;
        if !(0..=1_000_000).contains(&value) {
            return Err(bad());
        }
        offset = value;
    }
    Ok((limit, offset))
}

/// workspace 级 wakeup 子切片：M5-6 把占位换成真实 handler（#28 + #29）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/issue-wakeups", get(list_workspace_wakeups_handler))
        .route(
            "/api/issue-wakeup-summaries",
            get(list_workspace_wakeup_summaries_handler),
        )
}
