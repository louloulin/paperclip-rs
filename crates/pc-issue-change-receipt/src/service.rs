//! Service 实现 —— IssueChangeReceiptService。
//!
//! 设计：
//! - 纯函数 service，无 DB I/O
//! - 支持 `serde_json::Map` 直接输入（与 Node `Record<string, unknown>` 1:1）
//! - 也提供 `diff_from_issue`：从 IssueRow 序列化为 JSON 后 diff
//! - Hook 在关键点回调

use std::sync::Arc;

use serde_json::{Map, Value};
use thiserror::Error;

use pc_repos::issue::IssueRow;

use crate::hook::{IssueChangeReceiptHook, NoopIssueChangeReceiptHook};
use crate::{build_issue_changes, IssueChanges, RelationChangeInput};

/// Issue change receipt service 错误。
#[derive(Debug, Error)]
pub enum IssueChangeReceiptError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

pub type IssueChangeReceiptResult<T> = Result<T, IssueChangeReceiptError>;

/// Issue change receipt service —— 封装 `build_issue_changes` 纯函数 + Hook。
pub struct IssueChangeReceiptService {
    hook: Arc<dyn IssueChangeReceiptHook>,
}

impl std::fmt::Debug for IssueChangeReceiptService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueChangeReceiptService").finish()
    }
}

impl Default for IssueChangeReceiptService {
    fn default() -> Self {
        Self::new()
    }
}

impl IssueChangeReceiptService {
    /// 构造默认 service（NoopHook）。
    pub fn new() -> Self {
        Self {
            hook: Arc::new(NoopIssueChangeReceiptHook),
        }
    }

    /// 用自定义 hook 构造。
    pub fn with_hook(hook: Arc<dyn IssueChangeReceiptHook>) -> Self {
        Self { hook }
    }

    /// 取当前 hook（用于测试）。
    pub fn hook(&self) -> Arc<dyn IssueChangeReceiptHook> {
        self.hook.clone()
    }

    /// Diff 两个 issue 状态快照，返回字段级 diff。
    ///
    /// - 忽略 `updatedAt`（每次 update 都变）
    /// - `description` 字段总是 truncate 到 `ISSUE_CHANGE_TEXT_BUDGET` 字符
    /// - `title` 字段：任一长度 > 200 → truncate + 标记 `updated: true`
    /// - relation changes（blockedByIssueIds / labelIds）：去重 + 排序后再比较
    ///
    /// 与 Node `buildIssueChanges` 行为 1:1 对齐。
    pub fn diff(
        &self,
        existing: &Map<String, Value>,
        updated: &Map<String, Value>,
        relations: RelationChangeInput,
    ) -> IssueChanges {
        self.hook.before_diff(existing, updated);
        let changes = build_issue_changes(existing, updated, relations);
        if changes.is_empty() {
            self.hook.on_no_changes();
        } else {
            self.hook.after_diff(&changes);
        }
        changes
    }

    /// 便捷方法：bool 判断是否有 changes。
    pub fn has_changes(
        &self,
        existing: &Map<String, Value>,
        updated: &Map<String, Value>,
        relations: RelationChangeInput,
    ) -> bool {
        !self.diff(existing, updated, relations).is_empty()
    }

    /// 从两个 IssueRow 直接 diff。
    ///
    /// 会把每个 row 序列化为 `serde_json::Map` 后调用 `diff`。
    /// `relations` 由 caller 单独提供（一般从 DB 拉 blocked_by_issue_ids / label_ids）。
    pub fn diff_from_issue(
        &self,
        existing: &IssueRow,
        updated: &IssueRow,
        relations: RelationChangeInput,
    ) -> IssueChangeReceiptResult<IssueChanges> {
        let existing_map = issue_row_to_map(existing)?;
        let updated_map = issue_row_to_map(updated)?;
        Ok(self.diff(&existing_map, &updated_map, relations))
    }
}

/// 把 IssueRow 序列化为 serde_json::Map 用于 diff。
///
/// 与 Node 服务调用方一致：DB → Record<string, unknown> → buildIssueChanges。
fn issue_row_to_map(row: &IssueRow) -> IssueChangeReceiptResult<Map<String, Value>> {
    let value = serde_json::to_value(row).map_err(|e| IssueChangeReceiptError::InvalidInput(e.to_string()))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| IssueChangeReceiptError::InvalidInput("IssueRow did not serialize to object".to_string()))
}
