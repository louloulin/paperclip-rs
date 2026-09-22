//! Workspace 领域类型。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::slug::Slug;
use super::timestamp::Timestamp;

/// Workspace 角色（与 multica 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceRole {
    Owner,
    Admin,
    Member,
    Guest,
}

impl WorkspaceRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
            Self::Guest => "guest",
        }
    }
}

/// Workspace 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Id,
    pub name: String,
    pub slug: Slug,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// 是否归档。归档后所有 mutation 拒绝（参见 `IssueGuard`）。
    pub archived_at: Option<Timestamp>,
    /// 工作空间默认 settings。
    pub settings: serde_json::Value,
}

impl Workspace {
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

/// Workspace 创建请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewWorkspace {
    pub name: String,
    pub slug: Slug,
    pub description: Option<String>,
}

/// Workspace 更新请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub settings: Option<serde_json::Value>,
}

// Need FromStr impl for tests:
impl std::str::FromStr for WorkspaceRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "owner" => Ok(Self::Owner),
            "admin" => Ok(Self::Admin),
            "member" => Ok(Self::Member),
            "guest" => Ok(Self::Guest),
            other => Err(format!("unknown workspace role: {other}")),
        }
    }
}

impl WorkspaceRole {
    /// 宽松解析：未知值返回 `None`（区别于上面严格版的 `FromStr`）。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_str_round_trip() {
        for r in [
            WorkspaceRole::Owner,
            WorkspaceRole::Admin,
            WorkspaceRole::Member,
            WorkspaceRole::Guest,
        ] {
            assert_eq!(WorkspaceRole::from_str(r.as_str()), Some(r));
        }
    }

    #[test]
    fn is_archived() {
        let w = Workspace {
            id: Id::new(),
            name: "x".into(),
            slug: Slug::parse("acme").unwrap(),
            description: None,
            avatar_url: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            archived_at: Some(Timestamp::now()),
            settings: serde_json::json!({}),
        };
        assert!(w.is_archived());
    }
}
