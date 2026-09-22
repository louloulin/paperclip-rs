//! Chat 领域类型。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// Chat session 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: Id,
    pub workspace_id: Id,
    pub user_id: Id,
    pub agent_id: Option<Id>,
    pub runtime_id: Option<Id>,
    pub project_id: Option<Id>,
    pub title: Option<String>,
    pub pinned: bool,
    pub unread_since: Option<Timestamp>,
    pub read_cursor: Option<String>,
    pub pinned_agent: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Chat message 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: Id,
    pub session_id: Id,
    pub workspace_id: Id,
    pub role: String, // user / assistant / system / tool
    pub kind: String, // text / tool_call / tool_result / image / file / card
    pub content: String,
    pub task_id: Option<Id>,
    pub tool_call_id: Option<String>,
    pub input_owner: Option<String>,
    pub elapsed_ms: Option<u32>,
    pub truncated: bool,
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_session_constructs() {
        let s = ChatSession {
            id: Id::new(),
            workspace_id: Id::new(),
            user_id: Id::new(),
            agent_id: None,
            runtime_id: None,
            project_id: None,
            title: None,
            pinned: false,
            unread_since: None,
            read_cursor: None,
            pinned_agent: false,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        };
        assert!(!s.pinned);
    }
}