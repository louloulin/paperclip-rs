//! squad 面的鉴权上下文（上游 `squad.go` 的 `canManageSquad` /
//! `memberCanWireAgent` / `loadSquadInWorkspace` + `requireWorkspaceMember`）。
//!
//! 三个判定各自对应上游一个函数，**逐字**对齐，不做收敛：
//!
//! | 本文件 | 上游 | 规则 |
//! |---|---|---|
//! | [`SquadScope::can_manage`] | `canManageSquad`（L120） | admin/owner 全管；普通成员只管**自己创建**的 squad |
//! | [`SquadScope::member_can_wire_agent`] | `memberCanWireAgent`（L136 → `canInvokeAgent` L49） | admin 任意；否则按 **invoke 门**（owner 或 `public_to` + 白名单命中） |
//! | [`SquadScope::squad`] | `loadSquadInWorkspace`（L145） | id 非法 → 400；不在本 workspace → 404 `squad not found` |
//!
//! ⚠️ `memberCanWireAgent` 用的是 **invoke** 门而不是 view 门（M3-5 已核实两者不同：
//! `private` agent 在这里连 admin 都不能越权，admin 的放行来自上游函数开头的
//! `roleAllowed` 短路，而不是 view 门里的 admin 分支）。squad 这层**不**复用
//! `AgentScope::can_invoke`，因为它绑在 `mc_repos::agent::AgentRow` 上（带
//! `kind='user'` 过滤），而上游 `GetAgentInWorkspace` 不过滤 `kind`
//! ⇒ 自己持有 [`SquadWireAgentRow`] 与白名单查询。

use std::collections::HashMap;

use axum::http::HeaderMap;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::agent::{
    role_is_admin, AgentRepo, PERMISSION_MODE_PUBLIC_TO, TARGET_MEMBER, TARGET_WORKSPACE,
};
use mc_repos::squad::{SquadRepo, SquadRow, SquadWireAgentRow};

use crate::routes::agents::{forbidden, not_found, parse_uuid, repo_err, workspace_role};
use crate::routes::auth_user::AuthUser;
use crate::routes::inbox::resolve_workspace_id;
use crate::state::AppState;

/// 一次 squad 请求的「workspace + 调用者 + 角色 + 两个 repo」五元组。
pub(crate) struct SquadScope {
    pub(crate) workspace_id: Id,
    pub(crate) user_id: Id,
    pub(crate) role: String,
    pub(crate) repo: SquadRepo,
    /// invoke 白名单查询（`agent_invocation_target`，M3 域**只读**）。
    pub(crate) agents: AgentRepo,
}

impl SquadScope {
    /// workspace（400 invalid workspace id）→ 成员身份（非成员 404 `workspace`）→ repo。
    ///
    /// 上游把成员校验分成两类：`requireWorkspaceMember`（create/update/delete/members）
    /// 与「路由组中间件」（list/get/members 列表）。两者都是「非成员不可见」，因此本仓
    /// 统一在 handler 起点解析，形态与 M3-5 `AgentScope::resolve` 一致。
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
            repo: SquadRepo::new(state.db.clone()),
            agents: AgentRepo::new(state.db.clone()),
        })
    }

    /// 上游 `roleAllowed(member.Role, "owner", "admin")`。
    pub(crate) fn is_admin(&self) -> bool {
        role_is_admin(&self.role)
    }

    /// 上游 `canManageSquad`：admin/owner 管所有；普通成员只管自己创建的（MUL-4223）。
    pub(crate) fn can_manage(&self, squad: &SquadRow) -> bool {
        self.is_admin() || squad.creator_id == self.user_id.0
    }

    /// 上游 `canManageSquad` 的失败分支（403 `insufficient permissions`）。
    pub(crate) fn require_can_manage(&self, squad: &SquadRow) -> Result<(), Error> {
        if self.can_manage(squad) {
            Ok(())
        } else {
            Err(forbidden("insufficient permissions"))
        }
    }

    /// 上游 `loadSquadInWorkspace`：id 非法 → 400 `squad id must be a valid uuid`；
    /// 不在本 workspace（含已归档行查不到以外的情形）→ 404 `squad not found`。
    pub(crate) async fn squad(&self, raw: &str) -> Result<SquadRow, Error> {
        let id = Id(parse_uuid(raw, "squad id")?);
        self.repo
            .find_in_workspace(self.workspace_id, id)
            .await
            .map_err(|e| repo_err(e, "squad"))?
            .ok_or_else(|| not_found("squad"))
    }

    /// 上游 `GetAgentInWorkspace`：本 workspace 的 agent（**不**过滤 `kind` / 归档）。
    /// `None` = 不在本 workspace（各调用点自己给 400 文案）。
    pub(crate) async fn agent_in_workspace(
        &self,
        raw: &str,
    ) -> Result<Option<SquadWireAgentRow>, Error> {
        let id = Id(parse_uuid(raw, "leader_id")?);
        self.repo
            .agent_in_workspace(self.workspace_id, id)
            .await
            .map_err(|e| repo_err(e, "agent"))
    }

    /// 上游 `memberCanWireAgent` → `canInvokeAgent(..., "member", uid, uid, ...)`。
    ///
    /// admin/owner 恒真；普通成员走 invoke 门的 member 分支：
    /// ① agent owner；② `public_to` 且白名单命中（`workspace` 目标 ⇒ 成员恒命中，
    /// `member` 目标 ⇒ 本人，`team` ⇒ V1 恒不命中）。`private` 且非 owner ⇒ 拒绝。
    pub(crate) async fn member_can_wire_agent(
        &self,
        agent: &SquadWireAgentRow,
    ) -> Result<bool, Error> {
        if self.is_admin() {
            return Ok(true);
        }
        if agent.owner_id == Some(self.user_id.0) {
            return Ok(true);
        }
        if agent.permission_mode != PERMISSION_MODE_PUBLIC_TO {
            return Ok(false);
        }
        let targets = self
            .agents
            .list_invocation_targets(agent.id())
            .await
            .map_err(|e| repo_err(e, "agent"))?;
        Ok(targets.iter().any(|t| match t.target_type.as_str() {
            TARGET_WORKSPACE => true,
            TARGET_MEMBER => t.target_id == self.user_id.0,
            _ => false,
        }))
    }
}
