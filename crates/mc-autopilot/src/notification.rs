//! autopilot 通知收件人解析。
//!
//! - **写者**：M5-1。上游 `service/autopilot_notification_recipient.go`91。
//! - **调用者**：M5-4 的 dispatch（run 终态通知）、M5-6 的 wakeup、以及 M9 的额度通知面。
//! - **不做投递**：本文件只回答「这条通知该发给谁」。真正的发送（邮件 / WS / 站内）在
//!   `service/autopilot_notification.go`，属 M5-4 的 dispatch 段。
//!
//! # 收件人规则（上游逐字对照）
//!
//! | `created_by_type` | 收件人 | 上游行为 |
//! | --- | --- | --- |
//! | `member` | 创建者**本人**，但必须仍是本工作区成员 | `GetMemberByUserAndWorkspace` 命中才算 |
//! | `agent` | agent 的 `owner_id`，且该 owner 是本工作区成员 | `GetAgent` → 校验 `agent.WorkspaceID == autopilot.WorkspaceID` → 查 member |
//! | 其它（含 `system`） | 无 | 直接 `ok=false` |
//!
//! 两处「查不到就 `ok=false`」是**特性**而不是容错：通知发不出去时上游**不**把它退化成
//! 「发给工作区所有人」，也不报错 —— 离职创建者 / 已删 agent / 跨工作区 agent 都只是静默无收件人。
//! 本文件保留同一语义，并把「为什么没有收件人」拆成 [`NoRecipientReason`]，供 M5-4 记录日志
//! （上游只返回裸 `ok=false`，本地多这一层是为了可诊断，**不改变**「无收件人不是错误」这条契约）。
//!
//! # 明确不落的部分
//!
//! 上游同文件的 [`ListWorkspaceManagerNotificationRecipients`]（owners + admins）**不在本片**：
//! 它的唯一调用点是 `autopilot_quota_notifications.go` 的额度通知（M9/Cloud 面，本仓没有
//! entitlement 平面 ⇒ 那条路径当前不可达）。落一个 0 调用者的仓储方法只会让 ③ 的
//! `dead_code`/`unused` 面变脏。记录见 `docs/46-M5-1-READ-FACE.md` §7。
//!
//! [`ListWorkspaceManagerNotificationRecipients`]: https://github.com/louloulin/multica/blob/main/server/internal/service/autopilot_notification_recipient.go

use mc_repos::agent::AgentRepo;
use mc_repos::autopilot::{AutopilotRepo, AutopilotRow, USER_TYPE_MEMBER};
use uuid::Uuid;

use crate::error::AutopilotError;

/// 收件人（上游 `AutopilotNotificationRecipient`：`{Type, ID}`）。
///
/// `user_type` 目前恒为 `"member"` —— 上游两条分支都只产 `member`，agent 只是**用来找人的中介**
/// 而不是收件人类型。字段保留在 wire 上（M5-4 的 fanout 会把它序列化进通知载荷）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutopilotNotificationRecipient {
    /// 主体类型（恒为 `member`）。
    pub user_type: &'static str,
    /// 收件人 user id。
    pub user_id: Uuid,
}

impl AutopilotNotificationRecipient {
    /// 构造 member 收件人。
    #[must_use]
    pub const fn member(user_id: Uuid) -> Self {
        Self {
            user_type: USER_TYPE_MEMBER,
            user_id,
        }
    }
}

/// 「没有收件人」的原因（上游只给 `ok=false`；本地把它显式化以便日志）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoRecipientReason {
    /// `created_by_type` 既不是 `member` 也不是 `agent`。
    UnsupportedCreatorType,
    /// `member` 创建者已不在本工作区（离职 / 工作区被清空）。
    CreatorNotAMember,
    /// `agent` 创建者不存在，或不在本工作区（跨工作区 agent 不通知）。
    CreatorAgentUnusable,
    /// agent 的 `owner_id` 为空（无主 agent）。
    AgentHasNoOwner,
    /// agent 的 owner 已不是本工作区成员。
    AgentOwnerNotAMember,
}

/// 创建主体类型（`autopilot.created_by_type` 的两个可解析取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatorKind {
    /// 人类成员。
    Member,
    /// agent（其 owner 才是收件人）。
    Agent,
}

impl CreatorKind {
    /// 解析 `created_by_type`；其它值（含 `system`）返回 `None` = 无收件人。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "member" => Some(Self::Member),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// 纯映射：`autopilot.created_by_type` → 创建主体类型（DB 无关，可单测）。
///
/// 独立成函数是为了让「`system` 创建 ⇒ 无收件人」这条分支不必起真库即可锁住。
#[must_use]
pub fn creator_kind(row: &AutopilotRow) -> Option<CreatorKind> {
    CreatorKind::parse(&row.created_by_type)
}

/// 上游 `ResolveAutopilotNotificationRecipient`：解析该 autopilot 的通知收件人。
///
/// 返回 `Ok(None)` = **没有收件人**（不是错误，见模块文档）；`Err` 只用于真正的 DB 故障，
/// 因为上游在两处 DB 错误上是 `fmt.Errorf(...)` 上抛、在「查不到」上是 `ok=false` —— 这条
/// 区分必须保住，否则 M5-4 会把「工作区里没人可通知」当成故障重试。
pub async fn resolve_recipient(
    autopilots: &AutopilotRepo,
    agents: &AgentRepo,
    autopilot: &AutopilotRow,
) -> Result<Option<AutopilotNotificationRecipient>, AutopilotError> {
    match resolve_with_reason(autopilots, agents, autopilot).await? {
        Ok(recipient) => Ok(Some(recipient)),
        Err(_reason) => Ok(None),
    }
}

/// 与 [`resolve_recipient`] 同语义，但把「为什么没有收件人」一并返回（M5-4 的日志用）。
///
/// 返回类型是 `Result<Result<Recipient, NoRecipientReason>, AutopilotError>`：**外层**是 DB 故障，
/// **内层**是业务上的「无人可发」。故意不用两个 `Option`，免得调用方分不清两者。
pub async fn resolve_with_reason(
    autopilots: &AutopilotRepo,
    agents: &AgentRepo,
    autopilot: &AutopilotRow,
) -> Result<Result<AutopilotNotificationRecipient, NoRecipientReason>, AutopilotError> {
    let workspace = autopilot.workspace();
    let Some(kind) = creator_kind(autopilot) else {
        return Ok(Err(NoRecipientReason::UnsupportedCreatorType));
    };

    // `member` 创建者：本人，但必须仍是本工作区成员（上游 `GetMemberByUserAndWorkspace`）。
    if kind == CreatorKind::Member {
        let creator = autopilot.created_by_id;
        return if member_exists(autopilots, workspace, creator).await? {
            Ok(Ok(AutopilotNotificationRecipient::member(creator)))
        } else {
            Ok(Err(NoRecipientReason::CreatorNotAMember))
        };
    }

    // `agent` 创建者：先定位 agent（必须在本工作区），再取其 owner，再验 owner 的成员身份。
    let agent = match agents
        .find_in_workspace(workspace, mc_core::Id::from(autopilot.created_by_id))
        .await
    {
        Ok(Some(agent)) => agent,
        Ok(None) => return Ok(Err(NoRecipientReason::CreatorAgentUnusable)),
        Err(mc_repos::RepoError::NotFound) => {
            return Ok(Err(NoRecipientReason::CreatorAgentUnusable))
        }
        Err(err) => return Err(AutopilotError::from(err)),
    };
    let Some(owner) = agent.owner_id() else {
        return Ok(Err(NoRecipientReason::AgentHasNoOwner));
    };
    if member_exists(autopilots, workspace, owner.0).await? {
        Ok(Ok(AutopilotNotificationRecipient::member(owner.0)))
    } else {
        Ok(Err(NoRecipientReason::AgentOwnerNotAMember))
    }
}

/// 成员存在性：复用 [`AutopilotRepo::member_role`]（`None` = 非成员）。
///
/// 注意**不要**在这里改成「按 role 过滤」：上游 `GetMemberByUserAndWorkspace` 不筛 role，
/// 只有 `ListWorkspaceManagerNotificationRecipients`（本片不落）才筛 owners/admins。
async fn member_exists(
    autopilots: &AutopilotRepo,
    workspace: mc_core::Id,
    user: Uuid,
) -> Result<bool, AutopilotError> {
    Ok(autopilots
        .member_role(workspace, mc_core::Id::from(user))
        .await?
        .is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creator_kind_only_accepts_member_and_agent() {
        assert_eq!(CreatorKind::parse("member"), Some(CreatorKind::Member));
        assert_eq!(CreatorKind::parse("agent"), Some(CreatorKind::Agent));
        // 上游对 `system` 走的是 `if autopilot.CreatedByType != "agent" { ok=false }`，
        // 也就是**静默无收件人**，而不是「当作 member」。
        assert_eq!(CreatorKind::parse("system"), None);
        assert_eq!(CreatorKind::parse(""), None);
        assert_eq!(CreatorKind::parse("Member"), None);
    }

    #[test]
    fn recipient_is_always_a_member() {
        let r = AutopilotNotificationRecipient::member(Uuid::nil());
        assert_eq!(r.user_type, "member");
        assert_eq!(r.user_type, mc_repos::autopilot::USER_TYPE_MEMBER);
    }
}
