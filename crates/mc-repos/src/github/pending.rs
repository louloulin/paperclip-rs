//! `github_pending_check_suite` 仓储面（乱序 `check_suite` 的暂存）。
//!
//! - **写者**：M8-4（`docs/61-M8-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/handler/github.go` —— `check_suite` 事件可能**先于** PR 行到达，
//!   此时先暂存，PR 落库后再回放（`broadcastPRSnapshotApplied` 之前的粘合）。
//! - **语义**：幂等 + 回放后清理；不得因为暂存条目永久堆积。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定。
//!
//! **状态：M8-4 待落地**（本文件由 M8-0 anchor 建为 doc-only 桩）。
