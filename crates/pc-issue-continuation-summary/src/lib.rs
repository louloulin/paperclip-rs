#![forbid(unsafe_code)]
//! `pc-issue-continuation-summary` — Issue continuation summary 业务服务。
//!
//! 对应 Node `services/issue-continuation-summary.ts`（284 行）。
//!
//! 本 crate 提供：
//!
//! - **常量**：
//!   - `ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY`
//!   - `ISSUE_CONTINUATION_SUMMARY_TITLE`
//!   - `ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS`
//!   - `SUMMARY_SECTION_MAX_CHARS`
//!   - `PATH_CANDIDATE_RE`（regex）
//!   - `WAITING_FOR_REVIEW_OR_APPROVAL_RE`（regex）
//! - **DTO**：`IssueSummaryInput` / `RunSummaryInput` / `AgentSummaryInput` /
//!   `IssueContinuationSummaryDocument` / `RefreshInput`
//! - **纯 markdown 函数**：
//!   - `build_continuation_summary_markdown(input)` —— 主 builder
//!   - `extract_continuation_summary_next_action(body)` —— 抽取 next action
//!   - `continuation_summary_parks_executor(body)` —— 检查是否 park
//! - **DB 集成**：
//!   - `get_continuation_summary(db, issue_id)` —— 读取
//!   - `refresh_continuation_summary(db, input)` —— 刷新（upsert）
//! - **Service 层 API**（`IssueContinuationSummaryService`）：封装 + Hook
//! - **Hook 系统**：`IssueContinuationSummaryHook` trait（4 回调）
//!
//! 设计原则：
//! - **高内聚**：所有 continuation summary 业务集中在本 crate。
//! - **低耦合**：上游 recovery / heartbeat 只需调 service。
//! - **薄封装**：markdown 走 `markdown` 模块，DB 走 `pc_repos::document` + `pc_documents::DocumentService`。

mod hook;
mod markdown;
mod service;
mod types;

pub use hook::{
    IssueContinuationSummaryHook, IssueContinuationSummaryHookEvent,
    NoopIssueContinuationSummaryHook, RecordingIssueContinuationSummaryHook,
};
pub use markdown::{
    build_continuation_summary_markdown, continuation_summary_parks_executor,
    extract_continuation_summary_next_action, extract_markdown_section,
    extract_path_candidates, infer_mode, infer_next_action, read_result_summary,
};
pub use service::{
    get_continuation_summary, refresh_continuation_summary, IssueContinuationSummaryService,
};
pub use types::{
    AgentSummaryInput, BuildContinuationSummaryInput, ContinuationSummaryMode,
    IssueContinuationSummaryDocument, IssueSummaryInput, RefreshContinuationSummaryInput,
    RunSummaryInput, ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY,
    ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS, ISSUE_CONTINUATION_SUMMARY_TITLE,
    PATH_CANDIDATE_RE, WAITING_FOR_REVIEW_OR_APPROVAL_RE,
};
