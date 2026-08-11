//! Issue 业务子模块（原 `pc-issue-continuation-summary` 已下沉到 `pc-issues::continuation_summary`）。
//!
//! 对应 Node `server/src/services/issue-continuation_summary.ts`。

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
    extract_continuation_summary_next_action, extract_markdown_section, extract_path_candidates,
    infer_mode, infer_next_action, read_result_summary,
};
pub use service::{
    get_continuation_summary, refresh_continuation_summary, IssueContinuationSummaryService,
};
pub use types::{
    AgentSummaryInput, BuildContinuationSummaryInput, ContinuationSummaryMode,
    IssueContinuationSummaryDocument, IssueSummaryInput, RefreshContinuationSummaryInput,
    RunSummaryInput, ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY,
    ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS, ISSUE_CONTINUATION_SUMMARY_TITLE, PATH_CANDIDATE_RE,
    WAITING_FOR_REVIEW_OR_APPROVAL_RE,
};
