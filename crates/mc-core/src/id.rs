//! ID 类型。
//!
//! Multica 数据库主键：UUID v4（v7 也允许）。
//! 协议层：UUID-as-string。

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum IdError {
    #[error("invalid UUID: {0}")]
    InvalidUuid(String),
}

/// Multica 通用 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(pub Uuid);

impl Id {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn nil() -> Self {
        Self(Uuid::nil())
    }

    pub fn parse(s: &str) -> Result<Self, IdError> {
        Uuid::parse_str(s)
            .map(Self)
            .map_err(|e| IdError::InvalidUuid(e.to_string()))
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }

    pub fn as_string(self) -> String {
        self.0.to_string()
    }

    pub fn is_nil(self) -> bool {
        self.0.is_nil()
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for Id {
    fn from(u: Uuid) -> Self {
        Self(u)
    }
}

impl From<Id> for Uuid {
    fn from(id: Id) -> Self {
        id.0
    }
}

impl std::str::FromStr for Id {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// `PrefixedId`：在 API 层用 `issue_<uuid>` / `agent_<uuid>` 等前缀做协议标识，
/// 类似 paperclip-rs 的 `ExternalId`。基础形式仍存为 UUID。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrefixedId(String);

impl PrefixedId {
    pub fn new(prefix: &str, id: Id) -> Self {
        Self(format!("{prefix}_{id}"))
    }

    pub fn parse(s: &str) -> Result<(String, Id), IdError> {
        let (prefix, raw) = s
            .split_once('_')
            .ok_or_else(|| IdError::InvalidUuid(format!("missing prefix separator: {s}")))?;
        let id = Id::parse(raw)?;
        Ok((prefix.to_string(), id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PrefixedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let id = Id::new();
        let s = id.as_string();
        let parsed = Id::parse(&s).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn prefix_round_trip() {
        let id = Id::new();
        let prefixed = PrefixedId::new("issue", id);
        let s = prefixed.to_string();
        let (prefix, parsed) = PrefixedId::parse(&s).unwrap();
        assert_eq!(prefix, "issue");
        assert_eq!(parsed, id);
    }

    #[test]
    fn parse_rejects_invalid() {
        assert!(Id::parse("not-a-uuid").is_err());
        assert!(PrefixedId::parse("no_separator").is_err());
    }
}
