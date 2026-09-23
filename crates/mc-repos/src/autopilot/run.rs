//! `autopilot_run` 仓储。
//!
//! - **写者**：M5-4（**W**；`docs/44` §3.2）。M5-8 读（调度 job 按 `planned_at` 分桶）。
//! - **上游 SQL**：`db/queries/autopilot.sql` 的 run 查询。
//! - **`status` 必须显式写**：列默认值仍是 `'pending'`，而 `079` 之后 CHECK 只允许
//!   `{issue_created, running, completed, failed, skipped}` ⇒ **默认值已在 CHECK 之外**，
//!   `INSERT ... DEFAULT` 会失败。
//! - **两个无外键的引用**：`quota_reservation_id` 与 `webhook_delivery_id` 都**没有** FK
//!   ⇒ 悬垂引用是可达状态，读侧要能容忍 `NULL`/悬垂，且配额回收（M5-1 的扫陈旧保留）要能看到它。
//! - **`planned_at`**（`124`）是调度面的分桶键，写 run 时要带上（否则 M5-8 的 `plan_time` 无法对账）。
