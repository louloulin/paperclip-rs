//! 派发面：把 trigger / run 变成 `agent_task_queue` 行（以及「建 issue + 派任务」）。
//!
//! - **写者**：M5-4（`dispatch/**` 整组）。
//! - **上游**：`service/autopilot.go` 的 dispatch 段 ≈2,600 —— `DispatchAutopilot`35 /
//!   `DispatchAutopilotManual`12 / `DispatchAutopilotManualWithKey`20 / `dispatchAutopilot`52 /
//!   `dispatchAutopilotRun`57 / 三个执行分支 / `SyncRunFrom*`159 / `shouldSkipDispatch`98 /
//!   `recordSkippedRun`58 / `failRun`25 / `publishRunDone`13 / 分析 92 / 模板与工具 200。
//! - **三块必须分开测**（`docs/44` §4.2）：① `create_issue.rs` 建 issue + 派任务；
//!   ② `run_only.rs` 只派任务；③ `sync.rs` 把任务终态回写 run。
//! - **边界（R9）**：若发现 `agent_task_queue` 的 `queued` 行不会被执行（daemon 面未接线），
//!   登记为**跨波缺口**，**不要在本波实现 daemon**。
//! - **`autopilot_run.status` 必须显式写入**：列默认值还是 `'pending'`，而 `079` 之后 CHECK 只允许
//!   `{issue_created, running, completed, failed, skipped}` ⇒ 那个默认值已经**落在 CHECK 之外**，
//!   依赖默认值必然插入失败。
pub mod analytics;
pub mod create_issue;
pub mod run_only;
pub mod skip;
pub mod sync;
