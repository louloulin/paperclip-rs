#![forbid(unsafe_code)]

//! Issue attribution derivation.
//!
//! R532: Direct port of `paperclip/packages/shared/src/issue-attribution.ts`.
//!
//! 设计原则:
//! - 所有 pub fn 都是纯函数 (无 IO, 无副作用)
//! - 接受 `Pick`-style 引用, 不依赖完整的 `Issue` row
//! - 派生结果用 serde 序列化, 方便 API response 直接 emit
//!
//! 范围 (本 crate):
//! - [`derive_responsible_user`] — 决定 issue 真正的"负责人": 显式 > 创建者 > none
//! - [`derive_originating_actor`] — 决定 issue "源自" 谁: 人类创建者 > 间接人类 > agent > none
//!
//! **不** 范围 (留给集成层):
//! - `pc-repos::issue` 持久化层 (responsibleUserId / createdByUserId / createdByAgentId 字段)
//! - server `routes/issues.ts` 等 endpoint 调用
//! - UI 端的"Originating: User via Agent"渲染
//!
//! Node 上游该模块在 `packages/db/src/migration-safety-baseline.ts` 等多处用;
//! Rust port 让 pc-repos / pc-issue 业务层直接调用 derive fn, 不重复实现.

use serde::{Deserialize, Serialize};

/// Source attribution for a derived responsible user.
///
/// Mirrors Node upstream `ResponsibleUserSource` type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponsibleUserSource {
    /// Explicit `responsibleUserId` on the issue.
    Explicit,
    /// Auto-derived from `createdByUserId` (issue had no explicit responsible user).
    Creator,
    /// No human is attributable.
    None,
}

/// Attribution result for the responsible-user derivation.
///
/// Mirrors Node upstream `ResponsibleUserAttribution` interface.
/// Serialized as `{"userId":"...","source":"explicit","isAutoDerived":false}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponsibleUserAttribution {
    pub user_id: Option<String>,
    pub source: ResponsibleUserSource,
    pub is_auto_derived: bool,
}

/// Minimal issue shape required by [`derive_responsible_user`].
///
/// Mirrors Node upstream `Pick<Issue, "responsibleUserId" | "createdByUserId">`.
#[derive(Debug, Clone, Default)]
pub struct ResponsibleUserInput {
    pub responsible_user_id: Option<String>,
    pub created_by_user_id: Option<String>,
}

impl ResponsibleUserInput {
    #[must_use]
    pub fn new(responsible_user_id: Option<String>, created_by_user_id: Option<String>) -> Self {
        Self {
            responsible_user_id,
            created_by_user_id,
        }
    }
}

/// Derive the responsible user attribution from an issue.
///
/// Logic (mirrors Node upstream `deriveResponsibleUser`):
/// 1. If `responsibleUserId` is non-null → `{ userId, source: "explicit", isAutoDerived: false }`
/// 2. Else if `createdByUserId` is non-null → `{ userId, source: "creator", isAutoDerived: true }`
/// 3. Else → `{ userId: null, source: "none", isAutoDerived: false }`
#[must_use]
pub fn derive_responsible_user(issue: &ResponsibleUserInput) -> ResponsibleUserAttribution {
    if let Some(uid) = issue
        .responsible_user_id
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        return ResponsibleUserAttribution {
            user_id: Some(uid.to_string()),
            source: ResponsibleUserSource::Explicit,
            is_auto_derived: false,
        };
    }

    if let Some(uid) = issue
        .created_by_user_id
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        return ResponsibleUserAttribution {
            user_id: Some(uid.to_string()),
            source: ResponsibleUserSource::Creator,
            is_auto_derived: true,
        };
    }

    ResponsibleUserAttribution {
        user_id: None,
        source: ResponsibleUserSource::None,
        is_auto_derived: false,
    }
}

/// Originating actor for an issue (used by UI "Originating" affordance).
///
/// Mirrors Node upstream `OriginatingActor` discriminated union.
/// Serialized as `{"kind":"user","id":"...","viaAgentId":"..."}`
/// (camelCase field names to match Node's TypeScript convention).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OriginatingActor {
    /// Human creator. May carry `viaAgentId` when attribution flows through an agent.
    #[serde(rename_all = "camelCase")]
    User {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        via_agent_id: Option<String>,
    },
    /// Agent creator with no human responsible user.
    #[serde(rename_all = "camelCase")]
    Agent { id: String },
}

impl OriginatingActor {
    /// The canonical id of the originating actor (user id or agent id).
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            OriginatingActor::User { id, .. } | OriginatingActor::Agent { id } => id,
        }
    }

    /// True if this is a human user (not an agent).
    #[must_use]
    pub fn is_user(&self) -> bool {
        matches!(self, OriginatingActor::User { .. })
    }
}

/// Minimal issue shape required by [`derive_originating_actor`].
///
/// Mirrors Node upstream `Pick<Issue, "createdByUserId" | "createdByAgentId" | "responsibleUserId">`.
#[derive(Debug, Clone, Default)]
pub struct OriginatingActorInput {
    pub created_by_user_id: Option<String>,
    pub created_by_agent_id: Option<String>,
    pub responsible_user_id: Option<String>,
}

impl OriginatingActorInput {
    #[must_use]
    pub fn new(
        created_by_user_id: Option<String>,
        created_by_agent_id: Option<String>,
        responsible_user_id: Option<String>,
    ) -> Self {
        Self {
            created_by_user_id,
            created_by_agent_id,
            responsible_user_id,
        }
    }
}

/// Derive the originating actor for an issue.
///
/// Logic (mirrors Node upstream `deriveOriginatingActor`):
/// 1. If `createdByUserId` is non-null → `User { id: createdByUserId }` (human always wins)
/// 2. Else if `createdByAgentId` is non-null:
///    - If `responsibleUserId` non-null → `User { id: responsibleUserId, viaAgentId: createdByAgentId }`
///    - Else → `Agent { id: createdByAgentId }`
/// 3. Else if `responsibleUserId` non-null → `User { id: responsibleUserId }` (routine execution)
/// 4. Else → `None`
#[must_use]
pub fn derive_originating_actor(issue: &OriginatingActorInput) -> Option<OriginatingActor> {
    if let Some(uid) = non_empty_str(issue.created_by_user_id.as_deref()) {
        return Some(OriginatingActor::User {
            id: uid.to_string(),
            via_agent_id: None,
        });
    }

    if let Some(aid) = non_empty_str(issue.created_by_agent_id.as_deref()) {
        if let Some(uid) = non_empty_str(issue.responsible_user_id.as_deref()) {
            return Some(OriginatingActor::User {
                id: uid.to_string(),
                via_agent_id: Some(aid.to_string()),
            });
        }
        return Some(OriginatingActor::Agent {
            id: aid.to_string(),
        });
    }

    if let Some(uid) = non_empty_str(issue.responsible_user_id.as_deref()) {
        return Some(OriginatingActor::User {
            id: uid.to_string(),
            via_agent_id: None,
        });
    }

    None
}

/// Helper: treat None and empty string as missing. Returns Some(non-empty) or None.
fn non_empty_str(s: Option<&str>) -> Option<&str> {
    s.filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- derive_responsible_user -----

    #[test]
    fn r532_responsible_prefers_explicit() {
        let issue =
            ResponsibleUserInput::new(Some("user-responsible".into()), Some("user-creator".into()));
        assert_eq!(
            derive_responsible_user(&issue),
            ResponsibleUserAttribution {
                user_id: Some("user-responsible".into()),
                source: ResponsibleUserSource::Explicit,
                is_auto_derived: false,
            }
        );
    }

    #[test]
    fn r532_responsible_falls_back_to_creator() {
        let issue = ResponsibleUserInput::new(None, Some("user-creator".into()));
        assert_eq!(
            derive_responsible_user(&issue),
            ResponsibleUserAttribution {
                user_id: Some("user-creator".into()),
                source: ResponsibleUserSource::Creator,
                is_auto_derived: true,
            }
        );
    }

    #[test]
    fn r532_responsible_returns_none_when_nothing_available() {
        let issue = ResponsibleUserInput::new(None, None);
        assert_eq!(
            derive_responsible_user(&issue),
            ResponsibleUserAttribution {
                user_id: None,
                source: ResponsibleUserSource::None,
                is_auto_derived: false,
            }
        );
    }

    #[test]
    fn r532_responsible_treats_empty_string_as_none() {
        // Node upstream: `if (issue.responsibleUserId)` — empty string is falsy.
        let issue = ResponsibleUserInput::new(Some(String::new()), Some("user-creator".into()));
        assert_eq!(
            derive_responsible_user(&issue).source,
            ResponsibleUserSource::Creator
        );
    }

    #[test]
    fn r532_responsible_explicit_empty_with_no_creator_returns_none() {
        let issue = ResponsibleUserInput::new(Some(String::new()), None);
        assert_eq!(
            derive_responsible_user(&issue).source,
            ResponsibleUserSource::None
        );
    }

    // ----- derive_originating_actor -----

    #[test]
    fn r532_originating_prefers_human_creator() {
        let issue = OriginatingActorInput::new(
            Some("user-creator".into()),
            None,
            Some("user-responsible".into()),
        );
        assert_eq!(
            derive_originating_actor(&issue),
            Some(OriginatingActor::User {
                id: "user-creator".into(),
                via_agent_id: None,
            })
        );
    }

    #[test]
    fn r532_originating_agent_creator_with_responsible_user() {
        let issue = OriginatingActorInput::new(
            None,
            Some("agent-claude".into()),
            Some("user-responsible".into()),
        );
        assert_eq!(
            derive_originating_actor(&issue),
            Some(OriginatingActor::User {
                id: "user-responsible".into(),
                via_agent_id: Some("agent-claude".into()),
            })
        );
    }

    #[test]
    fn r532_originating_agent_creator_without_responsible_user() {
        let issue = OriginatingActorInput::new(None, Some("agent-claude".into()), None);
        assert_eq!(
            derive_originating_actor(&issue),
            Some(OriginatingActor::Agent {
                id: "agent-claude".into()
            })
        );
    }

    #[test]
    fn r532_originating_routine_execution_no_creator() {
        let issue = OriginatingActorInput::new(None, None, Some("user-responsible".into()));
        assert_eq!(
            derive_originating_actor(&issue),
            Some(OriginatingActor::User {
                id: "user-responsible".into(),
                via_agent_id: None,
            })
        );
    }

    #[test]
    fn r532_originating_returns_null_when_nothing_attributable() {
        let issue = OriginatingActorInput::new(None, None, None);
        assert_eq!(derive_originating_actor(&issue), None);
    }

    #[test]
    fn r532_originating_human_creator_overrides_agent() {
        // Even with all three populated, human creator wins (priority 1).
        let issue = OriginatingActorInput::new(
            Some("user-creator".into()),
            Some("agent-claude".into()),
            Some("user-responsible".into()),
        );
        let actor = derive_originating_actor(&issue).unwrap();
        assert!(actor.is_user());
        assert_eq!(actor.id(), "user-creator");
    }

    #[test]
    fn r532_originating_treats_empty_string_as_none() {
        let issue = OriginatingActorInput::new(
            Some(String::new()),
            Some("agent-claude".into()),
            Some("user-responsible".into()),
        );
        // Empty createdByUserId treated as missing → falls through to createdByAgentId.
        assert_eq!(
            derive_originating_actor(&issue),
            Some(OriginatingActor::User {
                id: "user-responsible".into(),
                via_agent_id: Some("agent-claude".into()),
            })
        );
    }

    #[test]
    fn r532_originating_empty_strings_all_around() {
        let issue = OriginatingActorInput::new(
            Some(String::new()),
            Some(String::new()),
            Some(String::new()),
        );
        assert_eq!(derive_originating_actor(&issue), None);
    }

    // ----- OriginatingActor helpers -----

    #[test]
    fn r532_originating_actor_id_helper() {
        let user = OriginatingActor::User {
            id: "u1".into(),
            via_agent_id: None,
        };
        let agent = OriginatingActor::Agent { id: "a1".into() };
        assert_eq!(user.id(), "u1");
        assert_eq!(agent.id(), "a1");
    }

    #[test]
    fn r532_originating_actor_is_user_helper() {
        let user = OriginatingActor::User {
            id: "u1".into(),
            via_agent_id: None,
        };
        let agent = OriginatingActor::Agent { id: "a1".into() };
        assert!(user.is_user());
        assert!(!agent.is_user());
    }

    #[test]
    fn r532_originating_actor_serde_roundtrip() {
        // Serde tag = "kind", so JSON looks like `{"kind":"user","id":"u1"}`
        // or `{"kind":"user","id":"u1","viaAgentId":"a1"}`.
        let user_with_via = OriginatingActor::User {
            id: "u1".into(),
            via_agent_id: Some("a1".into()),
        };
        let json = serde_json::to_string(&user_with_via).unwrap();
        assert!(json.contains("\"kind\":\"user\""));
        assert!(json.contains("\"id\":\"u1\""));
        assert!(json.contains("\"viaAgentId\":\"a1\""));
        let parsed: OriginatingActor = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, user_with_via);
    }

    #[test]
    fn r532_originating_actor_serde_omits_none_via_agent() {
        let user = OriginatingActor::User {
            id: "u1".into(),
            via_agent_id: None,
        };
        let json = serde_json::to_string(&user).unwrap();
        assert!(!json.contains("viaAgentId"), "got: {json}");
    }

    #[test]
    fn r532_responsible_source_serde_roundtrip() {
        // snake_case: explicit / creator / none
        assert_eq!(
            serde_json::to_string(&ResponsibleUserSource::Explicit).unwrap(),
            "\"explicit\""
        );
        assert_eq!(
            serde_json::to_string(&ResponsibleUserSource::Creator).unwrap(),
            "\"creator\""
        );
        assert_eq!(
            serde_json::to_string(&ResponsibleUserSource::None).unwrap(),
            "\"none\""
        );
    }
}
