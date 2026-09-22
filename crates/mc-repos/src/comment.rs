//! M2 anchor scaffold（LUM-1347 / M1-D 预扩展）：comment 仓储。
//!
//! 由 M2-B（LUM-1349，feat/multica-rs-m2b-comment） 填充：`CommentRow` / `NewComment` / `CommentFilter` + 评论 reaction 读写（表见迁移 0004）。
//!
//! 约定与 M1 各 Repo 保持一致（见 `crate::invitation` / `crate::share_link`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写
//!   `sqlx::FromRow`（`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`）
//!
//! scaffold 阶段只占位，避免三个 M2 分支同时编辑 `crate::lib`。
