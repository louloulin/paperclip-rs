//! M4 anchor scaffold（LUM-1470）：`chat_history` 仓储 —— **占位，无实现**。
//!
//! 归属：M4-4（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。填充内容 = `/api/chat/history` 与
//! `/api/chat/thread`（上游 `router.go` #24–#25，handler `chat_history.go` 418 行）的
//! **非渠道分支**读取面。
//!
//! ⚠️ 范围硬边界（`docs/42` §4.3 第 3 条）：上游这两个端点用 `X-Task-ID` 头 +
//! `GetChannelChatSessionBindingBySessionAny` + `channel.HistoryOptions`（slack/lark 分支）。
//! **本波只落「无渠道绑定」分支**（照上游 `writeNoChannelIntegration` 的响应），渠道分支随
//! M7 补齐并在 `docs/43` 的 `known_gap` 里显式登记。⇒ 本文件**不得**引入渠道相关表/列，
//! 也不要为渠道分支预留自造列。
//!
//! 本模块主要读 M3 域的表（`agent_task_queue` / `task_message`）与 `chat_session`，
//! 一律**只读**；查询面见上游 `server/pkg/db/queries/chat.sql` 的 history/thread 部分。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::task` / `crate::chat_session`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写 `sqlx::FromRow`
//!   （`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，不允许静默跳过）
//!
//! scaffold 阶段本文件只有文档注释，避免 M4-3 / M4-4 同时编辑 `crate::lib`。
