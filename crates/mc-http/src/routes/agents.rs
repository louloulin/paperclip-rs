//! M3-5（LUM-1428）：agent 面 —— `/api/agents*` 13 条 + workspace 级统计 3 条，
//! 共 **16 条路由**（docs/15 §1.3 / `contracts/upstream-routes`）。
//!
//! 上游对照：`server/internal/handler/agent.go`、`agent_env.go`、`label.go`、
//! `agent_permission.go`、`agent_access.go`、`agent_validation.go`（`f41fae6b`）。
//! 本切片在 M3-0 的**空 router 锚点**上填入真实实现（`mount.rs::mount_slice_agent()`
//! 已接好，本片不需要动 `mount.rs` / `routes/mod.rs`）。
//!
//! 路由表（与上游 `router.go` 逐条对齐；axum 0.7 / matchit 0.7 的路径参数必须写
//! `:id`，写成 `{id}` 会编译通过但恒 404 —— M1-D 踩过的坑）：
//!
//! | method | path | handler |
//! |---|---|---|
//! | GET | `/api/agents/` | [`crud::list_agents`] |
//! | POST | `/api/agents/` | [`crud::create_agent`] |
//! | GET | `/api/agents/:id/` | [`crud::get_agent`] |
//! | PUT | `/api/agents/:id/` | [`crud::update_agent`] |
//! | POST | `/api/agents/:id/archive` | [`crud::archive_agent`] |
//! | POST | `/api/agents/:id/restore` | [`crud::restore_agent`] |
//! | POST | `/api/agents/:id/cancel-tasks` | [`crud::cancel_agent_tasks`] |
//! | GET | `/api/agents/:id/tasks` | [`crud::list_agent_tasks`] |
//! | GET | `/api/agents/:id/labels` | [`labels::list_labels`] |
//! | POST | `/api/agents/:id/labels` | [`labels::attach_label`] |
//! | DELETE | `/api/agents/:id/labels/:labelId` | [`labels::detach_label`] |
//! | GET | `/api/agents/:id/env` | [`env::get_agent_env`] |
//! | PUT | `/api/agents/:id/env` | [`env::update_agent_env`] |
//! | GET | `/api/agent-task-snapshot` | [`stats::task_snapshot`] |
//! | GET | `/api/agent-activity-30d` | [`stats::activity_30d`] |
//! | GET | `/api/agent-run-counts` | [`stats::run_counts`] |
//!
//! **本片（M6-4）新增**：`/api/agents/{id}/skills*` 与 `/api/agents/{id}/runtime-skills/enabled`
//! （6 条注册键，见 sibling `agents/skills.rs`；M6-0 anchor 在 `routes/mod.rs:71-72` 明写这 6 条
//! 由 M6-4 在本文件内加 `mod skills;` + 逐条 `.route()`）。
//!
//! **不在本片**：`/api/agents/mika*`（M9），以及 `runtime_availability` 的在线投影（M3-4）、
//! `task` 状态机（M3-3/M3-6）。有意偏离的完整清单见 `docs/40-M3-5-AGENTS.md` §5。
//!
//! 鉴权约定（与本仓 M1/M2 各切片一致）：
//! - `X-Multica-User-Id` → [`AuthUser`]
//! - workspace 上下文 `X-Workspace-ID` / `?workspace_id` → [`resolve_workspace_id`]
//! - 非成员 → 404 `workspace`（上游 `requireWorkspaceRole` 的 `workspace not found` 分支）
//! - agent actor（上游 `resolveActor` 的 `X-Agent-ID` / `X-Actor-Source` 分支）**本片不解析**：
//!   本仓尚无「服务端可信地重写这些 header」的中间件，直接信任客户端 header 会让任意成员
//!   伪造成 agent 身份（`canAccessPrivateAgent` 对 agent actor 恒真）——属于**放宽**。
//!   因此本片一律按 member 处理（fail-closed）。M3-7 引入 task token 时补上可信来源判定。

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::HeaderMap;
use axum::routing::{delete, get, post, put};
use axum::Router;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::agent::{
    role_is_admin, AgentInvocationTargetRow, AgentRepo, AgentRow, RuntimeBindingRow,
};
use mc_repos::RepoError;
use uuid::Uuid;

use crate::routes::auth_user::AuthUser;
use crate::routes::inbox::resolve_workspace_id;
use crate::state::AppState;

mod crud;
mod dto;
mod env;
mod labels;
mod skills;
mod stats;

/// agent 面 16 条上游路由（+2 条尾斜杠别名 = 18 个注册键）。
///
/// **尾斜杠别名**：上游是 chi 的 `Route("/api/agents") + Get("/")`，两种写法都能命中；
/// 而 axum 0.7 / matchit 0.7 里树中只有 `/api/agents/` 时 `at("/api/agents")` 返回
/// `Err(MissingTrailingSlash)`（`axum-0.7.9/src/routing/path_router.rs:381` 并入 `Err`）
/// ⇒ **404 而不是 307**。三条 golden fixture（`contracts/golden/agents/00{1,2,3}-*`）恰好
/// 请求不带斜杠的 `/api/agents`，只注册带斜杠形态会让它们全落 `unmounted`（⑦ 折叠尾斜杠，
/// 看不见这个故障）。做法与 M3-4（`routes/runtimes.rs` L80-90）一致：两种都注册。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/agents/",
            get(crud::list_agents).post(crud::create_agent),
        )
        .route(
            "/api/agents",
            get(crud::list_agents).post(crud::create_agent),
        )
        .route(
            "/api/agents/:id/",
            get(crud::get_agent).put(crud::update_agent),
        )
        .route(
            "/api/agents/:id",
            get(crud::get_agent).put(crud::update_agent),
        )
        .route("/api/agents/:id/archive", post(crud::archive_agent))
        .route("/api/agents/:id/restore", post(crud::restore_agent))
        .route(
            "/api/agents/:id/cancel-tasks",
            post(crud::cancel_agent_tasks),
        )
        .route("/api/agents/:id/tasks", get(crud::list_agent_tasks))
        .route(
            "/api/agents/:id/labels",
            get(labels::list_labels).post(labels::attach_label),
        )
        .route(
            "/api/agents/:id/labels/:label_id",
            delete(labels::detach_label),
        )
        .route(
            "/api/agents/:id/env",
            get(env::get_agent_env).put(env::update_agent_env),
        )
        // M6-4：agent 的 skill 绑定面（5 条）+ runtime-local skill 开关（1 条）。
        // 静态段 `add` 与参数段 `:skill_id` 不冲突：matchit 0.7 静态优先。
        .route(
            "/api/agents/:id/skills",
            get(skills::list_agent_skills).put(skills::set_agent_skills),
        )
        .route("/api/agents/:id/skills/add", post(skills::add_agent_skills))
        .route(
            "/api/agents/:id/skills/:skill_id/enabled",
            put(skills::set_agent_skill_enabled),
        )
        .route(
            "/api/agents/:id/skills/:skill_id",
            delete(skills::remove_agent_skill),
        )
        .route(
            "/api/agents/:id/runtime-skills/enabled",
            put(skills::set_agent_runtime_skill_enabled),
        )
        .route("/api/agent-task-snapshot", get(stats::task_snapshot))
        .route("/api/agent-activity-30d", get(stats::activity_30d))
        .route("/api/agent-run-counts", get(stats::run_counts))
}

// ---------------------------------------------------------------------------
// 错误 / 解析 helper（各切片各自持有本地副本，与本仓既有约定一致）
// ---------------------------------------------------------------------------

pub(crate) fn bad_request(message: impl Into<String>) -> Error {
    Error::Validation {
        message: message.into(),
        details: vec![],
    }
}

pub(crate) fn not_found(resource: &'static str) -> Error {
    Error::NotFound {
        resource: resource.into(),
    }
}

pub(crate) fn forbidden(message: &str) -> Error {
    Error::Forbidden {
        message: message.to_string(),
    }
}

pub(crate) fn repo_err(e: RepoError, resource: &'static str) -> Error {
    match e {
        RepoError::NotFound => not_found(resource),
        RepoError::Conflict => Error::Conflict {
            message: format!("{resource} state conflict"),
        },
        RepoError::Db(msg) => Error::Database(msg),
    }
}

/// 上游 `q.Get(name) != ""` 的等价语义：空值等于没传。
#[cfg(test)]
pub(crate) fn query_value<'a>(query: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    query
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
}

/// 上游 `parseUUIDOrBadRequest(w, raw, field)`。
pub(crate) fn parse_uuid(raw: &str, field: &str) -> Result<Uuid, Error> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("{field} must be a valid uuid")))
}

/// 读取调用者在 workspace 里的角色；非成员 → 404 `workspace`
/// （上游 `requireWorkspaceRole(..., "workspace not found", ...)`）。
pub(crate) async fn workspace_role(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> Result<String, Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.0)
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
    row.map(|(role,)| role)
        .ok_or_else(|| not_found("workspace"))
}

// ---------------------------------------------------------------------------
// 调用上下文
// ---------------------------------------------------------------------------

/// 一次请求的「workspace + 调用者 + 角色 + repo」四元组，外加全部鉴权判定。
///
/// 上游把同样的判定散在 `loadAgentForUser` / `canManageAgent` /
/// `canAccessPrivateAgent` / `canViewAgentSecrets` / `canManageAgentEnv` 里；
/// 本片把它们收进一个结构体，让每个 handler 只写自己的业务分支。
pub(crate) struct AgentScope {
    pub(crate) workspace_id: Id,
    pub(crate) user_id: Id,
    pub(crate) role: String,
    pub(crate) repo: AgentRepo,
}

impl AgentScope {
    /// 解析 workspace（400）→ 成员身份（404）→ repo。
    pub(crate) async fn resolve(
        state: &AppState,
        user: AuthUser,
        headers: &HeaderMap,
        query: &HashMap<String, String>,
    ) -> Result<Self, Error> {
        let workspace_id = resolve_workspace_id(headers, query)?;
        let user_id = user.id();
        let role = workspace_role(state, workspace_id, user_id).await?;
        Ok(Self {
            workspace_id,
            user_id,
            role,
            repo: AgentRepo::new(state.db.clone()),
        })
    }

    /// workspace owner/admin（上游 `roleAllowed(role, "owner", "admin")`）。
    pub(crate) fn is_admin(&self) -> bool {
        role_is_admin(&self.role)
    }

    /// 调用者是否为 agent 的 owner（上游 `uuidToString(agent.OwnerID) == userID`）。
    ///
    /// `owner_id` 可空：NULL 与「调用者 id 为空」都不会命中（上游注释里那条
    /// 「NULL owner 不能让所有人可读」的守卫）。
    pub(crate) fn is_agent_owner(&self, agent: &AgentRow) -> bool {
        agent.owner_id == Some(self.user_id.0)
    }

    /// 上游 `canViewAgentSecrets(agent, userID, memberRole)`：admin 或 agent owner。
    pub(crate) fn can_view_secrets(&self, agent: &AgentRow) -> bool {
        self.is_admin() || self.is_agent_owner(agent)
    }

    /// 上游 `canManageAgent`：admin 或 agent owner。
    pub(crate) fn can_manage(&self, agent: &AgentRow) -> bool {
        self.is_admin() || self.is_agent_owner(agent)
    }

    /// 上游 `canManageAgentEnv`：admin 或 agent owner。
    pub(crate) fn can_manage_env(&self, agent: &AgentRow) -> bool {
        self.is_admin() || self.is_agent_owner(agent)
    }

    /// 上游 `memberHitsInvocationTargets(targets, userID)` 的 member 分支：
    /// workspace 目标对成员恒真；member 目标按 user id 命中；team 目标**恒不命中**
    /// （V1 没有 team 成员表，fail-closed）。
    pub(crate) fn member_hits_targets(&self, targets: &[AgentInvocationTargetRow]) -> bool {
        targets.iter().any(|t| match t.target_type.as_str() {
            "workspace" => true,
            "member" => t.target_id == self.user_id.0,
            _ => false,
        })
    }

    /// 上游 `memberAllowedToViewAgent`：admin / agent owner / `public_to` 且命中白名单。
    pub(crate) fn member_allowed_to_view(
        &self,
        agent: &AgentRow,
        targets: &[AgentInvocationTargetRow],
    ) -> bool {
        if self.can_manage(agent) {
            return true;
        }
        agent.permission_mode == mc_repos::agent::PERMISSION_MODE_PUBLIC_TO
            && self.member_hits_targets(targets)
    }

    /// 上游 `canAccessPrivateAgent`（member actor 分支）：看得到才能读/取消。
    pub(crate) fn can_access_private(
        &self,
        agent: &AgentRow,
        targets: &[AgentInvocationTargetRow],
    ) -> bool {
        self.member_allowed_to_view(agent, targets)
    }

    /// 上游 `canManageAgent` 的失败分支（403）。
    pub(crate) fn require_can_manage(&self, agent: &AgentRow) -> Result<(), Error> {
        if self.can_manage(agent) {
            Ok(())
        } else {
            Err(forbidden("only the agent owner can manage this agent"))
        }
    }

    /// 上游私有 agent 的读路径 403。
    pub(crate) fn require_can_access_private(
        &self,
        agent: &AgentRow,
        targets: &[AgentInvocationTargetRow],
    ) -> Result<(), Error> {
        if self.can_access_private(agent, targets) {
            Ok(())
        } else {
            Err(forbidden("you do not have access to this agent"))
        }
    }

    /// 上游 `authorizeAgentEnv` 的 403。
    pub(crate) fn require_can_manage_env(&self, agent: &AgentRow) -> Result<(), Error> {
        if self.can_manage_env(agent) {
            Ok(())
        } else {
            Err(forbidden(
                "only the agent owner or a workspace owner/admin can manage this agent's env",
            ))
        }
    }

    /// 上游 `loadAgentForUser`：解析 id → 本 workspace 且 `kind='user'` → 否则 404。
    pub(crate) async fn load_agent(&self, raw: &str) -> Result<AgentRow, Error> {
        let id = Id::parse(raw.trim()).map_err(|_| not_found("agent"))?;
        self.repo
            .get_in_workspace(self.workspace_id, id)
            .await
            .map_err(|e| repo_err(e, "agent"))
    }

    /// 上游 `canInvokeAgent`（`agent_access.go:49`）的 **member actor 分支**。
    ///
    /// ⚠️ **invoke 门 ≠ view 门**（M3-6 逐行核实）：
    /// - `private`：**只有 agent owner** —— 上游在这里**没有** admin 越权，也没有 A2A 通道；
    /// - `public_to`：白名单命中就放行（workspace 目标 ⇒ 任何成员；member 目标 ⇒ 该用户；
    ///   team 目标 V1 恒不命中）；
    /// - 其他 `permission_mode`：拒绝。
    ///
    /// 虽然 [`AgentScope::can_access_private`]（view 门）对 admin 放行，但 invoke 门
    /// **不**复用那个判定：同一个 admin 读得到 agent，却不能只因此就把它跑起来。
    pub(crate) fn can_invoke(
        &self,
        agent: &AgentRow,
        targets: &[AgentInvocationTargetRow],
    ) -> bool {
        if self.is_agent_owner(agent) {
            return true;
        }
        agent.permission_mode == mc_repos::agent::PERMISSION_MODE_PUBLIC_TO
            && self.member_hits_targets(targets)
    }

    /// 上游 `loadAgentForUser` 的非报错变体：不存在 / 非本 workspace / `kind<>'user'` 都回 `None`。
    pub(crate) async fn agent_opt(&self, id: Id) -> Result<Option<AgentRow>, Error> {
        self.repo
            .find_in_workspace(self.workspace_id, id)
            .await
            .map_err(|e| repo_err(e, "agent"))
    }

    /// agent 的 invoke 白名单。
    pub(crate) async fn targets_of(
        &self,
        agent_id: Id,
    ) -> Result<Vec<AgentInvocationTargetRow>, Error> {
        self.repo
            .list_invocation_targets(agent_id)
            .await
            .map_err(|e| repo_err(e, "agent"))
    }

    /// 按 agent 归组的白名单批量读（list / 统计端点用它避免 N+1）。
    pub(crate) async fn targets_by_agent(
        &self,
        agent_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<AgentInvocationTargetRow>>, Error> {
        let rows = self
            .repo
            .list_invocation_targets_for_agents(agent_ids)
            .await
            .map_err(|e| repo_err(e, "agent"))?;
        let mut grouped: HashMap<Uuid, Vec<AgentInvocationTargetRow>> = HashMap::new();
        for row in rows {
            grouped.entry(row.agent_id).or_default().push(row);
        }
        Ok(grouped)
    }

    /// `runtime_id` 必须指向本 workspace 的 runtime（上游 `GetAgentRuntimeForWorkspace`
    /// 失败 → 400 `invalid runtime_id`）。
    pub(crate) async fn runtime_binding(
        &self,
        runtime_id: Uuid,
    ) -> Result<RuntimeBindingRow, Error> {
        self.repo
            .runtime_binding(self.workspace_id, runtime_id)
            .await
            .map_err(|_| bad_request("invalid runtime_id"))
    }

    /// 上游 `canUseRuntimeForAgent`：runtime 必须有 owner；`public` 所有人可用，
    /// `private` 只有其 owner 可用（admin **不能**越权借用别人的机器）。
    pub(crate) fn can_use_runtime(&self, runtime: &RuntimeBindingRow) -> bool {
        runtime
            .owner_id
            .is_some_and(|owner| owner == self.user_id.0 || runtime.visibility == "public")
    }

    /// 上游 `accessibleAgentIDs` 的等价过滤（list / 三个统计端点共用）。
    pub(crate) fn filter_accessible(
        &self,
        agents: Vec<AgentRow>,
        targets: &HashMap<Uuid, Vec<AgentInvocationTargetRow>>,
    ) -> Vec<AgentRow> {
        let empty: Vec<AgentInvocationTargetRow> = Vec::new();
        if self.is_admin() {
            return agents;
        }
        agents
            .into_iter()
            .filter(|a| {
                let t = targets.get(&a.id).unwrap_or(&empty);
                self.member_allowed_to_view(a, t)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_builds_without_panicking() {
        // axum 0.7 同 path+method 重复注册会在 build 期 panic；本断言保证 16 条上游路由
        // + 2 条尾斜杠别名互不冲突（含 `:id` / `:label_id` 两个参数位）。
        let _ = router();
    }

    #[test]
    fn role_helpers_match_upstream_role_allowed() {
        assert!(role_is_admin("owner"));
        assert!(role_is_admin("admin"));
        assert!(!role_is_admin("member"));
        assert!(!role_is_admin("guest"));
    }

    #[test]
    fn blank_query_values_read_as_absent() {
        let mut query: HashMap<String, String> = HashMap::new();
        query.insert("include_archived".to_string(), String::new());
        assert_eq!(query_value(&query, "include_archived"), None);
        query.insert("include_archived".to_string(), "true".to_string());
        assert_eq!(query_value(&query, "include_archived"), Some("true"));
        assert_eq!(query_value(&query, "missing"), None);
    }
}
