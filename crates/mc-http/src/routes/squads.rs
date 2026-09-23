//! squad 面 `/api/squads*`（上游 `server/internal/handler/squad.go`，1243 行；10 条路由
//! `router.go` L2081–L2093）。
//!
//! | 方法 | 路径 | handler | 本文件 |
//! | --- | --- | --- | --- |
//! | GET / POST | `/api/squads/`（+ `/api/squads` 别名） | `ListSquads` / `CreateSquad` | `crud` |
//! | GET / PUT / DELETE | `/api/squads/:id/`（+ 无斜杠别名） | `GetSquad` / `UpdateSquad` / `DeleteSquad` | `crud` |
//! | GET / POST / DELETE | `/api/squads/:id/members` | `ListSquadMembers` / `Add` / `RemoveSquadMember` | `members` |
//! | GET | `/api/squads/:id/members/status` | `ListSquadMemberStatus` | `members` |
//! | PATCH | `/api/squads/:id/members/role` | `UpdateSquadMemberRole` | `members` |
//!
//! 分层：仓储 `mc_repos::squad`（`squad` + `squad_member` + 跨域只读投影），
//! 纯推导 `mc_squad::status`（presence 五桶），鉴权 [`SquadScope`]。
//!
//! # 尾斜杠形态（门 ⑦ `slash_alias_audit.py`）
//!
//! 上游 `chi` 的 `router.Route("/api/squads", func(r){ r.Get("/")… })` 同时服务
//! `/api/squads` 与 `/api/squads/`，而 axum 0.7 的 matchit **不**做归一化
//! （`MissingTrailingSlash` → 404，不是 307）⇒ 两个形态都得注册，且方法集合逐字相同
//! （`:id/` 同理）。`members` / `members/status` / `members/role` 是 plain 子路由 ⇒
//! **只有**一个形态（多注册会被判 `EXTRA_ALIAS`）。
//!
//! # 有意偏离（逐条都可核对）
//!
//! 1. **`avatar_url` 不做签名**：上游写库前过 `acceptAvatarURL`（对象存储签名 + 越权
//!    校验，`avatar.go:254`）。本仓没有存储接线，原样存/读（M3-5 agents 面同口径）。
//! 2. **不广播 WS 事件**：上游每个写端点 `h.publish(protocol.EventSquad*)`。本仓
//!    `crates/mc-http/src/routes/*` 目前**没有任何** route 切片发布事件（grep
//!    `publish`/`broadcast` 为空），squad 面跟随现状；M3-7 daemon 的广播是另一条线。
//! 3. **`UpdateSquad` 的 invoke 门校验位置**：上游在事务内、`LockAgentForAutopilotAssignment`
//!    （`FOR UPDATE`）之后做 `memberCanWireAgent`。本仓在事务**前**做（agent 查询用
//!    `SquadRepo::agent_in_workspace`），事务内仍会重新锁并校验该 agent 存在 —— 差别只是
//!    并发权限变更可能早一条语句被观察到。
//! 4. **`DELETE /api/squads/:id/members` 兼容 query**：上游只读 JSON body
//!    `{member_type, member_id}`；本仓额外接受「空 body + `?member_type=&member_id=`」
//!    （路由路径里没有成员位）。body 非空时仍严格按上游形状解码。
//! 5. **`identifier` 前缀**：上游 `getIssuePrefix` = `workspace.issue_prefix`，为空时回退
//!    `legacyIssuePrefixFromName(name)`。本仓回退到 `issue_prefix_from_slug(workspace.slug)`
//!    （M2 `issue` 面的既有口径），workspace 行读不到时返回 `""`（与上游一致）。
//! 6. **跨域写**：`DeleteSquad` 会把 `issue` / `autopilot` 的 assignee 转给 leader，
//!    `UpdateSquad` 会在新 leader 未绑 runtime 时暂停该 squad 的 autopilot —— 都是上游
//!    handler 语义的一部分，按 raw SQL 实现在 `mc_repos::squad`（`agent` /
//!    `agent_runtime` / `agent_task_queue` 保持**只读**）。
//! 7. `canManageSquad` 跟随上游 MUL-4223：**admin/owner 管全部；普通成员只管自己创建的**。
//!    读端点（list/get/members）只要求 workspace 成员（上游是路由组中间件）。

use std::sync::Arc;

use axum::routing::{get, patch};
use axum::Router;

use crate::state::AppState;

mod access;
mod crud;
mod dto;
mod members;

pub(crate) use access::SquadScope;
// 错误/解析 helper 复用 M3-5 的本地副本（`routes/agents.rs`），全仓 route 切片同口径：
// `tasks.rs` 也是这么用的。
pub(crate) use crate::routes::agents::{bad_request, parse_uuid, repo_err};

/// `/api/squads*` 的 10 条路由（+ 4 条尾斜杠别名）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/squads/",
            get(crud::list_squads).post(crud::create_squad),
        )
        .route(
            "/api/squads",
            get(crud::list_squads).post(crud::create_squad),
        )
        .route(
            "/api/squads/:id/",
            get(crud::get_squad)
                .put(crud::update_squad)
                .delete(crud::delete_squad),
        )
        .route(
            "/api/squads/:id",
            get(crud::get_squad)
                .put(crud::update_squad)
                .delete(crud::delete_squad),
        )
        .route(
            "/api/squads/:id/members",
            get(members::list_members)
                .post(members::add_member)
                .delete(members::remove_member),
        )
        .route(
            "/api/squads/:id/members/status",
            get(members::list_member_status),
        )
        .route(
            "/api/squads/:id/members/role",
            patch(members::update_member_role),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_builds_without_panicking() {
        // 同 path+method 重复注册会在 build 期 panic；本断言覆盖 10 条上游路由
        // + 4 条尾斜杠别名（`/api/squads/`、`/api/squads`、`/api/squads/:id/`、
        // `/api/squads/:id`）互不冲突。
        let _ = router();
    }
}
