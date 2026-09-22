//! 事件 envelope。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use mc_core::Id;
use mc_core::timestamp::Timestamp;

/// 事件类型（业务）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    IssueCreated { id: String },
    IssueUpdated { id: String },
    IssueDeleted { id: String },
    CommentCreated { id: String },
    CommentUpdated { id: String },
    InboxReady { id: String },
    ProjectViewAssigned,
    SkillUpdated,
    ChannelMessage { id: String },
    HeartbeatTick,
    Lagged { skipped: u64 },
    Custom(serde_json::Value),
}

/// 事件 envelope（live-events 协议）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_id: String,
    pub resource: String,
    pub resource_id: String,
    pub actor: Option<Id>,
    pub at: Timestamp,
    pub payload: serde_json::Value,
    pub event_type: String,
}

impl EventEnvelope {
    pub fn new(
        resource: impl Into<String>,
        resource_id: impl Into<String>,
        actor: Option<Id>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            resource: resource.into(),
            resource_id: resource_id.into(),
            actor,
            at: Timestamp::now(),
            payload,
            event_type: "custom".into(),
        }
    }

    pub fn with_type(mut self, event_type: impl Into<String>) -> Self {
        self.event_type = event_type.into();
        self
    }

    pub fn lagged(skipped: u64) -> Self {
        Self::new("system", "lagged", None, serde_json::json!({"skipped": skipped}))
            .with_type("system.lagged")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_serializes_with_event_id() {
        let env = EventEnvelope::new("issue", "issue-1", None, serde_json::json!({}));
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"event_id\""));
        assert!(json.contains("\"resource\":\"issue\""));
    }
}