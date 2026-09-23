//! M4 anchor scaffold（LUM-1470）：`chat_session` 仓储 —— **占位，无实现**。
//!
//! 归属：M4-3（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。填充内容 = 会话集合/单体读写、
//! pin / archive / 未读计数（`mark read`），覆盖上游 `router.go` #1–#7、#16（
//! `/api/chat/sessions*`）。
//!
//! 上游真值：表 `chat_session`（`migrations/upstream/033_chat.up.sql`，10 列）；
//! 查询面 `server/pkg/db/queries/chat.sql`（1660 行 / 77 条 query —— **本文件只取
//! session 相关那部分**，其余按 §4.2 的模块边界整块搬到同名兄弟文件）。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::issue` / `crate::agent` / `crate::task`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写 `sqlx::FromRow`
//!   （`mc_core::Id` 没有 sqlx impl ⇒ `derive(FromRow)` 不可用）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，**不允许静默跳过**，
//!   见 `docs/15` §8.1 的 ⑥ 门）
//!
//! 硬约束：**不引入本仓自造列**；不加迁移（`docs/42` §2：11 张表全部已在
//! `migrations/upstream/`）。
//!
//! scaffold 阶段本文件只有文档注释，避免 M4-3 / M4-4 同时编辑 `crate::lib`。
