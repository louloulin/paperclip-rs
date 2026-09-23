//! autopilot 配额（`autopilot_quota_period` / `autopilot_quota_reservation`）。
//!
//! - **写者**：M5-1。类型来自 `mc_core::autopilot_quota`（M5-0 已按 `352` + `448` 的列写死）。
//! - **上游**：`service/autopilot_quota.go`414 + `autopilot_quota_notifications.go`198 +
//!   `AutopilotQuotaUsage`40（`GET /api/autopilots/usage` 的唯一契约来源）+ `QuotaEnabled()`6。
//! - **`QuotaEnabled()` 依赖 entitlement 平面**（R7）⇒ 本地没有「商业默认值」可抄：限额与周期边界
//!   由云侧在运行时下发（`352` 的迁移注释原话）。`QuotaUsage.limit` 因此是 `Option<i64>`。
//! - **保留（reservation）语义**：`uq_autopilot_quota_reservation_key` 是
//!   `(workspace_id, period_start, period_end, idempotency_key) WHERE state <> 'released'`
//!   ⇒ `released` **会释放幂等键**（这是重试语义，不是漏洞）；
//!   `idx_autopilot_quota_reservation_state` 是 `state = 'reserved'` 的部分索引 ⇒ 需要一个
//!   **扫陈旧保留**的入口，因为 `autopilot_run.quota_reservation_id` **没有外键**
//!   （「run 已终态但保留还挂着」是可达状态）。
//! - **不要发明 reason code 词表**：`reason_code` / `source` 在库里是自由文本、无 CHECK。
