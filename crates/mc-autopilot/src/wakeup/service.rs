//! wakeup 的校验、保存（upsert）与派发。
//!
//! - **写者**：M5-6。
//! - **上游**：`service/issue_wakeup.go`831 的 `Validate`108 / `save`221 / `dispatch`185 /
//!   `CheckClaim`32 / `Tick`35；8 条路由的 handler 在 `mc-http/src/routes/{issues/wakeups,issue_wakeups}.rs`。
//! - **`kind` → 调度字段的对应关系是 app 侧校验**：`issue_wakeup` 表**没有**跨字段 CHECK
//!   （`kind=event` 用 `event_types`、`at` 用 `next_fire_at`、`every` 用 `interval_seconds`、
//!   `cron` 用 `cron_expression`）⇒ 校验必须在本文件写全，不能指望库。
//! - **容量上限走库的触发器**：`530` 的 `guard_issue_wakeup_capacity()` 抛
//!   `ERRCODE=23514` + `CONSTRAINT='issue_wakeup_active_limit'` ⇒ 本地要**按约束名**识别该错误
//!   （不要匹配错误文本），上限常量在 `mc_core::wakeup`（32 / 1000）。
//! - **`mode = once`** 的一次性语义：触发/消费后要走 disabled/回收路径，别留 enabled 死行。
//! - **派发**：与 M5-8 的 job 共用（`Tick` 薄壳在 `mc-scheduler/src/jobs/issue_wakeup.rs`）。
