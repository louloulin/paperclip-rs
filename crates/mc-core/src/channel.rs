//! Channel 领域类型：6 个内置 + 自定义。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// Channel 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Slack,
    Lark,
    DingTalk,
    WeCom,
    Telegram,
    Custom,
}

impl ChannelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Lark => "lark",
            Self::DingTalk => "dingtalk",
            Self::WeCom => "wecom",
            Self::Telegram => "telegram",
            Self::Custom => "custom",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "slack" => Some(Self::Slack),
            "lark" => Some(Self::Lark),
            "dingtalk" => Some(Self::DingTalk),
            "wecom" => Some(Self::WeCom),
            "telegram" => Some(Self::Telegram),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    pub fn all() -> [ChannelKind; 6] {
        [Self::Slack, Self::Lark, Self::DingTalk, Self::WeCom, Self::Telegram, Self::Custom]
    }
}

/// Channel 主体（installation）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInstallation {
    pub id: Id,
    pub workspace_id: Id,
    pub kind: ChannelKind,
    pub external_id: String,
    pub display_name: String,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_round_trip() {
        for k in ChannelKind::all() {
            assert_eq!(ChannelKind::from_str_opt(k.as_str()), Some(k));
        }
    }

    #[test]
    fn unknown_kind_returns_none() {
        assert!(ChannelKind::from_str_opt("discord").is_none());
    }
}