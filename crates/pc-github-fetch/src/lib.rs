#![forbid(unsafe_code)]

//! GitHub / GitHub Enterprise fetch wrapper + URL builders.
//!
//! R523: Direct port of `paperclip/server/src/services/github-fetch.ts`.
//!
//! Design principles:
//! - **Pure URL builders** in [`urls`] (no IO, easy to unit-test)
//! - **ghFetch async wrapper** consumes a `reqwest::Client` (caller-owned; lets
//!   higher-level code share a single connection pool)
//! - **Distinguishes github.com vs GitHub Enterprise** via hostname inspection
//!   — `api.github.com` for dotcom, `${host}/api/v3` for GHE
//! - **No retry / no auth** in this module — the caller wires its own token via
//!   `reqwest::RequestBuilder::bearer_auth()` etc. before passing the request
//!   in (see [`fetch::gh_fetch`])
//!
//! 与 Node 上游 [`github-fetch.ts`] 的差异：
//! - Rust 端把 ghFetch 设计成 [`gh_fetch_with`] 接受 caller-supplied client，
//!   避免每次调用都新建 reqwest::Client (Node `fetch` 全局复用即可)
//! - Rust 端不抛 unprocessable (HTTP 422) — 改用 [`GitHubFetchError`] 强类型
//!   错误，调用方决定如何映射

use thiserror::Error;

pub mod fetch;
pub mod urls;

pub use fetch::{gh_fetch, gh_fetch_with};
pub use urls::{git_hub_api_base, is_git_hub_dot_com, resolve_raw_git_hub_url};

/// Errors that can arise when fetching from a GitHub or GitHub Enterprise
/// instance. Mirrors the upstream `unprocessable(...)` failure mode but
/// surfaces it as a typed error so callers can decide whether to map it
/// to HTTP 422, log + retry, etc.
#[derive(Debug, Error)]
pub enum GitHubFetchError {
    #[error("could not connect to {host} — ensure the URL points to a GitHub or GitHub Enterprise instance")]
    Connection { host: String, source: reqwest::Error },
    #[error("HTTP {status} from GitHub: {body}")]
    Http { status: u16, body: String },
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn re_exports_are_consistent() {
        // Re-exports point at the same items.
        let _: fn(&str) -> bool = is_git_hub_dot_com;
        let _: fn(&str) -> String = git_hub_api_base;
    }
}
