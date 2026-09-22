//! Squad 领域类型（leader 路由）。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// Squad 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Squad {
    pub id: Id,
    pub workspace_id: Id,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub instructions: Option<String>,
    pub leader_agent_id: Option<Id>,
    pub archived_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squad_constructs() {
        let s = Squad {
            id: Id::new(),
            workspace_id: Id::new(),
            name: "core".into(),
            slug: "core".into(),
            description: None,
            avatar_url: None,
            instructions: None,
            leader_agent_id: None,
            archived_at: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        };
        assert_eq!(s.slug, "core");
    }
}
