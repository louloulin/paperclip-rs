#![forbid(unsafe_code)]
//! `pc-run-continuations` —— 转发 `recovery/*` 子模块。
//!
//! 对应 Node `server/src/services/run-continuations.ts`（11 行）。
//!
//! 原模块是一个 **re-export barrel** —— 把 `recovery/run-liveness-continuations.js`
//! 和 `recovery/issue-graph-liveness.js` 的 API 重新导出。
//!
//! Rust 端策略：
//! - 不创建转发层（避免循环依赖）；调用方直接引用目标 crate
//! - 本 crate 仅保留一个 **类型别名列表** 用于文档/搜索索引目的

// 这是 1:1 复刻 Node barrel 模块的语义层：
// Rust 端把 API 拆分到 pc-run-liveness-continuations 与 pc-issue-graph-liveness，
// 上层按需直接引用。

/// 标记常量 —— 与 Node barrel 1:1 文档化（无运行时行为）。
///
/// 调用方按需引用：
/// - `pc_run_liveness_continuations` crate 提供 `DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS`,
///   `RUN_LIVENESS_CONTINUATION_REASON`, `build_run_liveness_continuation_idempotency_key`,
///   `decide_run_liveness_continuation`, `find_existing_run_liveness_continuation_wake`,
///   `read_continuation_attempt`, `RunContinuationDecision`
/// - `pc_issue_graph_liveness` crate 提供 `classify_issue_graph_liveness`,
///   `IssueGraphLivenessInput`, `IssueLivenessAgentInput`, `IssueLivenessDependencyPathEntry`,
///   `IssueLivenessExecutionPathInput`, `IssueLivenessFinding`, `IssueLivenessIssueInput`,
///   `IssueLivenessOwnerCandidate`, `IssueLivenessOwnerCandidateReason`,
///   `IssueLivenessRelationInput`, `IssueLivenessSeverity`, `IssueLivenessState`
pub const BARREL_DOC: &str = "run-continuations barrel re-exports recovery/* APIs";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r704_barrel_doc_is_stable() {
        // 测试目的：确保 barrel 文档字符串不变，方便文档生成器引用
        assert!(BARREL_DOC.starts_with("run-continuations"));
    }
}
