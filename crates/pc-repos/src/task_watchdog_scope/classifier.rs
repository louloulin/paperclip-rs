//! Round 253: Task-watchdog capability classifier。
//!
//! 与 Node `TaskWatchdogClassifier` / `TaskWatchdogCapability` 1:1 对齐：
//! - watchdog 触发时，给被 wake 的 agent 派发一组「允许 / 禁止」操作 capability。
//! - capability 描述了 agent 可以对哪些 issue 子树做哪些操作（例如：仅 issue 评论 / 修改状态 / 修改 assignee）。
//!
//! 设计：
//! - `WatchdogOperation` enum 列出 watchdog 允许 / 禁止的具体操作。
//! - `TaskWatchdogTargetScope` 描述 scope 边界（watched_issue_id / descendants / siblings / depth）。
//! - `TaskWatchdogCapability` 聚合 target_scope + operations + denied_operations。
//! - `classify_task_watchdog_capability(input) -> TaskWatchdogCapability`：从
//!   `IssueWatchdogRow` + `IssueRow` (watched) 派生默认 capability。
//! - `default_capability_for_resume()` 提供保守默认（仅 comment + status_change）。
//! - `serde_json::Value` 序列化输出与 Node JSON shape 完全一致。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

/// Watchdog 允许 / 禁止的具体操作（与 Node `WatchdogOperation` 1:1 对齐）。
///
/// 注：Node 端用 `string` 联合类型（"comment" / "status_change" / "assign" / "archive" / "label" 等）；
/// Rust 端用 enum 强类型，避免拼写错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchdogOperation {
    /// 给 issue 增加 comment
    Comment,
    /// 修改 issue.status 字段
    StatusChange,
    /// 修改 issue.assignee_agent_id / assignee_user_id
    Assign,
    /// 修改 / 增加 issue labels
    Label,
    /// 修改 issue 优先级 / work_mode / harness_kind
    UpdateMetadata,
    /// 创建 / 删除 child issue
    ManageChildren,
    /// 标记 issue 为 archived（破坏性）
    Archive,
    /// 删除 issue（破坏性）
    Delete,
    /// 修改 issue.monitor_*（破坏性 — 关闭 monitor 会绕过 watchdog）
    DisableMonitor,
}

impl WatchdogOperation {
    /// 返回操作在 JSON 中的字符串名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::StatusChange => "status_change",
            Self::Assign => "assign",
            Self::Label => "label",
            Self::UpdateMetadata => "update_metadata",
            Self::ManageChildren => "manage_children",
            Self::Archive => "archive",
            Self::Delete => "delete",
            Self::DisableMonitor => "disable_monitor",
        }
    }

    /// 是否是「破坏性」操作（默认应被 watchdog 拒绝）。
    pub const fn is_destructive(self) -> bool {
        matches!(self, Self::Archive | Self::Delete | Self::DisableMonitor)
    }
}

/// Scope 边界（与 Node `TaskWatchdogTargetScope` 1:1 对齐）。
///
/// 注：scope 是 JSON 嵌套对象，序列化时使用 camelCase 与 Node 一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskWatchdogTargetScope {
    /// 被监听的 issue id（必填）
    pub watched_issue_id: Uuid,
    /// 是否允许对 watched_issue 的非 watchdog 后代做修改
    #[serde(default = "default_true")]
    pub include_non_watchdog_descendants: bool,
    /// 是否允许对 watched_issue 的非 watchdog 兄弟做修改
    #[serde(default)]
    pub include_unwatched_siblings: bool,
    /// 修改子树的深度限制（None = 不限）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth_limit: Option<i32>,
}

fn default_true() -> bool {
    true
}

impl TaskWatchdogTargetScope {
    /// 构造最小 scope（仅 watched_issue 本身 + 子树）。
    pub fn subtree_only(watched_issue_id: Uuid) -> Self {
        Self {
            watched_issue_id,
            include_non_watchdog_descendants: true,
            include_unwatched_siblings: false,
            depth_limit: None,
        }
    }
}

/// Capability 聚合（与 Node `TaskWatchdogCapability` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskWatchdogCapability {
    pub target_scope: TaskWatchdogTargetScope,
    /// 允许的操作列表
    pub operations: Vec<WatchdogOperation>,
    /// 禁止的操作列表（与 operations 是补集关系；冗余用于 runtime 检查）
    pub denied_operations: Vec<WatchdogOperation>,
}

impl TaskWatchdogCapability {
    /// 序列化为 Node JSON shape。
    pub fn to_node_json(&self) -> Value {
        json!({
            "targetScope": self.target_scope,
            "operations": self.operations,
            "deniedOperations": self.denied_operations,
        })
    }
}

/// `classify_task_watchdog_capability` 输入参数。
///
/// 注：Node 端是直接从 `IssueWatchdogRow` + `IssueRow` (watched) + 一些
/// 派生字段派生；Rust 端为简化，只取最关键的几个字段。
#[derive(Debug, Clone)]
pub struct ClassifyTaskWatchdogCapabilityInput<'a> {
    /// 被监听的 issue id
    pub watched_issue_id: Uuid,
    /// watchdog 行自定义 instructions（可被 classifier 用来调整默认 capability）
    pub custom_instructions: Option<&'a str>,
    /// 是否允许 destructive 操作（默认 false）
    pub allow_destructive: bool,
}

/// 默认 capability for resume — watchdog 触发时给被 wake agent 的最小操作集。
///
/// 默认行为（与 Node `defaultCapability` 对齐）：
/// - scope: subtree only（仅 watched_issue 及其后代）
/// - operations: Comment + StatusChange + Assign + Label + UpdateMetadata
/// - denied_operations: 破坏性操作（Archive / Delete / DisableMonitor）
///   除非 `allow_destructive` 为 true
pub fn default_capability_for_resume(
    input: &ClassifyTaskWatchdogCapabilityInput<'_>,
) -> TaskWatchdogCapability {
    let operations = vec![
        WatchdogOperation::Comment,
        WatchdogOperation::StatusChange,
        WatchdogOperation::Assign,
        WatchdogOperation::Label,
        WatchdogOperation::UpdateMetadata,
    ];
    let mut denied_operations = vec![
        WatchdogOperation::Archive,
        WatchdogOperation::Delete,
        WatchdogOperation::DisableMonitor,
        WatchdogOperation::ManageChildren,
    ];
    if input.allow_destructive {
        denied_operations.retain(|op| !op.is_destructive());
    }
    TaskWatchdogCapability {
        target_scope: TaskWatchdogTargetScope::subtree_only(input.watched_issue_id),
        operations,
        denied_operations,
    }
}

/// 主要入口：根据 watchdog 行 + 上下文派生 capability。
///
/// 当前实现直接调用 `default_capability_for_resume`；未来按 `custom_instructions`
/// 关键字（如 "allow_archive" / "deny_assign"）做调整。
pub fn classify_task_watchdog_capability(
    input: &ClassifyTaskWatchdogCapabilityInput<'_>,
) -> TaskWatchdogCapability {
    let mut capability = default_capability_for_resume(input);
    if let Some(instr) = input.custom_instructions {
        // 解析 custom_instructions 中的关键字
        let lower = instr.to_ascii_lowercase();
        if lower.contains("allow_archive") || lower.contains("allow_destructive") {
            capability
                .denied_operations
                .retain(|op| !op.is_destructive());
            if !capability.operations.contains(&WatchdogOperation::Archive) {
                capability.operations.push(WatchdogOperation::Archive);
            }
        }
        if lower.contains("deny_assign") {
            capability
                .operations
                .retain(|op| *op != WatchdogOperation::Assign);
            if !capability
                .denied_operations
                .contains(&WatchdogOperation::Assign)
            {
                capability.denied_operations.push(WatchdogOperation::Assign);
            }
        }
        if lower.contains("allow_children") {
            capability
                .denied_operations
                .retain(|op| *op != WatchdogOperation::ManageChildren);
            if !capability
                .operations
                .contains(&WatchdogOperation::ManageChildren)
            {
                capability
                    .operations
                    .push(WatchdogOperation::ManageChildren);
            }
        }
        if lower.contains("allow_siblings") {
            capability.target_scope.include_unwatched_siblings = true;
        }
    }
    capability
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_as_str_matches_node_contract() {
        assert_eq!(WatchdogOperation::Comment.as_str(), "comment");
        assert_eq!(WatchdogOperation::StatusChange.as_str(), "status_change");
        assert_eq!(WatchdogOperation::Archive.as_str(), "archive");
    }

    #[test]
    fn operation_is_destructive_marks_archive_delete_disable_monitor() {
        assert!(WatchdogOperation::Archive.is_destructive());
        assert!(WatchdogOperation::Delete.is_destructive());
        assert!(WatchdogOperation::DisableMonitor.is_destructive());
        assert!(!WatchdogOperation::Comment.is_destructive());
        assert!(!WatchdogOperation::StatusChange.is_destructive());
    }

    #[test]
    fn target_scope_subtree_only_has_siblings_false() {
        let id = Uuid::new_v4();
        let s = TaskWatchdogTargetScope::subtree_only(id);
        assert_eq!(s.watched_issue_id, id);
        assert!(s.include_non_watchdog_descendants);
        assert!(!s.include_unwatched_siblings);
        assert_eq!(s.depth_limit, None);
    }

    #[test]
    fn target_scope_serializes_to_camel_case() {
        let id = Uuid::nil();
        let s = TaskWatchdogTargetScope::subtree_only(id);
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("watchedIssueId").is_some());
        assert!(v.get("includeNonWatchdogDescendants").is_some());
        assert!(v.get("includeUnwatchedSiblings").is_some());
    }

    #[test]
    fn default_capability_deny_destructive_by_default() {
        let id = Uuid::new_v4();
        let cap = default_capability_for_resume(&ClassifyTaskWatchdogCapabilityInput {
            watched_issue_id: id,
            custom_instructions: None,
            allow_destructive: false,
        });
        assert!(cap.denied_operations.contains(&WatchdogOperation::Archive));
        assert!(cap.denied_operations.contains(&WatchdogOperation::Delete));
        assert!(cap
            .denied_operations
            .contains(&WatchdogOperation::DisableMonitor));
        assert!(cap.operations.contains(&WatchdogOperation::Comment));
        assert!(cap.operations.contains(&WatchdogOperation::StatusChange));
        assert!(!cap.operations.contains(&WatchdogOperation::Archive));
    }

    #[test]
    fn default_capability_allow_destructive_when_flag_true() {
        let id = Uuid::new_v4();
        let cap = default_capability_for_resume(&ClassifyTaskWatchdogCapabilityInput {
            watched_issue_id: id,
            custom_instructions: None,
            allow_destructive: true,
        });
        assert!(!cap.denied_operations.contains(&WatchdogOperation::Archive));
        assert!(!cap.denied_operations.contains(&WatchdogOperation::Delete));
        assert!(!cap
            .denied_operations
            .contains(&WatchdogOperation::DisableMonitor));
    }

    #[test]
    fn classifier_allow_archive_keyword_unlocks_archive() {
        let id = Uuid::new_v4();
        let cap = classify_task_watchdog_capability(&ClassifyTaskWatchdogCapabilityInput {
            watched_issue_id: id,
            custom_instructions: Some("allow_archive for this run"),
            allow_destructive: false,
        });
        assert!(cap.operations.contains(&WatchdogOperation::Archive));
        assert!(!cap.denied_operations.contains(&WatchdogOperation::Archive));
    }

    #[test]
    fn classifier_deny_assign_keyword_blocks_assign() {
        let id = Uuid::new_v4();
        let cap = classify_task_watchdog_capability(&ClassifyTaskWatchdogCapabilityInput {
            watched_issue_id: id,
            custom_instructions: Some("deny_assign to prevent abuse"),
            allow_destructive: false,
        });
        assert!(!cap.operations.contains(&WatchdogOperation::Assign));
        assert!(cap.denied_operations.contains(&WatchdogOperation::Assign));
    }

    #[test]
    fn classifier_allow_children_keyword_unlocks_manage_children() {
        let id = Uuid::new_v4();
        let cap = classify_task_watchdog_capability(&ClassifyTaskWatchdogCapabilityInput {
            watched_issue_id: id,
            custom_instructions: Some("allow_children for decomposition"),
            allow_destructive: false,
        });
        assert!(cap.operations.contains(&WatchdogOperation::ManageChildren));
    }

    #[test]
    fn classifier_allow_siblings_keyword_unlocks_siblings_scope() {
        let id = Uuid::new_v4();
        let cap = classify_task_watchdog_capability(&ClassifyTaskWatchdogCapabilityInput {
            watched_issue_id: id,
            custom_instructions: Some("allow_siblings for cross-issue fix"),
            allow_destructive: false,
        });
        assert!(cap.target_scope.include_unwatched_siblings);
    }

    #[test]
    fn capability_to_node_json_shape_matches_node_contract() {
        let id = Uuid::nil();
        let cap = default_capability_for_resume(&ClassifyTaskWatchdogCapabilityInput {
            watched_issue_id: id,
            custom_instructions: None,
            allow_destructive: false,
        });
        let v = cap.to_node_json();
        // 顶层 keys: targetScope / operations / deniedOperations
        assert!(v.get("targetScope").is_some());
        assert!(v.get("operations").is_some());
        assert!(v.get("deniedOperations").is_some());
        // targetScope 是 object，含 watchedIssueId
        let ts = v.get("targetScope").unwrap();
        assert!(ts.get("watchedIssueId").is_some());
        // operations 是 array
        assert!(v.get("operations").unwrap().is_array());
    }
}
