//! issue wakeup 仓储（`issue_wakeup` 26 列 / `issue_wakeup_receipt` 10 列）。
//!
//! - **状态**：M5-0 anchor（`LUM-1563`）只建文件，**0 查询、0 类型**（`docs/44` §5.3）。
//! - **写者**：M5-6（**W**：`wakeup/**.rs` 整组；`docs/44` §3.2）。M5-7 / M5-8 只 **读**。
//! - **上游 SQL**：`db/queries/wakeup.sql`162 / 23 查询 + `db/queries/workspace_wakeup.sql`70 / 1 查询
//!   （workspace 级列表 = `GET /api/issue-wakeups` 的来源）。
//! - **拆文件**：`issue.rs` = wakeup 本体（含 workspace 级列表查询）、`receipt.rs` = 证据/收据。
//! - **行结构口径**：列序与类型逐字段对照 `mc_core::wakeup` 的头表
//!   （`event_types` 是 `text[] NOT NULL DEFAULT '{}'`、`payload` 是 jsonb、
//!   `revision` 既是乐观并发版本又是合并作用域）。
//! - **本仓约定**：裸 `Uuid` 字段 + 手写 `sqlx::FromRow` + `crate::workspace::map_sqlx_err`。
pub mod issue;
pub mod receipt;
