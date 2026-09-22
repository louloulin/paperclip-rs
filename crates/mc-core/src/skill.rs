//! Skill 领域类型（structured skill）。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// Skill visibility。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SkillVisibility {
    #[default]
    Workspace,
    Private,
}

/// Skill 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: Id,
    pub workspace_id: Id,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub body: String, // markdown
    pub visibility: SkillVisibility,
    pub enabled: bool,
    pub owner_id: Option<Id>,
    pub plugin_key: Option<String>,
    pub plugin_installation_id: Option<Id>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_visibility_is_workspace() {
        assert_eq!(SkillVisibility::default(), SkillVisibility::Workspace);
    }
}
