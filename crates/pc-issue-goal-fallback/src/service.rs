//! Service 实现 —— IssueGoalFallbackService。
//!
//! 纯函数 service（无 DB I/O），封装两个 resolver + Hook。

use std::sync::Arc;

use pc_repos::issue_goal_fallback::{
    resolve_issue_goal_id as core_resolve, resolve_next_issue_goal_id as core_resolve_next,
    ResolveIssueGoalIdInput, ResolveNextIssueGoalIdInput,
};

use crate::hook::{IssueGoalFallbackHook, NoopIssueGoalFallbackHook};

/// 顶层公开函数：单点解析（与 Node `resolveIssueGoalId` 1:1）。
///
/// 设计：
/// - 接受 `ResolveIssueGoalIdInput`
/// - 返回 `Option<String>`（None 表示无法解析）
pub fn resolve_issue_goal_id(input: ResolveIssueGoalIdInput) -> Option<String> {
    core_resolve(input)
}

/// 顶层公开函数：状态迁移解析（与 Node `resolveNextIssueGoalId` 1:1）。
pub fn resolve_next_issue_goal_id(input: ResolveNextIssueGoalIdInput) -> Option<String> {
    core_resolve_next(input)
}

/// Issue goal fallback service —— 封装两个 resolver + Hook。
pub struct IssueGoalFallbackService {
    hook: Arc<dyn IssueGoalFallbackHook>,
}

impl std::fmt::Debug for IssueGoalFallbackService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueGoalFallbackService").finish()
    }
}

impl Default for IssueGoalFallbackService {
    fn default() -> Self {
        Self::new()
    }
}

impl IssueGoalFallbackService {
    pub fn new() -> Self {
        Self {
            hook: Arc::new(NoopIssueGoalFallbackHook),
        }
    }

    pub fn with_hook(hook: Arc<dyn IssueGoalFallbackHook>) -> Self {
        Self { hook }
    }

    pub fn hook(&self) -> Arc<dyn IssueGoalFallbackHook> {
        self.hook.clone()
    }

    /// 单点解析 issue goal_id（与 Node `resolveIssueGoalId` 1:1 对齐）。
    ///
    /// Hook 调用：
    /// - `before_resolve` 在解析前
    /// - `after_resolve` 当结果非 None
    /// - `on_null_single` 当结果为 None
    pub fn resolve(&self, input: ResolveIssueGoalIdInput) -> Option<String> {
        self.hook.before_resolve(&input);
        let result = core_resolve(input);
        match &result {
            Some(gid) => self.hook.after_resolve(gid),
            None => self.hook.on_null_single(),
        }
        result
    }

    /// 状态迁移解析 next issue goal_id（与 Node `resolveNextIssueGoalId` 1:1 对齐）。
    ///
    /// Hook 调用：
    /// - `before_resolve_next` 在解析前
    /// - `after_resolve_next` 当结果非 None
    /// - `on_null_next` 当结果为 None
    pub fn resolve_next(&self, input: ResolveNextIssueGoalIdInput) -> Option<String> {
        self.hook.before_resolve_next(&input);
        let result = core_resolve_next(input);
        match &result {
            Some(gid) => self.hook.after_resolve_next(gid),
            None => self.hook.on_null_next(),
        }
        result
    }
}
