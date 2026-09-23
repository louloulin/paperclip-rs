//! 事件证据捕获（`issue_wakeup_receipt`）与 actor 过滤。
//!
//! - **写者**：M5-6。
//! - **上游**：`service/issue_wakeup_evidence.go`135（136）+ `handler/wakeup_actor.go`63（≈64）。
//! - **DB 侧语义（必须逐条照抄 `530`/`532` 的 `capture_issue_wakeup()`）**：
//!   1. 先早退：没有 `enabled AND kind='event' AND p_type = ANY(event_types)` 的订阅就不写 receipt；
//!   2. 再早退：issue 必须存在、`status NOT IN ('done','cancelled')`、且没有
//!      `category IN ('done','closed')` 的 status 定义；
//!   3. `evidence`（version 1）字段：`event_id` / `event_type` / `version` / `occurred_at` /
//!      `workspace_id` / `issue_id` / `source_task_id` / `agent_id` / `actor_type` / `actor_id`
//!      + 调用方 payload（**只带引用与字段名，不带评论正文 / 附件 URL / metadata 值**）；
//!   4. 命中规则是**与**关系：`filter_agent_id` / `filter_task_id` 相等、
//!      `filter_actor_type` / `filter_actor_id` 对证据 actor、**自我抑制**
//!      （`p_task IS DISTINCT FROM source_task_id`）、**循环抑制**
//!      （`agent_task_queue.context->>'wakeup_id' == wakeup.id`）；
//!   5. 合并键是 `coalesce_key = event_type`：同一 `(wakeup_id, revision, event_type)` 只留一条
//!      pending，冲突时 `DO UPDATE SET id = EXCLUDED.id`（**row id 会轮换**，
//!      所以「读了但没加锁」的派发方只能消费它看见的那一版），`payload.coalesced_count` +1、
//!      `payload.first_occurred_at` 保留；
//!   6. `issue_wakeup_receipt_key_idx` 唯一冲突 = 重复事件（吞掉），**其它**唯一冲突要重新抛。
//! - **actor 三态**：`filter_actor_type` 的 CHECK（`531`，**NOT VALID**）只允许 `member|agent`；
//!   `system` 只作为**证据里的** actor 存在 ⇒ 用 `mc_core::wakeup::WakeupActorType::is_filterable()`
//!   把这条规则显式化，不要当成缺陷「修正」。
//! - **注意**：本文件是**读侧语义**的复制，真正的写入路径在库函数里；本地实现若改为应用侧写入，
//!   必须把上面 6 条全部落进代码并加真库测试（R2 类判据）。
