//! `issue_wakeup_receipt` 查询（证据 / 合并 / 消费）。
//!
//! - **写者**：M5-6（**W**）。
//! - **上游 SQL**：`db/queries/wakeup.sql` 的 receipt 段 + `528`/`529` 的索引。
//! - **唯一索引**：`issue_wakeup_pending_event_idx` =
//!   `(wakeup_id, revision, coalesce_key) WHERE processed_at IS NULL AND coalesce_key IS NOT NULL`
//!   ⇒ 「同一 `(wakeup, revision, event_type)` 只留一条 pending」由库保证；
//!   `coalesce_key IS NULL` 时不参与合并（同 `(wakeup_id, revision)` 可以有多条 pending）。
//! - **合并的副作用**：冲突合并走 `DO UPDATE SET id = EXCLUDED.id`（**row id 会轮换**）⇒
//!   任何「先读 pending 再按 id 更新」的路径都要在同一个事务里加锁，否则会更新到已消失的行。
//! - **消费**：`processed_at` 由 NULL → 时间戳；`task_id` 记录本次唤醒派出的任务。
//! - **`issue_wakeup_receipt_key_idx` 的唯一冲突 = 重复事件（吞掉）**；**其它**唯一冲突必须重抛。
