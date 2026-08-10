//! Types —— LiveEvent DTOs。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ============================================================================
// Constants
// ============================================================================

/// 全局 channel 的虚拟 company id（与 Node `"*"` 1:1 对齐）。
pub const GLOBAL_COMPANY_ID: &str = "*";

// ============================================================================
// LiveEventType
// ============================================================================

/// Event 类型（自由字符串，与 Node `LiveEventType` = `(typeof LIVE_EVENT_TYPES)[number]` 对齐）。
///
/// 这里用 newtype 包装 `String`，允许任意字符串值（运行时校验放上层）。
/// 编译期 enum 约束会与 Node 端 zod enum 不完全等价，故采用 stringly-typed 包装。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LiveEventType(pub String);

impl LiveEventType {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for LiveEventType {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for LiveEventType {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// ============================================================================
// LiveEventPayload
// ============================================================================

/// Event payload（与 Node `LiveEventPayload = Record<string, unknown>` 1:1 对齐）。
pub type LiveEventPayload = Map<String, Value>;

// ============================================================================
// LiveEvent
// ============================================================================

/// Live event（与 Node `LiveEvent` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveEvent {
    pub id: i64,
    pub company_id: String,
    #[serde(rename = "type")]
    pub event_type: LiveEventType,
    pub created_at: String,
    #[serde(default)]
    pub payload: LiveEventPayload,
}
