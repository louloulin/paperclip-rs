# R723 — pc-issues/src/tree_control/pure.rs

## 目标

补足 Node services/issue-tree-control.ts 中零 DB pure helpers。

## 新增 helpers（5 个）

| Node 函数 | Rust 函数 |
|---|---|
| coerceIssueStatus | coerce_issue_status(status) |
| isTerminalIssue | is_terminal_issue(status) |
| normalizeReleasePolicy | normalize_release_policy(policy) |
| restoreStatusFromCancelSnapshot | restore_status_from_cancel_snapshot(status) |
| issueSkipReason | issue_skip_reason(input) + IssueSkipReasonInput struct |

## 测试结果

cargo test -p pc-issues --lib tree_control::pure
running 10 tests
...
test result: ok. 10 passed; 0 failed

## 关键设计

- coerce_issue_status 返回 'static str，避免 lifetime 问题
- normalize_release_policy 用 match 表达式确保返回 'static
- issue_skip_reason 严格按 Node 优先级：restore 分支优先，然后 terminal 检测，最后 mode-specific 检查
