//! Capability check 结果类型。
//!
//! 高内聚：所有"检查结果"的形状集中在一个文件。
//! 低耦合：纯数据，不依赖 validator 实现细节。

use std::fmt;

use crate::capabilities::PluginCapability;

// ============================================================================
// CapabilityCheckResult
// ============================================================================

/// 单次 capability check 的结果。
///
/// 与 Node `CapabilityCheckResult` 1:1 对齐。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityCheckResult {
    /// 检查是否通过（`true` 表示 plugin 拥有全部所需 capability）。
    pub allowed: bool,
    /// 缺失的 capability 列表（`allowed=true` 时为空）。
    pub missing: Vec<PluginCapability>,
    /// 触发本次检查的 operation（runtime gate 时有值）。
    pub operation: Option<String>,
    /// 被检查的 plugin id。
    pub plugin_id: Option<String>,
}

impl CapabilityCheckResult {
    pub const fn allowed() -> Self {
        Self {
            allowed: true,
            missing: Vec::new(),
            operation: None,
            plugin_id: None,
        }
    }

    pub fn denied(missing: Vec<PluginCapability>) -> Self {
        Self {
            allowed: false,
            missing,
            operation: None,
            plugin_id: None,
        }
    }
}

impl fmt::Display for CapabilityCheckResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CapabilityCheckResult {{ allowed: {}, missing: [{}] }}",
            self.allowed,
            self.missing
                .iter()
                .map(PluginCapability::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
