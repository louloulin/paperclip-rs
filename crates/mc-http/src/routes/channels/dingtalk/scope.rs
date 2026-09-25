//! `DingTalk` 渠道面的**鉴权与装配**（写者 **M7-9**）。
//!
//! 拆出来是**门 ⑩**（单文件 800 行硬限）的要求。本文件承担三件事：
//!
//! 1. [`DingTalkScope`]：workspace 级 **成员解析**（比 `routes/agents.rs` 的 `workspace_role`
//!    多一条**自定义 404 文案** —— 上游各 handler 传给 `requireWorkspaceMember` 的文案逐条不同）；
//! 2. [`agent_visibility`]：上游 `dingtalkAgentVisibility`（`available` 用来把"孤儿安装"与
//!    "你看不见"分开；`visible` 是普通成员的过滤集，**复用** [`AgentScope`] 的判定，
//!    不新增第二份可见性真值）；
//! 3. [`collect_groups`]：群清单的取数与装配（workspace 级与 agent 级只差 `agent_id` / 可见性集）。
//!
//! 授权矩阵（workspace 成员 × 私有 agent 的 owner）见 `dingtalk.rs` 的模块文档表格。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use mc_channel::dingtalk::group_identity::{
    GroupInventory, GroupInventoryStore, GroupQuery, GroupQueryPlan, PresenceQuery,
};
use mc_core::id::Id;
use mc_errors::Error;
use mc_repos::agent::{role_is_admin, AgentRepo, AgentRow};

use crate::routes::agents::{bad_request, forbidden, not_found, parse_uuid, AgentScope};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 角色查询：非成员 ⇒ `None`（上游 `requireWorkspaceMember` 的 404 分支需要一条**自定义**
/// 文案，所以不能直接用 `routes/agents.rs` 的 `workspace_role` —— 它固定回 `workspace`）。
pub(crate) async fn role_in_workspace(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> Result<Option<String>, Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace_id.0)
    .bind(user_id.0)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|error| Error::Database(error.to_string()))
}

// =====================================================================
// 装配（七个 handler 共用）
// =====================================================================

/// 一次请求的 `(workspace, 调用者, 角色, agent 判定)`。
pub(crate) struct DingTalkScope {
    pub(crate) workspace_id: Id,
    pub(crate) user_id: Id,
    role: String,
    agents: AgentScope,
    pub(crate) repo: AgentRepo,
}

impl DingTalkScope {
    /// workspace 从**路径**取（上游戏把 18 条 workspace 级路由注册在 `/{id}` 子路由内部）。
    ///
    /// `not_found_resource` 是上游各 handler 传给 `requireWorkspaceMember` 的文案
    /// （逐条不同：`workspace` / `dingtalk group not found` / …）。
    pub(crate) async fn resolve(
        state: &AppState,
        user: AuthUser,
        raw_workspace_id: &str,
        not_found_resource: &'static str,
    ) -> Result<Self, Error> {
        let workspace_id = Id(parse_uuid(raw_workspace_id, "workspace id")?);
        let user_id = user.id();
        let role = role_in_workspace(state, workspace_id, user_id)
            .await?
            .ok_or_else(|| not_found(not_found_resource))?;
        Ok(Self::with_role(state, workspace_id, user_id, role))
    }

    /// 同一件事，但成员身份由调用方先取（撤销那条的判定顺序与上游一致：先查安装行）。
    pub(crate) fn with_role(state: &AppState, workspace_id: Id, user_id: Id, role: String) -> Self {
        let repo = AgentRepo::new(state.db.clone());
        Self {
            agents: AgentScope {
                workspace_id,
                user_id,
                role: role.clone(),
                repo: repo.clone(),
            },
            repo,
            workspace_id,
            user_id,
            role,
        }
    }

    pub(crate) fn is_admin(&self) -> bool {
        role_is_admin(&self.role)
    }

    /// workspace owner/admin（上游 `requireWorkspaceRole(…, "owner", "admin")`）。
    pub(crate) fn require_admin(&self) -> Result<(), Error> {
        if self.is_admin() {
            Ok(())
        } else {
            Err(forbidden("insufficient permissions"))
        }
    }

    /// 上游 `canManageAgent`：admin 或 agent owner（否则 403 上游逐字文案）。
    pub(crate) fn require_agent_manager(&self, agent: &AgentRow) -> Result<(), Error> {
        if self.agents.can_manage(agent) {
            Ok(())
        } else {
            Err(forbidden("only the agent owner can manage this agent"))
        }
    }
}

/// `(agent 全部 id, 调用者可见的 agent id)`（上游 `dingtalkAgentVisibility`）。
///
/// `available` 用来把"孤儿安装"（agent 行已删）与"agent 你看不见"分开；`visible` 是普通成员的
/// 过滤集（owner/admin 直接拿全量）。**复用** [`AgentScope::member_allowed_to_view`]。
pub(crate) async fn agent_visibility(
    scope: &DingTalkScope,
) -> Result<(HashSet<Id>, HashSet<Id>), Error> {
    let agents = scope
        .repo
        .list(scope.workspace_id, true)
        .await
        .map_err(|error| Error::Database(error.to_string()))?;
    let mut available = HashSet::with_capacity(agents.len());
    for agent in &agents {
        available.insert(Id(agent.id));
    }
    let mut visible = HashSet::new();
    if scope.is_admin() {
        visible = available.clone();
        return Ok((available, visible));
    }
    let ids: Vec<uuid::Uuid> = agents.iter().map(|agent| agent.id).collect();
    let targets = scope
        .repo
        .list_invocation_targets_for_agents(&ids)
        .await
        .map_err(|error| Error::Database(error.to_string()))?;
    let by_agent = group_targets(targets);
    for agent in &agents {
        let empty = Vec::new();
        let own = by_agent.get(&agent.id).unwrap_or(&empty);
        if scope.agents.member_allowed_to_view(agent, own) {
            visible.insert(Id(agent.id));
        }
    }
    Ok((available, visible))
}

/// `agent_id → 目标行`（`AgentScope` 的判定吃这个形状）。
pub(crate) fn group_targets(
    rows: Vec<mc_repos::agent::AgentInvocationTargetRow>,
) -> HashMap<uuid::Uuid, Vec<mc_repos::agent::AgentInvocationTargetRow>> {
    let mut grouped: HashMap<uuid::Uuid, Vec<mc_repos::agent::AgentInvocationTargetRow>> =
        HashMap::new();
    for row in rows {
        grouped.entry(row.agent_id).or_default().push(row);
    }
    grouped
}

/// 群清单的取数与装配（两条路由共用：workspace 级与 agent 级只差 `agent_id` / 可见性集）。
pub(crate) async fn collect_groups(
    state: &AppState,
    store: &Arc<dyn GroupInventoryStore>,
    workspace_id: Id,
    agent_id: Option<Id>,
    visible_agent_ids: Option<&HashSet<Id>>,
    query: &GroupQuery,
) -> Result<GroupInventory, Error> {
    let _ = state;
    let plan = GroupQueryPlan::parse(agent_id.is_some(), query)
        .map_err(|error| bad_request(error.to_string()))?;
    let active_since = mc_channel::dingtalk::group_identity::active_since(Utc::now());

    // 上游 `mayListInactiveDingTalkInstallation`：请求的安装必须是本 workspace 的、活跃的、
    // 且调用者看得到的 —— 四种不通过**同**结果（否则群数据与 `next_offset` 会泄露存在性）。
    let may_list = if plan.include_inactive {
        match plan.installation_id {
            Some(installation_id) => store
                .may_list_inactive_installation(workspace_id, installation_id, agent_id)
                .await
                .map_err(Error::Database)?,
            None => false,
        }
    } else {
        true
    };

    let fetch_limit = if plan.include_inactive {
        plan.page_limit + 1
    } else {
        0
    };
    let presence_query = PresenceQuery {
        workspace_id,
        agent_id,
        installation_id: plan.installation_id,
        include_inactive: plan.include_inactive,
        active_since,
        page_offset: plan.page_offset,
        page_limit: fetch_limit,
    };
    let rows = if may_list {
        store
            .list_presences(&presence_query)
            .await
            .map_err(Error::Database)?
    } else {
        Vec::new()
    };
    let counts = store
        .count_inactive(workspace_id, agent_id, active_since)
        .await
        .map_err(Error::Database)?;
    let identities = store
        .list_bot_identities(workspace_id, agent_id)
        .await
        .map_err(Error::Database)?;
    Ok(mc_channel::dingtalk::group_identity::assemble_inventory(
        &plan,
        agent_id,
        rows,
        counts,
        identities,
        visible_agent_ids,
    ))
}
