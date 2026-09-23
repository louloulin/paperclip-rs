//! 跨切片共享的响应 DTO（`mc-autopilot` 侧）。
//!
//! - **写者**：M5-1。各切片私有的 DTO 放各自文件，只有跨切片共享的进这里。
//! - **上游**：`autopilotToResponse`37 / `triggerToResponse`50 / `runToResponse`32 /
//!   `runToResponseSlim`88（`handler/autopilot.go`）。
//! - **最容易被抄错的契约**（`docs/44` §4.2 M5-1 原话）：`assignee_type` / `pause_reason` /
//!   `execution_mode` / `can_write` / `can_manage_access`，以及列表专属的 `trigger_kinds` /
//!   `next_run_at` / `last_run_status`。
//! - **`can_write` 是 `Option<bool>`，不是 `bool`**：上游文档注释写明「不带 caller 时省略该字段，
//!   客户端按 unknown 处理」⇒ 本地必须区分「省略」与 `false`。
//! - **时间戳一律 `mc_core::Timestamp`**，序列化形态由 `mc-core` 定（不要各切片自己 `to_rfc3339`）。
