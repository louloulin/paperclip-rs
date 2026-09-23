//! M4 anchor scaffold（LUM-1470）：`chat_task` 仓储 —— **占位，无实现**。
//!
//! 归属：M4-4（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。填充内容 = 派发面的任务队列读写：
//! `SendChatMessage` → `EnqueueChatTask`（上游 `chat.go:1728`）、pending / queued 的查询与
//! `queued-tasks{clear,prioritize}`，覆盖 `router.go` #8–#9、#13–#15、#19–#20。
//!
//! ⚠️ 跨波依赖（`docs/42` §4.3 第 1/2 条，必须登记）：
//! 1. 本模块**读写的是 M3 的表** `agent_task_queue`（领域层 `mc-task` 已合、任务队列用户面
//!    M3-6 已合）。⇒ 状态取值与列必须对齐 `mc_task::status::TaskStatus` 与上游迁移的
//!    CHECK 约束；**本仓 `migrations/0001_init.up.sql:230` 的 CHECK 是错的**，不能当契约
//!    （见 `crates/mc-task/src/lib.rs` 的模块文档）。
//! 2. ws 广播（`BroadcastTaskQueued` / `chat:done`）属 M3-7 的 notifier —— 本模块只负责
//!    落库与读回，**不自己发事件**（发事件在 `mc-http` 的路由层）。
//!
//! 上游真值：`server/pkg/db/queries/chat.sql` 的 task/queue 部分 +
//! `task_message.sql`（98 行 / 5 条 query，`task_message` 只读）。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::task` / `crate::agent`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写 `sqlx::FromRow`
//!   （`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，不允许静默跳过）
//!
//! 硬约束：**不引入本仓自造列**（尤其 `mc-task` 模块文档列出的 8 个自造列）；不加迁移；
//! `idx_one_pending_task_per_issue` 这类部分唯一索引若被本片触及，必须在 DB 测试里**真实触发**。
//!
//! scaffold 阶段本文件只有文档注释，避免 M4-3 / M4-4 同时编辑 `crate::lib`。
