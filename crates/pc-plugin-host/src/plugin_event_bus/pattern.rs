//! Event pattern matching + 命名空间守卫（与 Node `matchesPattern` 1:1 对齐）。
//!
//! 单一职责：决定一个事件类型是否匹配某个订阅 pattern；提供命名空间前缀常量。

/// 插件事件命名空间前缀（与 Node 隐式 `plugin.` 1:1 对齐）。
pub const PLUGIN_EVENT_PREFIX: &str = "plugin.";

/// 判定 `event_type` 是否匹配订阅 `pattern`。
///
/// 匹配规则（与 Node 1:1 对齐）：
/// - 精确匹配：`"issue.created"` 匹配 `"issue.created"`
/// - 后缀通配：`"plugin.foo.*"` 匹配任何以 `"plugin.foo."` 开头的事件
/// - 不支持 glob，仅支持以 `.` 分隔的尾随 `*`
///
/// 注意：`"foo*"` 或 `"foo.*bar"` 不会被识别为通配符 —— 必须以 `.*` 结尾才算通配。
pub fn matches_pattern(event_type: &str, pattern: &str) -> bool {
    if pattern == event_type {
        return true;
    }

    if let Some(prefix) = pattern.strip_suffix(".*") {
        // Trailing wildcard: "plugin.foo.*" → prefix is "plugin.foo."
        // 注意保留前缀末尾的 "."
        event_type.starts_with(&format!("{prefix}.")) || event_type == prefix.trim_end_matches('.')
    } else {
        false
    }
}

/// 验证插件提供的 event name（与 Node `forPlugin().emit()` 校验 1:1 对齐）。
///
/// 校验项：
/// 1. 非空
/// 2. 不以 `plugin.` 前缀开头（防命名空间仿冒）
pub fn validate_event_name(plugin_id: &str, name: &str) -> Result<(), super::ScopedBusError> {
    if name.trim().is_empty() {
        return Err(super::ScopedBusError::EmptyEventName {
            plugin_id: plugin_id.to_string(),
        });
    }
    if name.starts_with(PLUGIN_EVENT_PREFIX) {
        return Err(super::ScopedBusError::ForbiddenPrefix {
            plugin_id: plugin_id.to_string(),
        });
    }
    Ok(())
}

/// 构造命名空间化的事件类型（与 Node `\`plugin.${pluginId}.${name}\`` 1:1 对齐）。
pub fn namespaced_event_type(plugin_id: &str, name: &str) -> String {
    format!("{PLUGIN_EVENT_PREFIX}{plugin_id}.{name}")
}
