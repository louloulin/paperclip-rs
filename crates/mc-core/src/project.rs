//! Project 领域类型。

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// Project 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Id,
    pub workspace_id: Id,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub priority: String,
    pub start_date: Option<NaiveDate>,
    pub target_date: Option<NaiveDate>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_constructs() {
        let p = Project {
            id: Id::new(),
            workspace_id: Id::new(),
            name: "X".into(),
            slug: "x".into(),
            description: None,
            icon: None,
            priority: "none".into(),
            start_date: None,
            target_date: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            archived_at: None,
        };
        assert!(p.archived_at.is_none());
    }
}
