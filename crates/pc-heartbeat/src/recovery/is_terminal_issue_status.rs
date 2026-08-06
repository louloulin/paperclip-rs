//! `isTerminalIssueStatus` —— Node `services/recovery/service.ts:1325` 对齐。
//!
//! 业务语义：
//! - 判断 issue 是否处于 terminal disposition（done / cancelled）
//! - 用于 `fold_source_resolved_stale_run` 主循环短路（Node 第 2077 行）
//!
//! 设计意图：
//! - pure 函数：输入 status 字符串，输出 bool
//! - 与 Node 完全对齐：`status === "done" || status === "cancelled"`
//! - 提供 `is_terminal_issue_status_str`（&str）和 `is_terminal_issue_status_string`（String）两个 overload

/// Node `isTerminalIssueStatus` 的 Rust 等价（&str 版本）。
///
/// 返回 true 当 status == "done" || status == "cancelled"
pub fn is_terminal_issue_status_str(status: &str) -> bool {
    status == "done" || status == "cancelled"
}

/// Node `isTerminalIssueStatus` 的 Rust 等价（String 版本，便于直接传 row.status）。
pub fn is_terminal_issue_status_string(status: &String) -> bool {
    is_terminal_issue_status_str(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_done() {
        assert!(is_terminal_issue_status_str("done"));
    }

    #[test]
    fn recognizes_cancelled() {
        assert!(is_terminal_issue_status_str("cancelled"));
    }

    #[test]
    fn rejects_in_progress() {
        assert!(!is_terminal_issue_status_str("in_progress"));
    }

    #[test]
    fn rejects_todo() {
        assert!(!is_terminal_issue_status_str("todo"));
    }

    #[test]
    fn rejects_blocked() {
        assert!(!is_terminal_issue_status_str("blocked"));
    }

    #[test]
    fn rejects_in_review() {
        assert!(!is_terminal_issue_status_str("in_review"));
    }

    #[test]
    fn rejects_empty() {
        assert!(!is_terminal_issue_status_str(""));
    }
}
