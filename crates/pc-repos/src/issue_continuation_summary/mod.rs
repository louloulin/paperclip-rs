//! Issue continuation summary 域（1:1 port of Node
//! `server/src/services/issue-continuation-summary.ts`，284 行）。
//!
//! 单一职责：构造并持久化 issue 的 "continuation summary" markdown 文档，
//! 用于给下一次 heartbeat run 提供前情上下文。
//!
//! ## 模块结构（mod/ 拆分，遵循 docs/08-RUST-MODULAR-ARCHITECTURE.md）
//!
//! ```text
//! issue_continuation_summary/
//! ├── mod.rs       # facade, pub use 重导出
//! ├── types.rs     # 输入输出类型 + 常量（文档键 / 字符上限）
//! ├── markdown.rs  # 纯逻辑：truncate / extract / infer / builder
//! ├── queries.rs   # DB IO：load + refresh
//! └── （测试内联于 markdown.rs）
//! ```

mod markdown;
mod queries;
mod types;

// Public facade: 重导出稳定 API
pub use markdown::{
    as_non_empty_string, build_continuation_summary_markdown, bullet_list,
    continuation_summary_parks_executor, extract_continuation_summary_next_action,
    extract_markdown_section, extract_path_candidates, extract_previous_next_action, infer_mode,
    infer_next_action, read_result_summary, truncate_text, SummaryMode,
};
pub use queries::{
    load_issue_summary_with_doc, refresh_issue_continuation_summary, RefreshSummaryInput,
};
pub use types::{
    AgentSummaryInput, BuildSummaryInput, IssueContinuationSummaryDocument, IssueSummaryInput,
    RunSummaryInput, ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY,
    ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS, ISSUE_CONTINUATION_SUMMARY_TITLE,
    SUMMARY_SECTION_MAX_CHARS,
};
