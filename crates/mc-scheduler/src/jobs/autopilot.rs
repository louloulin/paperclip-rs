//! autopilot 调度 job：按 `next_run_at` 把到期的 trigger 变成 run。
//!
//! - **写者**：M5-8。
//! - **上游**：`scheduler/jobs_autopilot.go`448（449）—— `autopilotScopes`66 +
//!   `autopilotPlansForScope`84 + `isAutopilotSchedulePlanStale`9 + `advancedNextRun`15。
//! - **难点**：按 **workspace × 时区**分桶算 `plan_time`，并且要处理「慢 tick 之后 plan 已过期」
//!   （`isAutopilotSchedulePlanStale`9）；`autopilot_run.planned_at`（`124`）是分桶的证据列。
//! - **cron 解析**：调用 `mc_autopilot::trigger` 的 5 字段解析器（不要在 job 里重写）。
//! - **写行**：结果写入 `agent_task_queue` / `autopilot_run`；**不碰 daemon 协议**（R9）。
