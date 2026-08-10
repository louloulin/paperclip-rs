#![forbid(unsafe_code)]
//! `pc-issue-references` — Issue reference extraction and sync.
//!
//! 对应 Node `services/issue-references.ts` + `packages/shared/src/issue-references.ts`。
//!
//! 核心职责：
//! 1. **解析** 从 issue title / description / comment / document 中抽取 issue 标识符
//!    （形如 `PAP-123` 或 `/issues/pap-123` 链接）。
//! 2. **持久化** 把 source → target 关系写入 `issue_reference_mentions` 表。
//! 3. **同步** issue / comment 变更时重算其 mentions（事务内 delete + insert）。
//! 4. **查询** related work（出站 inbound / 入站 outbound 相关 issue 摘要）。
//!
//! 设计：
//! - 高内聚：所有 reference 业务集中在本 crate（regex 解析 + 同步 + 查询）。
//! - 低耦合：上游 issue / comment service 通过 `sync_issue` / `sync_comment` 调用。
//! - 严格分层：service → pc_repos::issue_reference_mentions → db。
//! - 幂等：`ON CONFLICT DO NOTHING` 保证重复同步不会出错。

mod extractor;
mod service;
mod types;

pub use extractor::{
    extract_identifiers, extract_matches, parse_issue_href, strip_markdown_code,
    IdentifierMatch, ISSUE_REFERENCE_IDENTIFIER_RE,
};
pub use service::{IssueReferenceError, IssueReferenceRelatedWork, IssueReferenceService};
pub use types::{
    IssueReferenceSource, IssueReferenceSourceKind, ReferenceRelatedIssueSummary,
    RelatedWorkItem, SOURCE_KIND_COMMENT, SOURCE_KIND_DESCRIPTION, SOURCE_KIND_DOCUMENT,
    SOURCE_KIND_TITLE,
};
