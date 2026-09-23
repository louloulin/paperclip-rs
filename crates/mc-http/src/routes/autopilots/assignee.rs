//! M5-0 anchor：autopilot **assignee 解析与校验**共享模块（**非路由**，不含 `router()`）。
//!
//! - **写者**：M5-2（`docs/44` §3.2），但 M5-3 的 trigger 写面会**读**它（`trigger.rs` 要按同一
//!   套规则校验 assignee 快照）。
//! - **上游**：`validateAutopilotAssigneeForSave`76 + `isValidAutopilotAssigneeType`19
//!   （`handler/autopilot.go`，合计 135 行 —— §6.3 明确把它从 `crud.rs` 拆出来防门 ⑩）。
//! - **二态**：`assignee_type ∈ {agent, squad}`（`042` 建列、`096` 定为二态）。
//! - **`squad` 的解析规则（Squad-as-Leader，`096` / MUL-2429）**：squad 不直接当 agent 用 ——
//!   运行期要解析到 `squad.leader_id`；`096` 同时给 `autopilot_run` 加了 `squad_id` 快照列
//!   （跑的时候的队长是谁，以 run 行为准，不要事后重算）。
//! - **`assignee_id` 的语义随 `assignee_type` 变**（`042` 原建的是 `REFERENCES agent(id)`，
//!   已由 `096` 放开为多态）⇒ 任何「JOIN agent」都要先判 `assignee_type`。
//!
//! # M5-2 落地了什么
//!
//! 上游那 95 行是**保存期校验**（保存期存在性 + 归档 + runtime + 私密队长的 invoke 门）。
//! 本地落成三个部分，边界刻意不重叠：
//!
//! | 上游 | 本地 | 备注 |
//! | --- | --- | --- |
//! | `isValidAutopilotAssigneeType`19 | [`is_valid_assignee_type`] | **领域纯函数**在 `mc_autopilot::collaborator`（M5-3 也要用），此处只做转出 |
//! | `validateAutopilotAssigneeForSave`76 | [`validate_assignee_for_save`] | 本文件；**必须传事务连接**（要 `FOR SHARE` 的锁） |
//! | 各端点的 403/409 扁平拒绝体 | [`FlatErrorBody`] + [`forbidden_write`] / [`forbidden_access`] / [`update_conflict`] | M5-3 的 trigger 拒绝体同源复用（见下） |
//!
//! ## 「嵌套 vs 扁平」的判定规则（M5-2 最容易抄错的一处）
//!
//! 上游每个错误都走 `writeError` 或 `writeErrorCode`，两者**都是扁平体**：
//!
//! ```text
//! writeError(w, 400, "title is required")      → {"error": "title is required"}
//! writeErrorCode(w, 409, "…_conflict", "…")    → {"error": "…", "code": "…"}
//! ```
//!
//! 本仓的既有约定是**嵌套体** `{"error":{"code":…,"message":…}}`（`ApiError`），M5-1 的读面
//! 也照这个约定落（`invalid workspace id` 等只保留文案，不保留扁平形状）。因此本片的规则是：
//!
//! - **上游没有 `code` 的错误 → 嵌套体**（`title is required` / assignee 的 400/422 /
//!   私密队长 403 …）：直接用 `mc_errors::Error` 走 `ApiError`，形状与全仓其余端点一致；
//! - **上游显式带 `code` 的错误 → 扁平体**：那是 CLI 的**机器可读契约**（上游注释原话：
//!   "the CLI keys its actionable output on these rather than on the English sentence"），
//!   折算成嵌套体就把唯一的拒绝码丢了 ⇒ 用 [`FlatErrorBody`] 逐字发扁平体。
//!
//! 本片因此有三个扁平端点错误：**写权 403**（`autopilot_forbidden`）、**改授权 403**
//! （同为 `autopilot_forbidden`，文案不同）、**乐观并发 409**（`autopilot_update_conflict`）。
//!
//! ## 拒绝码词表（上游 `handler/autopilot.go` 的 const 块，逐字对照）
//!
//! | 常量 | 值 | 用在哪 |
//! | --- | --- | --- |
//! | [`CODE_NO_ORIGINATOR`] | `autopilot_no_originator` | 无人类发起人的写请求（**本片不可达**，见下） |
//! | [`CODE_FORBIDDEN`] | `autopilot_forbidden` | 写权 / 改授权被拒 |
//! | [`CODE_ACTOR_NOT_MEMBER`] | `autopilot_actor_not_member` | Create 的发起人不是成员（**本片不可达**） |
//! | [`CODE_UPDATE_CONFLICT`] | `autopilot_update_conflict` | Update 的乐观并发 |
//! | `autopilot_trigger_no_originator` / `autopilot_trigger_forbidden` | —— | **M5-3**（`trigger.rs` 自己的文件） |
//!
//! 前两条不可达的原因同 M5-1 的 `acting_user_id` 偏离：本地没有 task-token / originator 平面，
//! 调用者**就是**已认证用户，成员门槛由 [`require_member`](super::access::require_member) 的
//! 404 承担（上游 `requireAutopilotActingMember` 对 member 调用者同样落 404）。等 M3-7 的
//! daemon 面落了 originator，这两码与 `acting_user_id` 一起换实现 —— 常量已就位，不必再改形状。
//!
//! ## 加锁顺序（与 `mc_repos::autopilot::write` 的锁序表同源）
//!
//! [`validate_assignee_for_save`] 在**调用方的事务里**取 `FOR SHARE`：单 agent 取一行；squad 先取
//! squad 行、再取 `squad.leader_id` 那一行（顺序固定，抄错就是死锁）。它必须排在订阅者 advisory
//! 锁与成员重申**之后**、autopilot 行锁**之前**。
//!
//! ## 私密队长的 invoke 门（上游 `canInvokeAgent`）
//!
//! squad 分支的最后一步是「配置者能不能调用队长」：`private` 队长对非 owner **恒拒**（无 admin
//! 越权 —— MUL-3963 的原话是 "a workspace admin must NOT be able to invoke someone's private
//! agent"），`public_to` 队长按白名单命中。本文件用 [`AgentScope::can_invoke`](super::super::agents::AgentScope)
//! 的同一套语义（owner ∨ `public_to` ∧ 命中白名单），**不**复用 `squads/access.rs` 的
//! `member_can_wire_agent`（那一条多了 admin 越权，是「接线」而不是「invoke」的判定）。
//! 全仓 invoke 门因此有三处调用点（`agents.rs` 本体、`squads/access.rs`、本文件），
//! 语义变更必须三处同动。
//!
//! # 已知偏离
//!
//! 1. **`parse_uuid` 的文案**：上游 `parseUUIDOrBadRequest` 是 `"invalid " + fieldName`，
//!    本仓全仓约定是 `"<field> must be a valid uuid"`（`agents::parse_uuid`）⇒ 沿用本地文案。
//! 2. **422 不经过 `AutopilotError`**：`mc-autopilot` 的错误类型没有 422 变体（M5-1 的映射表
//!    只到 4xx/409/500），而 assignee 的归档 / 缺 runtime 都是 422 ⇒ 本文件直接构
//!    `Error::Unprocessable{…}`（[`unprocessable`]），返回类型是 `Result<(), Error>`
//!    （等价于上游那个 `bool`，调用方 `?` 即可）。M5-3/M5-4 若需要 422 走同一处；要收进类型的话在
//!    `mc-autopilot/src/error.rs` **加变体**（该文件明确允许加变体）。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

use mc_repos::autopilot::write::{
    lock_agent_for_autopilot_assignment, lock_squad_for_autopilot_assignment, AssigneeAgentRow,
};
use mc_repos::RepoError;

use crate::routes::agents::{bad_request, forbidden, repo_err, AgentScope};

/// 领域侧的 assignee 二态判定（实现与 `ASSIGNEE_TYPES` 都在 `mc_autopilot::collaborator`）。
///
/// 转出的理由：`autopilots/mod.rs` 的模块表把 `isValidAutopilotAssigneeType`19 记在本文件上，
/// 而 M5-3 的 trigger 校验也要这个谓词 ⇒ 实现留在领域层，本文件只做入口。
pub(crate) use mc_autopilot::collaborator::is_valid_assignee_type;

/// `assignee_type` 的 agent 分支（本文件的分支标签，值同 `mc_autopilot::collaborator`）。
const ASSIGNEE_TYPE_AGENT: &str = mc_autopilot::collaborator::ASSIGNEE_TYPE_AGENT;

/// `assignee_type` 的 squad 分支。
const ASSIGNEE_TYPE_SQUAD: &str = mc_autopilot::collaborator::ASSIGNEE_TYPE_SQUAD;

// ---------------------------------------------------------------------------
// 扁平拒绝体（上游 `writeErrorCode`）
// ---------------------------------------------------------------------------

/// 无人类发起人（本片不可达，见模块文档）。
// `allow(dead_code)`：本片没有 task-token / originator 平面 ⇒ 这个码发不出来，但它属于上游的
// **公开拒绝码词表**（CLI 按码分支），提前落地 + 单测锁值比事后补形状更安全。
#[allow(dead_code)]
pub(crate) const CODE_NO_ORIGINATOR: &str = "autopilot_no_originator";

/// 写权 / 改授权被拒（`memberCanWriteAutopilot` / `autopilotWriteByOwnership` 判负）。
pub(crate) const CODE_FORBIDDEN: &str = "autopilot_forbidden";

/// Create 的发起人不是本工作区成员（本片不可达，见模块文档）。
#[allow(dead_code)]
pub(crate) const CODE_ACTOR_NOT_MEMBER: &str = "autopilot_actor_not_member";

/// Update 的乐观并发冲突。
pub(crate) const CODE_UPDATE_CONFLICT: &str = "autopilot_update_conflict";

/// `autopilotWriteRefusal.forbiddenMsg`（逐字）。
pub(crate) const MSG_WRITE_FORBIDDEN: &str = "only the autopilot creator, a workspace admin, or a granted collaborator can manage this autopilot";

/// `autopilotAccessRefusal.forbiddenMsg`（逐字；协作者能写但不能转授权，MUL-3807）。
pub(crate) const MSG_ACCESS_FORBIDDEN: &str =
    "only the autopilot creator or a workspace admin can manage access";

/// `UpdateAutopilot` 的 409 文案（逐字，含句尾句号）。
pub(crate) const MSG_UPDATE_CONFLICT: &str =
    "the autopilot changed while it was being edited; reload and try again.";

/// 上游 `writeErrorCode` 的响应体：**扁平** `{"error": "msg", "code": "code"}`。
///
/// 字段顺序与上游 `writeJSON` 的 `map[string]any{"error":…, "code":…}` 一致（`error` 在前）；
/// `serde_json` 按结构体字段序输出，所以这里的字段顺序是契约的一部分。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FlatErrorBody {
    /// 人类可读消息（**字符串**，不是 `{"code","message"}` 对象）。
    pub error: String,
    /// 机器可读的稳定拒绝码。
    pub code: String,
}

impl FlatErrorBody {
    /// 构造扁平错误响应（状态码由调用方给：本片有 403 与 409 两种）。
    #[must_use]
    pub fn response(status: StatusCode, code: &str, message: &str) -> Response {
        (
            status,
            axum::Json(Self {
                error: message.to_string(),
                code: code.to_string(),
            }),
        )
            .into_response()
    }
}

/// 写权 403（`requireAutopilotWrite` 判负）。
#[must_use]
pub(crate) fn forbidden_write() -> Response {
    FlatErrorBody::response(StatusCode::FORBIDDEN, CODE_FORBIDDEN, MSG_WRITE_FORBIDDEN)
}

/// 改授权 403（`requireAutopilotAccessManagement` 判负；协作者也拿不到这一条）。
#[must_use]
pub(crate) fn forbidden_access() -> Response {
    FlatErrorBody::response(StatusCode::FORBIDDEN, CODE_FORBIDDEN, MSG_ACCESS_FORBIDDEN)
}

/// 乐观并发 409（`UpdateAutopilot` 的 `updated_at` 不匹配）。
#[must_use]
pub(crate) fn update_conflict() -> Response {
    FlatErrorBody::response(
        StatusCode::CONFLICT,
        CODE_UPDATE_CONFLICT,
        MSG_UPDATE_CONFLICT,
    )
}

// ---------------------------------------------------------------------------
// 保存期 assignee 校验
// ---------------------------------------------------------------------------

/// 上游 `validateAutopilotAssigneeForSave`(76)：保存期存在性 / 归档 / runtime / invoke 门。
///
/// **必须在调用方的事务里跑**（`conn` 是事务连接）：上游这三条查询都带锁，正是为了让「创建 /
/// 改派 / 恢复 active autopilot」与「runtime teardown / squad 改队长」互斥。多取一列
/// （`owner_id` / `permission_mode`）不是多余：squad 分支的 invoke 门要在**同一把锁**下读它们。
///
/// `require_runtime` = 目标状态是否 `active`（上游 `requireRuntime = nextStatus == "active"`）：
/// 暂停中的 autopilot 允许指向没 runtime 的 agent，一旦要 active 就必须有。
///
/// 返回 `Err(Error)` 表示拒绝（调用方 `?` 一下就成响应；等价于上游那个 `bool` 返回值）。
/// 调用方不能把它当成 500 兜底 —— 分支覆盖见下表：
///
/// | 情形 | 状态码 | 文案 |
/// | --- | ---: | --- |
/// | 不是本工作区的 agent（`kind<>'user'` 也算） | 400 | `assignee must be a valid agent in this workspace` |
/// | agent 已归档 | 422 | `assignee agent is archived; pick a different agent` |
/// | agent 无 runtime（`require_runtime`） | 422 | `assignee agent needs a runtime before this autopilot can be active` |
/// | 不是本工作区的 squad | 400 | `assignee must be a valid squad in this workspace` |
/// | squad 已归档 | 422 | `squad is archived; pick a different squad` |
/// | 队长 agent 找不到 | 400 | `squad leader agent not found` |
/// | 队长已归档 | 422 | `squad leader is archived; pick a different squad or rotate the leader before assigning autopilot` |
/// | 队长无 runtime（`require_runtime`） | 422 | `squad leader needs a runtime before this autopilot can be active` |
/// | 配置者调不动私密队长 | 403 | `cannot assign autopilot to squad with private leader` |
/// | 其他 `assignee_type` | 400 | `assignee_type must be agent or squad` |
pub(crate) async fn validate_assignee_for_save(
    conn: &mut PgConnection,
    scope: &AgentScope,
    assignee_type: &str,
    assignee_id: Uuid,
    require_runtime: bool,
) -> Result<(), mc_errors::Error> {
    let workspace_id = scope.workspace_id.0;
    match assignee_type {
        ASSIGNEE_TYPE_AGENT => {
            let Some(agent) = lock_agent_for_autopilot_assignment(conn, assignee_id, workspace_id)
                .await
                .map_err(db_failure)?
            else {
                return Err(bad_request(
                    "assignee must be a valid agent in this workspace",
                ));
            };
            check_agent_ready(
                &agent,
                require_runtime,
                "assignee agent is archived; pick a different agent",
                "assignee agent needs a runtime before this autopilot can be active",
            )
        }
        ASSIGNEE_TYPE_SQUAD => {
            let Some(squad) = lock_squad_for_autopilot_assignment(conn, assignee_id, workspace_id)
                .await
                .map_err(db_failure)?
            else {
                return Err(bad_request(
                    "assignee must be a valid squad in this workspace",
                ));
            };
            if squad.archived_at.is_some() {
                return Err(unprocessable("squad is archived; pick a different squad"));
            }
            // Squad-as-Leader（`096`）：真正的执行者是队长，所以锁的是**队长那一行**。
            let Some(leader) =
                lock_agent_for_autopilot_assignment(conn, squad.leader_id, workspace_id)
                    .await
                    .map_err(db_failure)?
            else {
                return Err(bad_request("squad leader agent not found"));
            };
            check_agent_ready(
                &leader,
                require_runtime,
                "squad leader is archived; pick a different squad or rotate the leader before assigning autopilot",
                "squad leader needs a runtime before this autopilot can be active",
            )?;
            // 私密队长门（上游 `canInvokeAgent`，见模块文档）：调用者得能**跑**这个队长。
            if !leader_is_invocable(scope, &leader).await? {
                return Err(forbidden(
                    "cannot assign autopilot to squad with private leader",
                ));
            }
            Ok(())
        }
        _ => Err(bad_request("assignee_type must be agent or squad")),
    }
}

/// 归档 / runtime 两句判定（agent 分支与 squad 队长分支只差文案，判定顺序一致）。
fn check_agent_ready(
    agent: &AssigneeAgentRow,
    require_runtime: bool,
    archived_message: &str,
    no_runtime_message: &str,
) -> Result<(), mc_errors::Error> {
    if agent.archived_at.is_some() {
        return Err(unprocessable(archived_message));
    }
    if require_runtime && agent.runtime_id.is_none() {
        return Err(unprocessable(no_runtime_message));
    }
    Ok(())
}

/// 上游 `canInvokeAgent` 的 **member actor 分支**（与 `AgentScope::can_invoke` 同语义）。
///
/// 顺序逐字对照：**先** owner 那条腿（owner 恒可调用，连白名单都不读），**再** `public_to`
/// + 白名单命中。`private`（含未知 mode）对非 owner 一律拒 —— **无 admin 越权**。
///
/// 为什么不直接调 `AgentScope::can_invoke`：那个函数吃 `AgentRow`（完整行），而这里手上是
/// `FOR SHARE` 锁住的那**三列 + owner/permission**（`AssigneeAgentRow`）。为此多读一次 agent
/// 会绕开刚才那把锁所保护的那一列的原子性，所以宁可复制这 8 行判定；全仓 invoke 门三处
/// 调用点（见模块文档）必须同动。
async fn leader_is_invocable(
    scope: &AgentScope,
    leader: &AssigneeAgentRow,
) -> Result<bool, mc_errors::Error> {
    if leader.owner_id == Some(scope.user_id.0) {
        return Ok(true);
    }
    if leader.permission_mode != mc_repos::agent::PERMISSION_MODE_PUBLIC_TO {
        return Ok(false);
    }
    let targets = scope.targets_of(mc_core::Id(leader.id)).await?;
    Ok(scope.member_hits_targets(&targets))
}

/// 422 的嵌套错误（无 `code` ⇒ 本仓形状与状态码映射，见模块文档「已知偏离 2」）。
fn unprocessable(message: &str) -> mc_errors::Error {
    mc_errors::Error::Unprocessable {
        message: message.to_string(),
    }
}

/// 数据库故障 → 500（`RepoError` 折法沿用本仓 `repo_err`；`NotFound` 不可能出现在这两个
/// 加锁查询里，它们用的是 `fetch_optional`）。
fn db_failure(err: RepoError) -> mc_errors::Error {
    repo_err(err, "autopilot")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignee_type_predicate_is_reexported() {
        assert!(is_valid_assignee_type("agent"));
        assert!(is_valid_assignee_type("squad"));
        assert!(!is_valid_assignee_type("member"));
    }

    #[test]
    fn flat_bodies_carry_upstream_codes_and_verbatim_messages() {
        assert_eq!(CODE_FORBIDDEN, "autopilot_forbidden");
        assert_eq!(CODE_UPDATE_CONFLICT, "autopilot_update_conflict");
        assert_eq!(CODE_NO_ORIGINATOR, "autopilot_no_originator");
        assert_eq!(CODE_ACTOR_NOT_MEMBER, "autopilot_actor_not_member");
        assert_eq!(
            FlatErrorBody {
                error: MSG_WRITE_FORBIDDEN.to_string(),
                code: CODE_FORBIDDEN.to_string(),
            },
            FlatErrorBody {
                error: "only the autopilot creator, a workspace admin, or a granted collaborator can manage this autopilot".to_string(),
                code: "autopilot_forbidden".to_string(),
            }
        );
        assert!(MSG_UPDATE_CONFLICT.ends_with('.'));
        assert!(MSG_ACCESS_FORBIDDEN.starts_with("only the autopilot creator or a workspace admin"));
    }
}
