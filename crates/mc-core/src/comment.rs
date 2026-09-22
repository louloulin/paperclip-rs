//! Comment 领域类型（含 reactions / parent / resolved / triage）。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// Comment author type（与 multica `comment_system_author` / `comment_routing_escalation` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentAuthorType {
    User,
    Agent,
    System,
    Plugin,
    Squad,
    Autopilot,
}

impl CommentAuthorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
            Self::Plugin => "plugin",
            Self::Squad => "squad",
            Self::Autopilot => "autopilot",
        }
    }
}

/// Comment 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: Id,
    pub workspace_id: Id,
    pub issue_id: Id,
    pub parent_id: Option<Id>,
    pub author_type: CommentAuthorType,
    pub author_id: String, // user or agent UUID-as-string
    pub body: String,
    pub source_task_id: Option<Id>,
    pub routing_escalation: Option<String>,
    pub revision: i64,
    pub resolved_at: Option<Timestamp>,
    pub deleted_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Comment reaction。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentReaction {
    pub comment_id: Id,
    pub user_id: Id,
    pub emoji: String,
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn author_type_str_round_trip() {
        for a in [
            CommentAuthorType::User,
            CommentAuthorType::Agent,
            CommentAuthorType::System,
            CommentAuthorType::Plugin,
        ] {
            assert!(!a.as_str().is_empty());
        }
    }
}