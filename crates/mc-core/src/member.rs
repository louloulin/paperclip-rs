//! Workspace member 领域类型。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;
use super::workspace::WorkspaceRole;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMember {
    pub id: Id,
    pub workspace_id: Id,
    pub user_id: Id,
    pub role: WorkspaceRole,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_constructs() {
        let m = WorkspaceMember {
            id: Id::new(),
            workspace_id: Id::new(),
            user_id: Id::new(),
            role: WorkspaceRole::Member,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        };
        assert_eq!(m.role, WorkspaceRole::Member);
    }
}