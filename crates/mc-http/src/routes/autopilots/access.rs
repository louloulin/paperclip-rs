//! autopilot 面**权限/可见性**共享模块（**非路由**，不含 `router()`）。
//!
//! - **写者**：M5-1（`docs/44` §3.2）。
//! - **上游**：`autopilotWriteByOwnership`14 + `memberCanWriteAutopilot`70 +
//!   `autopilotActingUserID`36 + `requireAutopilotActingMember`35 + `loadAutopilotInWorkspace`25
//!   （`handler/autopilot.go` 的权限段，合计 157 行 = ⑨ 的 `TestAutopilot…Forbidden` 族真值）。
//! - **两条判负语义**（`docs/44` §4.2）：
//!   1. **非本 workspace / 非成员一律 404**（不是 403）—— 与 `invitations::require_workspace_member`
//!      的既有约定一致，避免资源存在性泄露；
//!   2. **成员但无写权 → 403**（`memberCanWriteAutopilot` 判负）。
//! - **不要在本文件重新实现 workspace 解析**：复用 `crate::routes::workspaces` /
//!   `invitations` 的既有入口（`resolve_workspace` 一族），否则会出现第二份成员真值。
//! - **写面（M5-2/3/4）只调用本模块**，不要各自复制一份权限判断。
//!
//! # M5-1 落地了什么（`docs/46-M5-1-READ-FACE.md`）
//!
//! 上游那 157 行里，读面真正用到的是四条：**成员门槛**、**写权判定**、**加载单机**、
//! **acting user**。本地分别落成：
//!
//! | 上游 | 本地 | 判负 |
//! | --- | --- | --- |
//! | `requireWorkspaceMember`（路由组中间件） | [`require_member`] | 非成员 → **404** `workspace` |
//! | `loadAutopilotInWorkspace` | [`load_in_workspace`] | 非 UUID → **400**；跨工作区/不存在 → **404** `autopilot` |
//! | `autopilotWriteByOwnership` | [`write_by_ownership`]（纯函数） | —— 返回 `bool`，不判负 |
//! | `memberCanWriteAutopilot` | [`member_can_write`] | —— 返回 `bool`（**DB 故障也是 `false`**，见下） |
//! | `autopilotActingUserID` | [`acting_user_id`] | 见「已知偏离」 |
//!
//! ## 写权判定的两条腿（逐字对照上游）
//!
//! ```text
//! write = role ∈ {owner, admin}                       // 工作区管理员永远能写
//!       ∨ (created_by_type == "member" ∧ created_by_id == caller)   // 人类创建者
//!       ∨ is_autopilot_collaborator(autopilot_id, caller)           // 显式授权（≠ 管理授权）
//! ```
//!
//! 两个细节不能抄漏：
//!
//! 1. **创建者那条腿带 `created_by_type == "member"` 前置**：agent 创建的 autopilot 即使
//!    `created_by_id` 恰好等于某个 user id（跨表同 UUID 的假想情况）也**不**给写权。
//! 2. **协作者能写但不能再授权**：`can_manage_access` = 第一、二条腿（[`can_manage_access`]），
//!    协作者在授权列表上是 **false**（上游 MUL-3807 的原话："collaborators can write but cannot
//!    re-grant"）。
//!
//! ## 为什么协作者查询出错时**不**报 500（`member_can_write` 的 `unwrap_or(false)`）
//!
//! 上游写的是 `granted, err := …; return err == nil && granted` —— 查询失败**静默降级成
//! `false`**。这不是疏忽：`can_write=false` 的后果是「详情响应里的 webhook 凭据被抹掉」，
//! 是**安全的那一侧**；而 500 会让整个详情/列表不可用。所以本地逐字保留这个 fail-closed 降级，
//! 只在**权限为真的分支**上要求查询成功。
//!
//! # 已知偏离（记入 `docs/46` §5）
//!
//! **`acting_user_id` 就是已认证用户**。上游的 "acting" 用户是「agent run 的发起人」——
//! 它先看 `X-Actor-Source: task_token`（鉴权中间件盖的章）+ `X-Agent-ID`，再用
//! `invokeOriginatorFromRequest` 把 agent 身份换回发起人 user id；目的是让「agent 代跑」时
//! 也按**发起人**的权限判负，而不是按 agent 的 runtime owner。本仓的 dev-mode 鉴权
//! （`AuthUser` = `X-Multica-User-Id`）**没有** task-token 中间件与 originator 平面 ——
//! 那一套属 M3-7 的 daemon / agent-run 面（`docs/41-M3-6-TASK-QUEUE.md`）。⇒ 本地读面
//! 的 acting user 恒为已认证 user；人类调用者的契约（含 `can_write` / 凭据抹除）逐字成立。
//! 等 daemon 面落 originator 后，只需把 [`acting_user_id`] 换成真正实现，**上面四条判定
//! 一行都不用改**。

use mc_core::Id;
use mc_errors::Error;
use mc_repos::autopilot::{AutopilotRepo, AutopilotRow};

use crate::routes::agents::{parse_uuid, workspace_role};
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::not_found;
use crate::state::AppState;

/// 「工作区管理员」那两个角色（上游 `roleAllowed(member.Role, "owner", "admin")`）。
///
/// 顺序与上游一致；`role` 是 `member.role` 的自由文本列（`guest` / `member` / `admin` / `owner`）。
pub const WRITE_ROLES: [&str; 2] = ["owner", "admin"];

/// `autopilot.created_by_type` 的人类分支（与 `autopilot_collaborator.user_type` 共用 `member` 词表）。
pub const CREATOR_TYPE_MEMBER: &str = mc_repos::autopilot::USER_TYPE_MEMBER;

/// 上游 `roleAllowed(role, WRITE_ROLES…)`。
#[must_use]
pub fn role_may_write(role: &str) -> bool {
    WRITE_ROLES.contains(&role)
}

/// 上游 `autopilotWriteByOwnership` 的**最低层形状**：只吃判定真正需要的两个列。
///
/// 单独抽出来是为了让**列表行**（[`mc_repos::autopilot::AutopilotListRow`]）不必为了调用它
/// 先 `.base()` 拷一个 `AutopilotRow`；三处调用共用这一份真值。
#[must_use]
pub fn owns_for_write(
    created_by_type: &str,
    created_by_id: uuid::Uuid,
    role: &str,
    user_id: Id,
) -> bool {
    role_may_write(role) || (created_by_type == CREATOR_TYPE_MEMBER && created_by_id == user_id.0)
}

/// 上游 `autopilotWriteByOwnership`：**纯函数**（不查库），因此 `can_manage_access` 也用它。
///
/// 注意它**不**包含协作者那条腿 —— 这正是 [`can_manage_access`] 与
/// [`write_by_ownership`] 在本文件里共用一个实现、而 [`member_can_write`] 另外多查一次的原因。
#[must_use]
pub fn write_by_ownership(row: &AutopilotRow, role: &str, user_id: Id) -> bool {
    owns_for_write(&row.created_by_type, row.created_by_id, role, user_id)
}

/// 上游 `canManageAccess = autopilotWriteByOwnership(...)`：管理授权列表比「能写」更窄。
#[must_use]
pub fn can_manage_access(row: &AutopilotRow, role: &str, user_id: Id) -> bool {
    write_by_ownership(row, role, user_id)
}

/// 上游 `memberCanWriteAutopilot`：ownership ∨ 协作者授权（**DB 故障降级为 `false`**，见模块文档）。
pub async fn member_can_write(
    repo: &AutopilotRepo,
    row: &AutopilotRow,
    role: &str,
    user_id: Id,
) -> bool {
    if write_by_ownership(row, role, user_id) {
        return true;
    }
    repo.is_collaborator(row.id, user_id).await.unwrap_or(false)
}

/// 上游 `autopilotActingUserID` 的本地等价物（见模块文档「已知偏离」）。
#[must_use]
pub fn acting_user_id(user: AuthUser) -> Id {
    user.id()
}

/// 上游 `requireWorkspaceMember` 的角色面：非成员 → 404 `workspace`。
///
/// 复用 `agents::workspace_role`（本仓 14 个面共用的那一份成员真值），不要在这里写第二条 SQL。
pub async fn require_member(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> Result<String, Error> {
    workspace_role(state, workspace_id, user_id).await
}

/// 上游 `loadAutopilotInWorkspace`：非 UUID → 400；不存在 / **跨工作区** → 404 `autopilot`。
///
/// 「跨工作区也是 404」是刻意的（上游 `GetAutopilotInWorkspace` 的 `WHERE workspace_id = $2`）：
/// 同一个 id 在别的 workspace 下必须与「不存在」不可区分，否则就是一个存在性探测器。
pub async fn load_in_workspace(
    repo: &AutopilotRepo,
    raw_id: &str,
    workspace_id: Id,
) -> Result<AutopilotRow, Error> {
    let id = parse_uuid(raw_id, "autopilot id")?;
    match repo.get_in_workspace(id, workspace_id).await {
        Ok(row) => Ok(row),
        Err(mc_repos::RepoError::NotFound) => Err(not_found("autopilot")),
        Err(err) => Err(map_repo_err(err)),
    }
}

/// 仓储错误 → HTTP 错误（**只**用于真正的数据库故障：`NotFound` 已在调用点折成资源 404）。
fn map_repo_err(err: mc_repos::RepoError) -> Error {
    match err {
        mc_repos::RepoError::NotFound => not_found("autopilot"),
        mc_repos::RepoError::Conflict => Error::Conflict {
            message: "autopilot state conflict".into(),
        },
        mc_repos::RepoError::Db(message) => Error::Database(message),
    }
}

/// 成员门槛 + 写权：成员但没有写权 → **403**（上游 `requireAutopilotActingMember` 的判负面）。
///
/// 读面（M5-1 的四条路由）**不调用**它 —— 读面只算 `can_write` 布尔值并据此抹凭据；
/// 它留给 M5-2/3/4 的写面（那里判负是 403 而不是「不显示」）。
pub async fn require_write(
    repo: &AutopilotRepo,
    row: &AutopilotRow,
    role: &str,
    user_id: Id,
) -> Result<(), Error> {
    if member_can_write(repo, row, role, user_id).await {
        Ok(())
    } else {
        Err(Error::Forbidden {
            message: "insufficient permission for this autopilot".into(),
        })
    }
}
