//! `decision_training` 域 —— 用户训练决策（1:1 port of Node `server/src/services/decision-training.ts`，403 行）。
//!
//! 模块拆分（按 `docs/08-RUST-MODULAR-ARCHITECTURE.md` ≥ 300 行 / ≥ 3 类职责门槛）：
//! - [`types`]    ：公开类型（`DecisionTrainingSourceKind` 枚举 + `DecisionTrainingExampleRow` 表行 + 各种 Input/Result + Snapshot v1 嵌套结构）
//! - [`commit_sha`]：纯助手（`find_commit_sha` 递归搜索 + `json_copy` 深拷贝 + `is_commit_sha` 正则校验）
//! - [`capture`]  ：snapshot 捕获（`capture_decision_snapshot` + `load_source_decision` + `build_snapshot` 工厂）
//! - [`service`]  ：仓储入口（`DecisionTrainingService` struct + 7 个 CRUD 方法 + 事务 + UPSERT）
//!
//! 设计：
//! - `mod.rs` 是唯一公共 facade
//! - 子模块默认私有；`pub(super)` 跨子模块；crate 内复用才用 `pub(crate)`
//! - HTTP 层只导入 `pc_repos::decision_training::*`，不直接接触 `capture` / `service` / `commit_sha`
//! - 所有 DB 操作返回 `sqlx::Result` 或 `RepoResult`，调用方按需转换

mod capture;
mod commit_sha;
mod service;
mod types;

// ============================================================================
// Public API
// ============================================================================

pub use capture::{
    build_snapshot, capture_decision_snapshot, load_source_decision,
    LoadedSourceDecision, DECISION_TRAINING_RETENTION_POLICY,
};
pub use commit_sha::{find_commit_sha, is_commit_sha, json_copy, COMMIT_SHA_KEYS};
pub use service::DecisionTrainingService;
pub use types::{
    CaptureInput, CaptureResult, CreateInput, DecisionTrainingExampleRow,
    DecisionTrainingSnapshotV1, DecisionTrainingSourceKind, ListExampleRow, ListInput,
    NotesHistoryEntry, ScrubDeletedCommentsInput, ScrubDeletedCommentsResult,
    SnapshotCode, SnapshotCutoff, SnapshotDecision, SnapshotRetention,
};
