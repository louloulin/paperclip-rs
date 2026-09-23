//! `issue_wakeup` 本体查询（含 workspace 级列表）。
//!
//! - **写者**：M5-6（**W**）。
//! - **上游 SQL**：`db/queries/wakeup.sql`162 / 23 查询 + `workspace_wakeup.sql`70 / 1 查询。
//! - **要落的写点**：create / upsert（`PUT` 与 `POST` 复用 `CreateIssueWakeup`）/ disable /
//!   enable / instruction 编辑 + 调度推进（`next_fire_at`）。
//! - **并发**：更新要带 `revision` 的乐观条件（`WHERE revision = $n`）并 **bump `revision`**；
//!   `revision` 同时是 receipt 的合并作用域 ⇒ 配置变更不 bump 会让旧 pending receipt 继续生效。
//! - **容量**：插入/启用时的上限由库触发器 `guard_issue_wakeup_capacity()`（`530`）兜底
//!   （`ERRCODE=23514` + `CONSTRAINT='issue_wakeup_active_limit'`）⇒ 错误映射要**按约束名**识别。
//! - **跨表读**：workspace 级列表要读 issue（标题等）与 `agent_task_queue`（运行态）
//!   ⇒ 只读、不写别表。
