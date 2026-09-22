//! Issue priority。

use serde::{Deserialize, Serialize};
use std::fmt;

/// 与 multica `validIssuePriorities` 对齐：5 个内置值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Urgent,
    High,
    Medium,
    Low,
    None,
}

impl Default for Priority {
    fn default() -> Self {
        Self::None
    }
}

impl Priority {
    pub const ALL: [Priority; 5] = [
        Priority::Urgent,
        Priority::High,
        Priority::Medium,
        Priority::Low,
        Priority::None,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Urgent => "urgent",
            Priority::High => "high",
            Priority::Medium => "medium",
            Priority::Low => "low",
            Priority::None => "none",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "urgent" => Some(Self::Urgent),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for p in Priority::ALL {
            assert_eq!(Priority::from_str_opt(p.as_str()), Some(p));
        }
    }

    #[test]
    fn default_is_none() {
        assert_eq!(Priority::default(), Priority::None);
    }

    #[test]
    fn unknown_string_returns_none() {
        assert!(Priority::from_str_opt("foo").is_none());
    }
}