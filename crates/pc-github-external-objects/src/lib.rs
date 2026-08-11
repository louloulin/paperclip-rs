#![forbid(unsafe_code)]

//! Pure parsers + identity types for GitHub / GitHub Enterprise external objects.
//!
//! R525: Direct port of the **pure** parts of
//! `paperclip/server/src/services/github-external-object-provider.ts`.
//!
//! 范围 (本 crate):
//! - GitHub canonical URL parsing → `GitHubObjectIdentity`
//! - externalId string parsing → `GitHubObjectIdentity`
//! - identity helpers (`external_id_for`, `display_title_for`, `display_key_for`)
//! - `retry_after_seconds` from GitHub rate-limit headers
//! - `failure_from_github_response` (status code → typed error)
//!
//! **NOT** 范围 (留给后续 / 集成层):
//! - HTTP fetching (consume [`pc_github_fetch`])
//! - DB 持久化 (`external_objects` / `externalObjectMentions` 表)
//! - Plugin worker manager 集成
//! - live-events publish
//! - snapshot 构造 (依赖 DTO 类型, 留给 R526 集成层)
//!
//! 设计原则:
//! - **所有 pub fn 都是纯函数** (无 IO, 无副作用, 无 async)
//! - 错误用 [`ParseError`] 强类型, 不抛 string
//! - 类型化 enum (`ObjectType`, `PathKind`, `LivenessState`, `ErrorCode`) 取代 stringly-typed
//! - 不引入 DTO 依赖; 集成层负责把 typed error 映射到上游 `ExternalObjectResolveResult`

use thiserror::Error;

mod identity;
mod retry;
mod status;

pub use identity::{
    display_key_for, display_title_for, external_id_for, parse_github_canonical_url,
    parse_github_object, GitHubObjectIdentity, ObjectType, PathKind,
};
pub use retry::{failure_from_github_response, retry_after_seconds, ResolveFailure};
pub use status::{ErrorCode, LivenessState};

/// Errors that can arise while parsing a GitHub external-object reference.
///
/// All variants are recoverable — callers should fall back to a
/// "not_found" snapshot rather than aborting.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("canonical URL is not https: {0}")]
    NotHttps(String),
    #[error("canonical URL host is not GitHub: {0}")]
    NotGitHubHost(String),
    #[error(
        "canonical URL path has wrong arity (expected owner/repo/kind/number, got {0} segments)"
    )]
    WrongPathArity(usize),
    #[error("canonical URL kind is not 'pull' or 'issues': {0}")]
    WrongKind(String),
    #[error("canonical URL number is not a positive integer: {0}")]
    InvalidNumber(String),
    #[error("external ID does not match GitHub format owner/repo#kind/number: {0}")]
    BadExternalId(String),
    #[error("sanitized canonical URL is not a valid URL: {0}")]
    BadCanonicalUrl(String),
}
