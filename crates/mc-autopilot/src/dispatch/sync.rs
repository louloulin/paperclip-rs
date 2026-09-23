//! run 终态回写：`SyncRunFrom*`。
//!
//! - **写者**：M5-4。
//! - **上游**：`SyncRunFrom*`159 + `failRun`25 + `publishRunDone`13。
//! - **终态集合**（`079` + `043`）：`issue_created` / `running` / `completed` / `failed` / `skipped`
//!   （`043` 把 `pending`+`skipped` 合成 `failed`，`079` 再加回 `skipped`；
//!   `autopilot_run_planned_at`(124) 供调度面按 plan 时间分桶）。
//! - **回写时机**：由 daemon 侧的 task 终态驱动（M3-7 的 daemon loop）；本文件只做映射与落库，
//!   不要在这里起轮询循环。
