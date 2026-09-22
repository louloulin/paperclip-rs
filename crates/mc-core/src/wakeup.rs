//! Wakeup 领域类型（事件唤醒 + 时间唤醒）。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// Wakeup 源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeupSource {
    Event,
    Time,
    Manual,
}

impl WakeupSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Time => "time",
            Self::Manual => "manual",
        }
    }
}

/// Wakeup 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wakeup {
    pub id: Id,
    pub workspace_id: Id,
    pub issue_id: Id,
    pub source: WakeupSource,
    pub event_type: Option<String>,
    pub due_at: Option<Timestamp>,
    pub actor_type: Option<String>,
    pub actor_id: Option<String>,
    pub status: String, // pending / active / settled / cancelled
    pub run_id: Option<Id>,
    pub receipt_id: Option<String>,
    pub coalesced_count: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Wakeup event capture（事件捕获快照）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeupEventCapture {
    pub id: Id,
    pub wakeup_id: Id,
    pub event_kind: String,
    pub actor_type: Option<String>,
    pub actor_id: Option<String>,
    pub payload: serde_json::Value,
    pub captured_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_str_round_trip() {
        for s in [WakeupSource::Event, WakeupSource::Time, WakeupSource::Manual] {
            assert!(!s.as_str().is_empty());
        }
    }
}