//! M3-6（LUM-1429）：**15 条路由** —— agent-builder 4 条 + task / lifecycle /
//! usage / retry 11 条（docs/36 §2 / docs/15 §1.4、§1.6）。
//!
//! 上游对照（`f41fae6b`）：`server/internal/handler/agent_builder.go`、
//! `client_usage.go`、`daemon.go`、`issue_trigger.go`、`task_lifecycle.go`、
//! `chat.go`、`agent.go`、`middleware/client.go`。
//!
//! 本片是 W3b 里**唯一改既有文件**的切片：其中 6 条此前是 `issues/mod.rs` 的 501 占位，
//! 本片把它们**就地替换**（同 path + method 重复注册会让 axum 在 `Router::route` 处 panic，
//! 因此不能另起一处再注册一遍）。
//!
//! | method | path | handler | 备注 |
//! |---|---|---|---|
//! | GET | `/api/agent-builder/sessions/` | [`builder::list_sessions`] | 上游 `Route("/")+Get` |
//! | POST | `/api/agent-builder/sessions/` | [`builder::create_session`] | 201 |
//! | PATCH | `/api/agent-builder/sessions/:session_id/runtime` | [`builder::switch_runtime`] | |
//! | PUT | `/api/agent-builder/sessions/:session_id/draft` | [`builder::save_draft`] | |
//! | POST | `/api/client-usage` | [`usage::upsert_client_usage`] | 204 |
//! | POST | `/api/issues/preview-trigger` | [`lifecycle::preview_trigger`] | 原 501 |
//! | GET | `/api/issues/:id/active-task` | [`lifecycle::get_active_task`] | 原 501 |
//! | POST | `/api/issues/:id/rerun` | [`rerun::rerun_issue`] | 原 501 |
//! | GET | `/api/issues/:id/task-runs` | [`lifecycle::task_runs`] | 原 501 |
//! | GET | `/api/issues/:id/usage` | [`usage::issue_usage`] | 原 501 |
//! | POST | `/api/issues/:id/tasks/:task_id/cancel` | [`cancel::cancel_issue_task`] | 原 501 |
//! | GET | `/api/tasks/:task_id/messages` | [`usage::list_task_messages`] | |
//! | POST | `/api/tasks/:task_id/retry-source-context` | [`rerun::retry_source_context`] | |
//! | POST | `/api/tasks/:task_id/cancel` | [`cancel::cancel_task`] | |
//! | GET | `/api/working-agents` | [`working::working_agents`] | |
//!
//! 另有 2 个尾斜杠别名（`/api/agent-builder/sessions`），共 **17 个注册键**。
//!
//! 鉴权约定（与本仓 M1/M2/M3-5 一致，逐条沿用 [`AgentScope`]）：
//! - `X-Multica-User-Id` → [`AuthUser`]
//! - workspace `X-Workspace-ID` / `?workspace_id` → [`resolve_workspace_id`]
//! - 非成员 → 404 `workspace`（上游 `requireWorkspaceRole`）
//! - 私有 agent 的可见性 / invoke 白名单复用 [`AgentScope::filter_accessible`] /
//!   [`AgentScope::can_invoke`]，**不新增判定**
//!
//! 上游两条 `RequireHumanActor` 中间件（`/api/client-usage`、
//! `/api/tasks/{taskId}/retry-source-context`）在本仓天然满足：`AuthUser` 只接受成员
//! 身份（agent actor 的 header 本仓不解析，见 `docs/40` §5 的同一处放宽）。M3-7 引入
//! task token 时补上可信来源判定。
//!
//! 有意偏离的完整清单见 `docs/41-M3-6-TASK-QUEUE.md`。
//!
//! 注意（M1-D 实测踩过的坑）：axum 0.7（matchit 0.7）路径参数必须写 `:id`，
//! `{id}` 会被当字面量段——编译通过但恒 404。
//!
//! 文件布局（R7：单文件 800 行硬上限，`scripts/file_size_check.py` + 门 ⑩）：
//! - `mod.rs`（本文件）：模块文档 + `pub fn router()` + [`TaskScope`] + 共享 helper
//! - `dto.rs`：请求 / 响应 DTO
//! - `lifecycle.rs`：preview-trigger / active-task / task-runs
//! - `cancel.rs`：两条取消路由
//! - `rerun.rs`：rerun-issue / retry-source-context
//! - `usage.rs`：client-usage / issue-usage / task messages
//! - `builder.rs`：agent-builder 四条
//! - `working.rs`：working-agents

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::HeaderMap;
use axum::routing::{get, patch, post, put};
use axum::Router;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::agent::AgentRow;
use mc_repos::task::{IssueBrief, TaskRepo, TaskRow};
use mc_repos::RepoError;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::routes::agents::AgentScope;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

mod builder;
mod cancel;
pub(crate) mod dto;
mod lifecycle;
mod rerun;
mod usage;
mod working;

/// M3-6 的 15 条上游路由（+2 条尾斜杠别名 = 17 个注册键）。
///
/// **尾斜杠别名**：上游是 chi 的 `r.Route("/api/agent-builder/sessions", ...)` +
/// `r.Get("/")` / `r.Post("/")`，两种写法都能命中；axum 0.7 / matchit 0.7 只注册其一
/// 时另一种返回 **404 而非 307**（M3-4 / M3-5 同款处理）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // ---- agent-builder（4 条）------------------------------------------
        .route(
            "/api/agent-builder/sessions/",
            get(builder::list_sessions).post(builder::create_session),
        )
        .route(
            "/api/agent-builder/sessions",
            get(builder::list_sessions).post(builder::create_session),
        )
        .route(
            "/api/agent-builder/sessions/:session_id/runtime",
            patch(builder::switch_runtime),
        )
        .route(
            "/api/agent-builder/sessions/:session_id/draft",
            put(builder::save_draft),
        )
        // ---- task / lifecycle / usage / retry（11 条）-----------------------
        .route("/api/client-usage", post(usage::upsert_client_usage))
        .route(
            "/api/issues/preview-trigger",
            post(lifecycle::preview_trigger),
        )
        .route(
            "/api/issues/:id/active-task",
            get(lifecycle::get_active_task),
        )
        .route("/api/issues/:id/rerun", post(rerun::rerun_issue))
        .route("/api/issues/:id/task-runs", get(lifecycle::task_runs))
        .route("/api/issues/:id/usage", get(usage::issue_usage))
        .route(
            "/api/issues/:id/tasks/:task_id/cancel",
            post(cancel::cancel_issue_task),
        )
        .route(
            "/api/tasks/:task_id/messages",
            get(usage::list_task_messages),
        )
        .route(
            "/api/tasks/:task_id/retry-source-context",
            post(rerun::retry_source_context),
        )
        .route("/api/tasks/:task_id/cancel", post(cancel::cancel_task))
        .route("/api/working-agents", get(working::working_agents))
}

/// 上游 `parseUUIDOrBadRequest`（`handler.go`）：400 `invalid <field>`。
pub(crate) fn parse_uuid(raw: &str, field: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("invalid {field}")))
}

// ---------------------------------------------------------------------------
// 错误 helper
// ---------------------------------------------------------------------------
//
// 本模块的 handler 一律以 [`ApiResult`] 为返回类型，因此下面四个包装函数回
// [`ApiError`]（而不是像 `agents.rs` 那样回 `mc_errors::Error`）：直接 `Err(...)`
// 需要精确类型，靠 `?` 的 `From` 转换救不了。语义与 `agents.rs` 的同名函数一一对应，
// 不新增判定。

/// 400 `validation_error`。
pub(crate) fn bad_request(message: impl Into<String>) -> ApiError {
    crate::routes::agents::bad_request(message).into()
}

/// 404 `not_found`（`message` = 资源名）。
pub(crate) fn not_found(resource: &'static str) -> ApiError {
    crate::routes::agents::not_found(resource).into()
}

/// 403 `forbidden`。
pub(crate) fn forbidden(message: &str) -> ApiError {
    crate::routes::agents::forbidden(message).into()
}

/// 仓储错误 → HTTP。
pub(crate) fn repo_err(e: RepoError, resource: &'static str) -> ApiError {
    crate::routes::agents::repo_err(e, resource).into()
}

/// `mc_repos::task::TaskError` → HTTP 错误。
///
/// 映射（`TaskError::Conflict` 的两处上游来源分别是
/// `ErrTaskNoLongerQueued`（409）与 CAS 冲突）：
/// - `NotFound` → 404 `resource`
/// - `Conflict` → 409（message 用领域层给的 `detail`）
/// - `Backend` → 500
/// - 其余（`IllegalTransition` / `TerminalState` / `Unknown*` / `MalformedEvent`）→ 400
pub(crate) fn task_error(e: mc_repos::task::TaskError, resource: &'static str) -> ApiError {
    match e {
        mc_repos::task::TaskError::NotFound { .. } => not_found(resource),
        mc_repos::task::TaskError::Conflict { detail } => Error::Conflict {
            message: detail.to_owned(),
        }
        .into(),
        mc_repos::task::TaskError::Backend { message } => Error::Database(message).into(),
        other => bad_request(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// 调用上下文
// ---------------------------------------------------------------------------

/// 一次请求的「agent 侧鉴权 + task 仓储」组合。
///
/// 鉴权 / workspace / 角色判定全部委托 [`AgentScope`]（M3-5 已合入的同一份实现），
/// 本结构只补 task 面需要的两个解析器与 invoke 门。
pub(crate) struct TaskScope {
    /// 调用者 + workspace + 角色 + agent repo。
    pub(crate) agent: AgentScope,
    /// task 仓储。
    pub(crate) repo: TaskRepo,
}

impl TaskScope {
    /// 解析 header / query，并校验调用者是 workspace 成员。
    pub(crate) async fn resolve(
        state: &AppState,
        user: AuthUser,
        headers: &HeaderMap,
        query: &HashMap<String, String>,
    ) -> ApiResult<Self> {
        let agent = AgentScope::resolve(state, user, headers, query).await?;
        Ok(Self {
            agent,
            repo: TaskRepo::new(&state.db),
        })
    }

    pub(crate) fn workspace_id(&self) -> Id {
        self.agent.workspace_id
    }

    pub(crate) fn user_id(&self) -> Id {
        self.agent.user_id
    }

    /// 加载 issue（必须属于本 workspace）→ 404 `issue`。
    ///
    /// 上游 `loadIssueForUser`：不存在 / 别的 workspace / 非成员一律 `issue not found`；
    /// `:id` 同时接受 UUID 与 identifier（`LUM-42`）。
    pub(crate) async fn issue(&self, raw: &str) -> ApiResult<IssueBrief> {
        self.repo
            .issue_for_workspace_ref(raw, self.workspace_id())
            .await
            .map_err(|e| repo_err(e, "issue"))?
            .ok_or_else(|| not_found("issue"))
    }

    /// 加载 task（必须属于本 workspace）→ 404 `task`。
    pub(crate) async fn task_in_workspace(&self, raw: &str) -> ApiResult<TaskRow> {
        let id = parse_uuid(raw, "task_id")?;
        self.repo
            .task_in_workspace(Id::from(id), self.workspace_id())
            .await
            .map_err(|e| repo_err(e, "task"))?
            .ok_or_else(|| not_found("task"))
    }

    /// invoke 门：agent 必须在本 workspace、`kind='user'`、且调用者命中白名单。
    ///
    /// 返回 `Ok(None)` 表示**拒绝**（不存在与无权**同形**，不区分）。调用方回上游
    /// `dispatchBlockedResponse`（403 + `reason_code: invocation_not_allowed`），
    /// 不套本仓错误信封 —— 前端按 `reason_code` 分支，见 `docs/41` §5。
    pub(crate) async fn invoke_gate_opt(&self, agent_id: Id) -> ApiResult<Option<AgentRow>> {
        let Some(agent) = self.agent.agent_opt(agent_id).await? else {
            return Ok(None);
        };
        let targets = self.agent.targets_of(agent_id).await?;
        Ok(self.agent.can_invoke(&agent, &targets).then_some(agent))
    }

    /// 本 workspace 内调用者**可见**的 agent（`id → row`）。
    ///
    /// 上游 `accessibleAgentIDs`：workspace 级聚合端点（working-agents / 统计）
    /// 一律先过访问过滤，私有 agent 不得凭名字 / 头像 / 计数暴露存在性。
    pub(crate) async fn accessible_agents(&self) -> ApiResult<HashMap<Uuid, AgentRow>> {
        let all = self
            .agent
            .repo
            .list(self.workspace_id(), false)
            .await
            .map_err(|e| repo_err(e, "agent"))?;
        let ids: Vec<Uuid> = all.iter().map(|a| a.id).collect();
        let targets = self.agent.targets_by_agent(&ids).await?;
        Ok(self
            .agent
            .filter_accessible(all, &targets)
            .into_iter()
            .map(|a| (a.id, a))
            .collect())
    }
}

/// 取成员的展示名（人工取消时写 `cancelled_by_name`）。
///
/// 上游走 `user.name`；查不到（用户被删）就返回 `None`，与 `delivered_comments_plan`
/// 的 departed-safe 处理一致 —— 名字缺失不该让取消失败。
pub(crate) async fn user_display_name(state: &AppState, user_id: Id) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT name FROM \"user\" WHERE id = $1")
        .bind(user_id.0)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
}

/// 空字符串 → `None`（上游 `strings.TrimSpace(q) == ""` 等价语义）。
pub(crate) fn non_empty_query(query: &HashMap<String, String>, name: &str) -> Option<String> {
    query
        .get(name)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uuid_rejects_garbage_with_upstream_message() {
        let err = parse_uuid("not-a-uuid", "task_id").expect_err("must reject");
        assert_eq!(err.0.code(), "validation_error");
        assert!(err.0.to_string().contains("invalid task_id"));
    }

    #[test]
    fn parse_uuid_trims_whitespace() {
        let raw = "  6f1c3f18-8d3d-4b34-9b3b-4e3a2f6a1f11  ";
        assert!(parse_uuid(raw, "issue_id").is_ok());
    }

    #[test]
    fn task_error_maps_conflict_to_409() {
        let err = task_error(
            mc_repos::task::TaskError::Conflict {
                detail: "task is no longer queued",
            },
            "task",
        );
        assert_eq!(err.0.http_status(), 409);
    }

    #[test]
    fn task_error_maps_backend_to_500() {
        let err = task_error(
            mc_repos::task::TaskError::Backend {
                message: "boom".to_owned(),
            },
            "task",
        );
        assert_eq!(err.0.http_status(), 500);
    }

    #[test]
    fn non_empty_query_drops_blank_values() {
        let mut query = HashMap::new();
        query.insert("scope".to_owned(), "  ".to_owned());
        assert!(non_empty_query(&query, "scope").is_none());
        query.insert("scope".to_owned(), " family ".to_owned());
        assert_eq!(non_empty_query(&query, "scope").as_deref(), Some("family"));
    }
}
