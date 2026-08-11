#![forbid(unsafe_code)]
//! `pc-run-liveness` — Run liveness classification 业务服务。
//!
//! 对应 Node `services/run-liveness.ts`（368 行 — 核心 liveness 分类器）。
//!
//! 本 crate 提供：
//!
//! - **类型**：`RunLivenessState` / `RunLivenessActionability` /
//!   `RunLivenessIssueInput` / `RunLivenessEvidenceInput` /
//!   `RunLivenessClassificationInput` / `RunLivenessClassification`
//! - **纯函数 classifier**：
//!   - `has_useful_output(input)` —— 是否有有用输出
//!   - `declared_blocker(input)` —— 是否声明 blocker
//!   - `looks_like_planning_only(input)` —— 是否仅规划
//!   - `is_planning_or_document_task(issue)` —— 是否规划/文档任务
//!   - `has_concrete_action_evidence(evidence)` —— 是否有具体动作证据
//!   - `classify_run_actionability(input)` —— 行动性分类
//!   - `classify_run_liveness(input)` —— 主分类（返回 RunLivenessClassification）
//! - **Service 层 API**（`RunLivenessService`）：封装 + Hook
//! - **Hook 系统**：`RunLivenessHook` trait（2 回调）
//!
//! 设计原则：
//! - **高内聚**：所有 liveness 分类逻辑集中在本 crate。
//! - **低耦合**：上游 heartbeat / recovery 只需调用 classifier。
//! - **纯函数**：无 DB I/O，易测试。

mod classifier;
mod hook;
mod service;
mod types;

pub use classifier::{
    classify_run_actionability, classify_run_liveness, declared_blocker,
    has_concrete_action_evidence, has_useful_output, is_planning_or_document_task,
    looks_like_planning_only,
};
pub use hook::{
    NoopRunLivenessHook, RecordingRunLivenessHook, RunLivenessHook, RunLivenessHookEvent,
};
pub use service::RunLivenessService;
pub use types::{
    RunLivenessActionability, RunLivenessClassification, RunLivenessClassificationInput,
    RunLivenessEvidenceInput, RunLivenessIssueInput, RunLivenessState,
    UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON, UNMANAGED_BACKGROUND_TASK_STOP_REASON,
};
