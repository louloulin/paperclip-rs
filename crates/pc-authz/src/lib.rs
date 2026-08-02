//! pc-authz：Policy trait + 基础策略实现。

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use pc_core::Actor;

#[derive(Debug, Error, Serialize)]
pub enum AuthzError {
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not authenticated")]
    Unauthenticated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Read,
    Create,
    Update,
    Delete,
    Decide,
    Trigger,
}

#[derive(Debug, Clone)]
pub struct Company {
    pub id: Uuid,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub id: Uuid,
    pub company_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct Issue {
    pub id: Uuid,
    pub company_id: Uuid,
    pub assignee_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub responsible_user_id: Option<String>,
}

/// 默认策略：
/// - System 与 user 拥有 company/issue 操作权限（多公司隔离由 future 实现）
/// - Agent 仅能操作自己被 assign 的 issue
pub struct DefaultPolicy;

impl DefaultPolicy {
    pub fn check_company(&self, actor: &Actor, _company_id: Uuid) -> Result<(), AuthzError> {
        match actor {
            Actor::System | Actor::User { .. } => Ok(()),
            Actor::Agent { .. } => Err(AuthzError::Forbidden(
                "agent cannot access company-level resources".into(),
            )),
        }
    }

    pub fn check_issue(&self, actor: &Actor, issue: &Issue) -> Result<(), AuthzError> {
        match actor {
            Actor::System | Actor::User { .. } => Ok(()),
            Actor::Agent { id } => {
                if Some(*id) == issue.assignee_agent_id {
                    Ok(())
                } else {
                    Err(AuthzError::Forbidden(
                        "agent not assigned to this issue".into(),
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_core::Actor;

    #[test]
    fn system_passes_company() {
        assert!(DefaultPolicy
            .check_company(&Actor::system(), Uuid::new_v4())
            .is_ok());
    }
    #[test]
    fn user_passes_company() {
        assert!(DefaultPolicy
            .check_company(&Actor::User { id: "u1".into() }, Uuid::new_v4())
            .is_ok());
    }
    #[test]
    fn agent_blocked_company() {
        assert!(DefaultPolicy
            .check_company(&Actor::Agent { id: Uuid::new_v4() }, Uuid::new_v4())
            .is_err());
    }
    #[test]
    fn assignee_agent_passes_issue() {
        let agent = Uuid::new_v4();
        let issue = Issue {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            assignee_agent_id: Some(agent),
            created_by_user_id: None,
            responsible_user_id: None,
        };
        assert!(DefaultPolicy
            .check_issue(&Actor::Agent { id: agent }, &issue)
            .is_ok());
    }
    #[test]
    fn other_agent_blocked_issue() {
        let issue = Issue {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            assignee_agent_id: Some(Uuid::new_v4()),
            created_by_user_id: None,
            responsible_user_id: None,
        };
        assert!(DefaultPolicy
            .check_issue(&Actor::Agent { id: Uuid::new_v4() }, &issue)
            .is_err());
    }
}
