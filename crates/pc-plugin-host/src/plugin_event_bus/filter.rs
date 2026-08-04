//! 服务端过滤器判定（与 Node `passesFilter` 1:1 对齐）。
//!
//! 单一职责：根据 `EventFilter` 决定事件是否通过；多字段 AND 语义。
//!
//! 字段解析策略（与 Node 注释一致）：
//! - `projectId`：`entityType == "project"` 时取 `entityId`，否则从 `payload.projectId`
//! - `companyId`：始终从 `payload.companyId` 解析
//! - `agentId`：`entityType == "agent"` 时取 `entityId`，否则从 `payload.agentId`

use serde_json::Value;

use super::types::{EventFilter, PluginEvent};

/// 判定事件是否通过过滤器。
///
/// `None` 或全 `None` 字段的过滤器视为"通过全部"。
pub fn passes_filter(event: &PluginEvent, filter: Option<&EventFilter>) -> bool {
    let Some(filter) = filter else {
        return true;
    };

    let payload = event.payload.as_object();

    if let Some(expected) = filter.project_id.as_deref() {
        let resolved = resolve_field(event, payload, "project", "projectId");
        if resolved.as_deref() != Some(expected) {
            return false;
        }
    }

    if let Some(expected) = filter.company_id.as_deref() {
        let resolved = resolve_field(event, payload, "", "companyId");
        if resolved.as_deref() != Some(expected) {
            return false;
        }
    }

    if let Some(expected) = filter.agent_id.as_deref() {
        let resolved = resolve_field(event, payload, "agent", "agentId");
        if resolved.as_deref() != Some(expected) {
            return false;
        }
    }

    true
}

/// 字段解析：`entityType == <entity_field>` 时优先取 `entityId`，否则从 payload 取。
fn resolve_field(
    event: &PluginEvent,
    payload: Option<&serde_json::Map<String, Value>>,
    entity_field: &str,
    payload_key: &str,
) -> Option<String> {
    if !entity_field.is_empty() && event.entity_type.as_deref() == Some(entity_field) {
        return event.entity_id.clone();
    }
    payload
        .and_then(|m| m.get(payload_key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}
