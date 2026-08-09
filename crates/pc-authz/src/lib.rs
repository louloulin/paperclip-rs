//! pc-authz：基于规则的授权决策引擎。
//!
//! 与原 `paperclip/server/src/services/authorization.ts` 中 `evaluateAuthorization`
//! 的核心分支对齐：
//!
//! - **Actor**：User / Agent / System / Anonymous（来自 `pc-auth`）
//! - **Action**：21 个 `PermissionKey` + 12 个隐式 action（`issue:read` 等）
//! - **Resource**：company / agent / project / issue
//! - **Decision**：allowed + reason + explanation
//!
//! 该 crate 是**纯函数**：不带 IO，调用方把 memberships、grants、issue 上下文注入到
//! [`policy::Context`]。`evaluate` 返回 [`Decision`]，`check` 返回 `Result<(), AuthzError>`。
//!
//! 用法：
//!
//! ```ignore
//! use pc_authz::{policy, types::{Action, PermissionKey, Resource}};
//! use pc_auth::Actor;
//!
//! let ctx = policy::Context::for_user(memberships, vec![], Some(CompanyRole::Admin), false);
//! let decision = policy::evaluate(&actor, &ctx, &resource, Action::Permission(PermissionKey::JoinsApprove));
//! if !decision.allowed { return Err(ApiError::Forbidden(decision.explanation)); }
//! ```

pub mod builder;
pub mod http;
pub mod mentions;
pub mod policy;
pub mod trust;
pub mod types;

pub use builder::build_context;
pub use http::{company_resource, denial_to_string, enforce, enforce_issue, enforce_permission};
pub use mentions::{
    build_agent_mention_href, build_user_mention_href, extract_agent_mention_ids,
    extract_pipeline_mention_ids, extract_routine_mention_ids, extract_skill_mention_ids,
    extract_user_mention_ids, parse_agent_mention_href, parse_user_mention_href,
    ParsedAgentMention, ParsedUserMention,
    AGENT_MENTION_SCHEME, PIPELINE_MENTION_SCHEME, PROJECT_MENTION_SCHEME,
    ROUTINE_MENTION_SCHEME, SKILL_MENTION_SCHEME, USER_MENTION_SCHEME,
};
pub use policy::{evaluate, principal_type_of, AuthzError, Context};
pub use trust::{
    is_agent_within_boundary, is_issue_within_boundary, is_tool_class_within_boundary,
    resolve_core_trust_preset, DenyReason as TrustDenyReason, LowTrustBoundary,
    ResolveInput as TrustResolveInput, TrustError, TrustPreset, TrustPresetResolution,
    TrustPresetSource, LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH, LOW_TRUST_REVIEW_PRESET,
    LOW_TRUST_REVIEW_PRESET_VERSION, LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION,
};
pub use types::{
    Action, CompanyRole, Decision, PermissionKey, PrincipalType, Reason, Resource,
};

// 兼容旧 API（保留 DefaultPolicy + Action 旧枚举）。
pub use crate::compat::{Action as LegacyAction, AuthzError as LegacyAuthzError, Company, DefaultPolicy, Issue, Project};
pub mod compat {
    //! 向后兼容旧 stub API（迁移期使用，新代码请直接用 [`crate::policy`]）。
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

    /// 默认策略：保留旧 stub 行为（system + user 拥有 company/issue；agent 仅能操作被 assign 的 issue）。
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
}

#[cfg(test)]
mod lib_tests {
    use super::*;

    #[test]
    fn public_api_exports_resolve() {
        // Smoke: ensure key exports are reachable.
        let _ = std::any::type_name::<Action>();
        let _ = std::any::type_name::<Decision>();
        let _ = std::any::type_name::<Context>();
        let _ = std::any::type_name::<PermissionKey>();
        let _ = std::any::type_name::<Resource>();
    }
}
