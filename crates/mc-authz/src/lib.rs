//! Multica 授权模型：资源 × 动作 × 主体。
//!
//! 与 paperclip-rs `pc-authz` 风格一致，但资源集合是 multica 自己的。

use serde::{Deserialize, Serialize};

use mc_core::workspace::WorkspaceRole;
use mc_core::Id;

/// Multica 资源类型（粗粒度）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    Workspace,
    Member,
    Invitation,
    Issue,
    Comment,
    Project,
    Agent,
    Runtime,
    Skill,
    Autopilot,
    Squad,
    Chat,
    Inbox,
    Channel,
    Plugin,
    Webhook,
    SourceContext,
    Attachment,
    Notification,
    Seat,
    ShareLink,
}

/// 动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Read,
    Write,
    Delete,
    Archive,
    Admin,
    Invite,
    Assign,
    Comment,
    Trigger,
    Dispatch,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Delete => "delete",
            Self::Archive => "archive",
            Self::Admin => "admin",
            Self::Invite => "invite",
            Self::Assign => "assign",
            Self::Comment => "comment",
            Self::Trigger => "trigger",
            Self::Dispatch => "dispatch",
        }
    }
}

/// 主体身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Principal {
    Anonymous,
    User {
        id: Id,
        role: WorkspaceRole,
    },
    Agent {
        agent_id: Id,
        workspace_id: Id,
    },
    System,
    Plugin {
        plugin_id: Id,
        workspace_id: Option<Id>,
    },
    Channel {
        installation_id: Id,
    },
}

impl Principal {
    pub fn role(&self) -> WorkspaceRole {
        match self {
            Principal::User { role, .. } => *role,
            Principal::Agent { .. } | Principal::System | Principal::Plugin { .. } => {
                WorkspaceRole::Member
            }
            Principal::Channel { .. } | Principal::Anonymous => WorkspaceRole::Guest,
        }
    }
}

/// 授权请求。
#[derive(Debug, Clone)]
pub struct AuthorizationRequest {
    pub principal: Principal,
    pub resource: Resource,
    pub action: Action,
    pub resource_owner_id: Option<Id>,
    pub workspace_id: Option<Id>,
}

impl AuthorizationRequest {
    pub fn new(principal: Principal, resource: Resource, action: Action) -> Self {
        Self {
            principal,
            resource,
            action,
            resource_owner_id: None,
            workspace_id: None,
        }
    }

    #[must_use]
    pub fn with_workspace(mut self, workspace_id: Id) -> Self {
        self.workspace_id = Some(workspace_id);
        self
    }

    #[must_use]
    pub fn with_owner(mut self, owner_id: Id) -> Self {
        self.resource_owner_id = Some(owner_id);
        self
    }
}

/// 授权决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum AuthzError {
    #[error("denied: principal {principal:?} cannot {action:?} {resource:?}")]
    Denied {
        principal: String,
        resource: String,
        action: String,
    },
}

/// 授权策略：基于 workspace 角色 + 资源类型 + 动作三元组判断。
///
/// 规则（与 multica 现有权限模型对齐）：
/// - Owner / Admin：所有动作允许
/// - Member：read / write / comment / assign 允许
/// - Guest：仅 read
/// - Anonymous：仅 public read
/// - Agent / System：dispatch / read 自家资源
/// - Channel：仅 comment / message 投递
pub fn decide(req: &AuthorizationRequest) -> Decision {
    use Action::{Admin, Archive, Assign, Comment, Delete, Dispatch, Invite, Read, Trigger, Write};
    use Principal::{Agent, Anonymous, Channel, Plugin, System, User};
    // 注意：不要 `use Resource::*;` —— Resource 与 Action / Principal 存在同名 variant
    //（Comment / Agent / Channel / Plugin），glob 导入会导致 E0659 歧义与 E0408 绑定错误。
    // 下文所有 Resource variant 均使用 `Resource::` 全路径。

    // 系统始终允许内部操作
    if matches!(req.principal, System) {
        return Decision::Allow;
    }

    match req.principal {
        // health / openapi / auth 等公开端点由 routing 层处理；业务资源一律 deny。
        Anonymous => Decision::Deny,
        User { role, .. } => match role {
            WorkspaceRole::Owner | WorkspaceRole::Admin => Decision::Allow,
            WorkspaceRole::Member => match (req.resource, req.action) {
                // 先列「资源 + 动作」的特例 deny（Seat / Webhook 全动作 deny 必须在
                // `(_, Read)` 之前，否则会被下面的通配 allow 抢先匹配）。
                (
                    Resource::Workspace
                    | Resource::Invitation
                    | Resource::Channel
                    | Resource::Plugin
                    | Resource::Squad,
                    Admin,
                )
                | (Resource::Member, Delete | Admin)
                | (Resource::Seat | Resource::Webhook, _) => Decision::Deny,
                (Resource::Autopilot, Trigger | Dispatch)
                | (_, Read | Comment | Assign | Write) => Decision::Allow,
                (_, Delete | Archive | Invite | Admin | Trigger | Dispatch) => Decision::Deny,
            },
            WorkspaceRole::Guest => match (req.resource, req.action) {
                (_, Read) => Decision::Allow,
                _ => Decision::Deny,
            },
        },
        Agent {
            agent_id,
            workspace_id,
        } => {
            // agent 仅能操作自己的 issue / task
            if req.workspace_id == Some(workspace_id) {
                match (req.resource, req.action) {
                    (Resource::Issue | Resource::Chat, Read)
                    | (Resource::Comment, Read | Comment) => Decision::Allow,
                    (Resource::Comment, Write) => {
                        // 仅当 agent 是 comment author 时允许
                        if req.resource_owner_id == Some(agent_id) {
                            Decision::Allow
                        } else {
                            Decision::Deny
                        }
                    }
                    _ => Decision::Deny,
                }
            } else {
                Decision::Deny
            }
        }
        Plugin {
            workspace_id: pid, ..
        } => {
            if pid.is_none() || pid == req.workspace_id {
                match (req.resource, req.action) {
                    (Resource::Comment, Comment | Write | Read)
                    | (Resource::Webhook | Resource::SourceContext, Read) => Decision::Allow,
                    _ => Decision::Deny,
                }
            } else {
                Decision::Deny
            }
        }
        Channel { .. } => match (req.resource, req.action) {
            (Resource::Comment, Comment | Write) => Decision::Allow,
            _ => Decision::Deny,
        },
        // 上方 `if matches!(req.principal, System)` 已提前返回；此处补齐穷尽性。
        System => Decision::Allow,
    }
}

/// Authorize helper：返回 `Result<(), AuthzError>`。
pub fn authorize(req: &AuthorizationRequest) -> Result<(), AuthzError> {
    match decide(req) {
        Decision::Allow => Ok(()),
        Decision::Deny => Err(AuthzError::Denied {
            principal: format!("{:?}", req.principal),
            resource: format!("{:?}", req.resource),
            action: format!("{:?}", req.action),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_can_do_anything() {
        let req = AuthorizationRequest::new(
            Principal::User {
                id: Id::new(),
                role: WorkspaceRole::Owner,
            },
            Resource::Workspace,
            Action::Admin,
        );
        assert_eq!(decide(&req), Decision::Allow);
    }

    #[test]
    fn member_cannot_admin_workspace() {
        let req = AuthorizationRequest::new(
            Principal::User {
                id: Id::new(),
                role: WorkspaceRole::Member,
            },
            Resource::Workspace,
            Action::Admin,
        );
        assert_eq!(decide(&req), Decision::Deny);
    }

    #[test]
    fn member_can_read_and_comment() {
        let user = Principal::User {
            id: Id::new(),
            role: WorkspaceRole::Member,
        };
        let read = AuthorizationRequest::new(user, Resource::Issue, Action::Read);
        let comment = AuthorizationRequest::new(user, Resource::Comment, Action::Comment);
        assert_eq!(decide(&read), Decision::Allow);
        assert_eq!(decide(&comment), Decision::Allow);
    }

    #[test]
    fn member_cannot_delete_member() {
        let user = Principal::User {
            id: Id::new(),
            role: WorkspaceRole::Member,
        };
        let req = AuthorizationRequest::new(user, Resource::Member, Action::Delete);
        assert_eq!(decide(&req), Decision::Deny);
    }

    #[test]
    fn anonymous_is_denied_business_resources() {
        let req = AuthorizationRequest::new(Principal::Anonymous, Resource::Issue, Action::Read);
        assert_eq!(decide(&req), Decision::Deny);
    }

    #[test]
    fn agent_can_read_own_workspace_issues() {
        let agent_id = Id::new();
        let workspace_id = Id::new();
        let req = AuthorizationRequest::new(
            Principal::Agent {
                agent_id,
                workspace_id,
            },
            Resource::Issue,
            Action::Read,
        )
        .with_workspace(workspace_id);
        assert_eq!(decide(&req), Decision::Allow);
    }

    #[test]
    fn agent_cannot_comment_outside_workspace() {
        let req = AuthorizationRequest::new(
            Principal::Agent {
                agent_id: Id::new(),
                workspace_id: Id::new(),
            },
            Resource::Issue,
            Action::Read,
        )
        .with_workspace(Id::new()); // different workspace
        assert_eq!(decide(&req), Decision::Deny);
    }

    #[test]
    fn system_can_do_anything() {
        let req = AuthorizationRequest::new(Principal::System, Resource::Seat, Action::Admin);
        assert_eq!(decide(&req), Decision::Allow);
    }

    #[test]
    fn channel_can_write_comment() {
        let req = AuthorizationRequest::new(
            Principal::Channel {
                installation_id: Id::new(),
            },
            Resource::Comment,
            Action::Write,
        );
        assert_eq!(decide(&req), Decision::Allow);
    }

    #[test]
    fn channel_cannot_admin() {
        let req = AuthorizationRequest::new(
            Principal::Channel {
                installation_id: Id::new(),
            },
            Resource::Channel,
            Action::Admin,
        );
        assert_eq!(decide(&req), Decision::Deny);
    }

    #[test]
    fn guest_can_read_only() {
        let user = Principal::User {
            id: Id::new(),
            role: WorkspaceRole::Guest,
        };
        assert_eq!(
            decide(&AuthorizationRequest::new(
                user,
                Resource::Issue,
                Action::Read
            )),
            Decision::Allow
        );
        assert_eq!(
            decide(&AuthorizationRequest::new(
                user,
                Resource::Issue,
                Action::Write
            )),
            Decision::Deny
        );
    }

    #[test]
    fn member_can_trigger_autopilot() {
        let user = Principal::User {
            id: Id::new(),
            role: WorkspaceRole::Member,
        };
        let req = AuthorizationRequest::new(user, Resource::Autopilot, Action::Trigger);
        assert_eq!(decide(&req), Decision::Allow);
    }

    #[test]
    fn member_cannot_admin_seat() {
        let user = Principal::User {
            id: Id::new(),
            role: WorkspaceRole::Member,
        };
        let req = AuthorizationRequest::new(user, Resource::Seat, Action::Admin);
        assert_eq!(decide(&req), Decision::Deny);
    }

    #[test]
    fn authorize_returns_err_on_deny() {
        let req =
            AuthorizationRequest::new(Principal::Anonymous, Resource::Workspace, Action::Admin);
        assert!(authorize(&req).is_err());
    }
}
