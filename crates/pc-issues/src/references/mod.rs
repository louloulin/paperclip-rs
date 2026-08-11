//! Issue 业务子模块（原 `pc-issue-references` 已下沉到 `pc-issues::references`）。
//!
//! 对应 Node `server/src/services/issue-references.ts`。

mod extractor;
mod service;
mod types;

pub use extractor::{
    extract_identifiers, extract_matches, parse_issue_href, strip_markdown_code, IdentifierMatch,
    ISSUE_REFERENCE_IDENTIFIER_RE,
};
pub use service::{IssueReferenceError, IssueReferenceRelatedWork, IssueReferenceService};
pub use types::{
    IssueReferenceSource, IssueReferenceSourceKind, ReferenceRelatedIssueSummary, RelatedWorkItem,
    SOURCE_KIND_COMMENT, SOURCE_KIND_DESCRIPTION, SOURCE_KIND_DOCUMENT, SOURCE_KIND_TITLE,
};
