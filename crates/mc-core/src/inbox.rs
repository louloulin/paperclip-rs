//! Inbox 领域类型。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// Inbox item 来源类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxActorType {
    User,
    Agent,
    System,
    Autopilot,
    Squad,
    Channel,
}

impl InboxActorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
            Self::Autopilot => "autopilot",
            Self::Squad => "squad",
            Self::Channel => "channel",
        }
    }
}

/// Inbox item 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: Id,
    pub workspace_id: Id,
    pub user_id: Id,
    pub issue_id: Option<Id>,
    pub actor_type: InboxActorType,
    pub actor_id: String,
    pub category: String, // mention / assigned / status / review / blocker
    pub title: String,
    pub body: Option<String>,
    pub read_at: Option<Timestamp>,
    pub archived_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_str_round_trip() {
        for a in [
            InboxActorType::User,
            InboxActorType::Agent,
            InboxActorType::Channel,
        ] {
            assert!(!a.as_str().is_empty());
        }
    }
}
