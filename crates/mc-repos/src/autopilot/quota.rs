//! 配额仓储（`autopilot_quota_period` / `autopilot_quota_reservation`）。
//!
//! - **写者**：M5-1（**W**；`docs/44` §3.2）。M5-4 读（派发前保留配额）。
//! - **上游 SQL**：`db/queries/autopilot_quota.sql`148 / 10 查询。
//! - **两张表的键不一样**：`autopilot_quota_period` 的主键是
//!   `(workspace_id, period_start, period_end)`（**没有 `id`**，多种周期约定可共存）；
//!   `autopilot_quota_reservation` 的主键是 `id`。
//! - **幂等键**：`uq_autopilot_quota_reservation_key` =
//!   `(workspace_id, period_start, period_end, idempotency_key) WHERE state <> 'released'`
//!   ⇒ `released` 之后同一幂等键可再占用（**这是重试语义**）。
//! - **要落的入口**：保留（reserve）/ 记账（consume）/ 释放（release）+ **扫陈旧保留**
//!   （`idx_autopilot_quota_reservation_state` 是 `state='reserved'` 的部分索引，专为它建）。
//!   `448` 的 `rejection_notified_at` 是一次性通知标记（同一个周期只通知一次）。
//! - **不要发明限额**：限额与周期边界由 entitlement 平面在运行时下发（`352` 的迁移注释）
//!   ⇒ 本文件只读写事实，不写商业默认值。
