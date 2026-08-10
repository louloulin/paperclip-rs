//! Tool 存储 trait + 内存实现。
//!
//! 高内聚：所有"tool 怎么存 / 怎么查 / 怎么删"的逻辑集中在这。
//! 低耦合：[`ToolStore`] 是 trait —— 可替换为 sqlite / redis / DB；
//! 上层 [`registry`] 通过 trait 操作，不依赖具体存储。

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::error::ToolRegistryError;
use super::types::{build_namespaced_name, RegisteredTool};

// ============================================================================
// Parsing helpers
// ============================================================================

/// 解析 `<plugin_id>:<tool_name>` 形式的字符串。
///
/// 与 Node `parseName` 1:1 对齐。返回 `None` 当字符串不含分隔符。
pub fn parse_namespaced_name(namespaced_name: &str) -> Option<ParsedToolName> {
    let idx = namespaced_name.find(super::types::TOOL_NAMESPACE_SEPARATOR)?;
    let plugin_id = &namespaced_name[..idx];
    let tool_name = &namespaced_name[idx + super::types::TOOL_NAMESPACE_SEPARATOR.len_utf8()..];
    if plugin_id.is_empty() || tool_name.is_empty() {
        return None;
    }
    Some(ParsedToolName {
        plugin_id: plugin_id.to_string(),
        tool_name: tool_name.to_string(),
    })
}

/// 解析后的命名空间名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedToolName {
    pub plugin_id: String,
    pub tool_name: String,
}

impl ParsedToolName {
    /// 重新组装为完整命名空间名。
    pub fn as_namespaced(&self) -> String {
        build_namespaced_name(&self.plugin_id, &self.tool_name)
    }
}

// ============================================================================
// ToolStore trait
// ============================================================================

/// Tool 存储 trait —— registry 只通过此接口访问底层存储。
///
/// 设计：trait 让测试可以用 mock store，registry 不用关心存储细节。
pub trait ToolStore: Send + Sync {
    /// 注册单个 tool（已构造好）。
    fn put(&self, tool: RegisteredTool);

    /// 通过命名空间名移除。
    fn remove(&self, namespaced_name: &str) -> Option<RegisteredTool>;

    /// 移除一个 plugin 的所有 tools，返回数量。
    fn remove_plugin(&self, plugin_id: &str) -> usize;

    /// 通过命名空间名查找。
    fn get(&self, namespaced_name: &str) -> Option<RegisteredTool>;

    /// 通过 `(plugin_id, tool_name)` 复合键查找。
    fn get_by_plugin(&self, plugin_id: &str, tool_name: &str) -> Option<RegisteredTool> {
        let namespaced = build_namespaced_name(plugin_id, tool_name);
        self.get(&namespaced)
    }

    /// 列出所有 tools（可选过滤）。
    fn list(&self, filter: &super::types::ToolListFilter) -> Vec<RegisteredTool>;

    /// tool 总数（可选 plugin_id 过滤）。
    fn count(&self, plugin_id: Option<&str>) -> usize;
}

// ============================================================================
// InMemoryToolStore
// ============================================================================

/// 内存 ToolStore —— 用二级索引 `HashMap<namespaced, tool>` + `HashMap<plugin_id, Set<namespaced>>`。
///
/// 与 Node `PluginToolRegistry` 内部数据结构 1:1 对齐。
///
/// 线程安全：用 `parking_lot::Mutex`（更快）或 `std::sync::Mutex`（零外部 dep）。
/// 选 `std::sync::Mutex` —— 零依赖 + `lock().unwrap()` 简单。
pub struct InMemoryToolStore {
    by_namespace: std::sync::Mutex<HashMap<String, RegisteredTool>>,
    by_plugin: std::sync::Mutex<HashMap<String, HashSet<String>>>,
}

impl Default for InMemoryToolStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryToolStore {
    pub fn new() -> Self {
        Self {
            by_namespace: std::sync::Mutex::new(HashMap::new()),
            by_plugin: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl ToolStore for InMemoryToolStore {
    fn put(&self, tool: RegisteredTool) {
        let mut by_ns = self.by_namespace.lock().expect("poisoned by_namespace");
        let mut by_pl = self.by_plugin.lock().expect("poisoned by_plugin");

        // 维护 plugin 索引
        by_pl
            .entry(tool.plugin_id.clone())
            .or_insert_with(HashSet::new)
            .insert(tool.namespaced_name.clone());

        by_ns.insert(tool.namespaced_name.clone(), tool);
    }

    fn remove(&self, namespaced_name: &str) -> Option<RegisteredTool> {
        let mut by_ns = self.by_namespace.lock().expect("poisoned by_namespace");
        let removed = by_ns.remove(namespaced_name)?;
        let mut by_pl = self.by_plugin.lock().expect("poisoned by_plugin");
        if let Some(set) = by_pl.get_mut(&removed.plugin_id) {
            set.remove(namespaced_name);
            if set.is_empty() {
                by_pl.remove(&removed.plugin_id);
            }
        }
        Some(removed)
    }

    fn remove_plugin(&self, plugin_id: &str) -> usize {
        let mut by_ns = self.by_namespace.lock().expect("poisoned by_namespace");
        let mut by_pl = self.by_plugin.lock().expect("poisoned by_plugin");
        let Some(set) = by_pl.remove(plugin_id) else {
            return 0;
        };
        let mut removed = 0;
        for name in set {
            if by_ns.remove(&name).is_some() {
                removed += 1;
            }
        }
        removed
    }

    fn get(&self, namespaced_name: &str) -> Option<RegisteredTool> {
        self.by_namespace
            .lock()
            .expect("poisoned by_namespace")
            .get(namespaced_name)
            .cloned()
    }

    fn list(&self, filter: &super::types::ToolListFilter) -> Vec<RegisteredTool> {
        let by_ns = self.by_namespace.lock().expect("poisoned by_namespace");
        let by_pl = self.by_plugin.lock().expect("poisoned by_plugin");

        if let Some(ref pid) = filter.plugin_id {
            let Some(set) = by_pl.get(pid) else {
                return Vec::new();
            };
            set.iter()
                .filter_map(|name| by_ns.get(name).cloned())
                .collect()
        } else {
            by_ns.values().cloned().collect()
        }
    }

    fn count(&self, plugin_id: Option<&str>) -> usize {
        if let Some(pid) = plugin_id {
            self.by_plugin
                .lock()
                .expect("poisoned by_plugin")
                .get(pid)
                .map(|s| s.len())
                .unwrap_or(0)
        } else {
            self.by_namespace
                .lock()
                .expect("poisoned by_namespace")
                .len()
        }
    }
}

// ============================================================================
// Helper for parse error path
// ============================================================================

#[allow(dead_code)]
pub(crate) fn invalid_name(name: &str) -> ToolRegistryError {
    ToolRegistryError::InvalidToolName(name.to_string())
}

// silence unused warning for Uuid
#[allow(dead_code)]
fn _uuid_use(uuid: Uuid) -> Uuid {
    uuid
}
