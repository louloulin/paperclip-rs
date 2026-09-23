//! `run_only` 执行模式：只派任务，不建 issue。
//!
//! - **写者**：M5-4。
//! - **上游**：`dispatchRunOnly`104。
//! - **数据来源**：`autopilot.execution_mode = 'run_only'`，`issue_title_template` 不参与。
//! - **共同点**：与 `create_issue.rs` 共用 `dispatchAutopilotRun`57 的 run 落库与配额保留
//!   （`autopilot_run.quota_reservation_id` **无外键**，见 `../quota.rs`）。
