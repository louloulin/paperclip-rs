#![forbid(unsafe_code)]

//! GitHub external-object provider (detector + resolvers).
//!
//! R767: Direct port of the **wire-level** parts of
//! `paperclip/server/src/services/github-external-object-provider.ts` (445 LOC).
//!
//! The pure parser / identity / failure-classification helpers were already
//! landed in R525 (`crate::identity`, `crate::retry`, `crate::status`).
//! This module adds the remaining moving parts that R525 explicitly
//! excluded:
//!
//! - **Detector** — turns a list of canonical URLs into
//!   [`GitHubExternalObjectDetection`]s with stable external IDs.
//! - **Resolvers** — fetch a single GitHub PR / issue and turn it into a
//!   [`GitHubExternalObjectSnapshot`] (or a typed failure).
//! - **`create_github_external_object_provider` factory** — bundles
//!   detector + resolvers, exactly like the Node upstream.
//!
//! Design choices:
//!
//! - The crate **does not** depend on `reqwest`; HTTP I/O is injected via the
//!   [`GitHubFetcher`] trait. The integration layer (`pc-server` etc.) supplies
//!   a `reqwest::Client`-backed adapter; tests provide a closure-shaped mock.
//! - Errors never use strings; failures are the typed
//!   [`crate::retry::ResolveFailure`] (liveness + error_code + retry_after).
//! - Snapshots are strongly typed (no `data: Record<string, unknown>` blob).
//! - All pub items carry the `R767_` test prefix so a quick
//!   `cargo test -p pc-github-external-objects r767_` filters to just this work.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::identity::{
    display_key_for, display_title_for, external_id_for, parse_github_canonical_url,
    parse_github_object, GitHubObjectIdentity, ObjectType,
};
use crate::retry::{failure_from_github_response, retry_after_seconds, ResolveFailure, RetryAfterResponse};
use crate::status::{ErrorCode, LivenessState};

// ---------------------------------------------------------------------------
// Public constants — mirror the Node module's top-level `const` definitions.
// ---------------------------------------------------------------------------

/// Default ordered list of secret names searched for the GitHub API token.
pub const DEFAULT_GITHUB_TOKEN_SECRET_NAMES: &[&str] =
    &["GITHUB_TOKEN", "GH_TOKEN", "PAPERCLIP_GITHUB_TOKEN"];

/// TTL applied to every successful snapshot and to fallback retry-after values
/// (matches Node upstream `GITHUB_OBJECT_TTL_SECONDS = 300`).
pub const GITHUB_OBJECT_TTL_SECONDS: u64 = 300;

// ---------------------------------------------------------------------------
// Public types: detector output
// ---------------------------------------------------------------------------

/// Confidence score carried by a [`GitHubExternalObjectDetection`].
///
/// Mirrors the Node `ExternalObjectMentionConfidence` union (we only ever
/// produce `"exact"`; lower tiers are reserved for future code paths).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetectionConfidence {
    Exact,
}

/// A single external-object detection emitted by the GitHub detector.
///
/// 1:1 with the `detector.detect(...)` return shape in the Node upstream,
/// minus fields (`detectorKey`, `providerKey`, `pluginId`) that are
/// trivially derivable.
#[derive(Debug, Clone)]
pub struct GitHubExternalObjectDetection {
    /// The canonical URL this detection was extracted from.
    pub canonical_index: usize,
    /// Object type (PR vs issue) — drives display + icon + resolver routing.
    pub object_type: ObjectType,
    /// Stable DB-friendly externalId (`owner/repo#pull|issues/n`).
    pub external_id: String,
    /// DisplayKey e.g. `"GitHub Pull Request"`.
    pub display_key: &'static str,
    /// IconKey — always `"github"` for this detector.
    pub icon_key: &'static str,
    /// Display title — `<owner>/<repo>#<number>`.
    pub display_title: String,
    /// Detection confidence (always `Exact` for the regex-based GitHub detector).
    pub confidence: DetectionConfidence,
}

// ---------------------------------------------------------------------------
// Public types: snapshot + result
// ---------------------------------------------------------------------------

/// Icon key bundled into [`GitHubExternalObjectSnapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnapshotIconKey {
    Github,
    GitMerge,
    GitPullRequest,
    XCircle,
    Clock,
    Circle,
    CircleDot,
    Archive,
}

impl SnapshotIconKey {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::GitMerge => "git-merge",
            Self::GitPullRequest => "git-pull-request",
            Self::XCircle => "x-circle",
            Self::Clock => "clock",
            Self::Circle => "circle",
            Self::CircleDot => "circle-dot",
            Self::Archive => "archive",
        }
    }
}

/// UI status category — drives badge colour / placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusCategory {
    Open,
    Closed,
    Succeeded,
    Archived,
    Waiting,
    Unknown,
}

impl StatusCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Succeeded => "succeeded",
            Self::Archived => "archived",
            Self::Waiting => "waiting",
            Self::Unknown => "unknown",
        }
    }
}

/// UI status tone — secondary visual cue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusTone {
    Info,
    Success,
    Warning,
    Muted,
    Neutral,
}

impl StatusTone {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Muted => "muted",
            Self::Neutral => "neutral",
        }
    }
}

/// Typed snapshot returned by a successful resolve.
///
/// Mirrors Node `ExternalObjectResolverSnapshot` with two simplifications:
/// `data` is not a free-form `Record<string, unknown>` blob but a typed
/// [`SnapshotData`] enum covering the variants we actually emit.
#[derive(Debug, Clone, PartialEq)]
pub struct GitHubExternalObjectSnapshot {
    pub display_key: &'static str,
    pub icon_key: SnapshotIconKey,
    pub display_title: String,
    pub status_key: String,
    pub status_label: String,
    pub status_icon_key: SnapshotIconKey,
    pub status_category: StatusCategory,
    pub status_tone: StatusTone,
    pub is_terminal: bool,
    pub remote_version: Option<String>,
    pub etag: Option<String>,
    pub ttl_seconds: u64,
    pub data: SnapshotData,
}

/// Provider-specific data payload — typed instead of `serde_json::Value` so
/// consumers can pattern-match without parsing strings.
#[derive(Debug, Clone, PartialEq)]
pub enum SnapshotData {
    /// GitHub returned 404 for this identity.
    NotFound {
        provider: &'static str,
        owner: String,
        repo: String,
        number: u64,
    },
    /// Resolved pull-request state.
    PullRequest(PullRequestData),
    /// Resolved issue state.
    Issue(IssueData),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PullRequestData {
    pub provider: &'static str,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub state: String,
    pub merged: bool,
    pub draft: bool,
    pub author_login: Option<String>,
    pub head_ref: Option<String>,
    pub base_ref: Option<String>,
    pub review_decision: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueData {
    pub provider: &'static str,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub state: String,
    pub state_reason: Option<String>,
    pub author_login: Option<String>,
}

/// Successful or failed resolve result.
pub type GitHubExternalObjectResolveResult = Result<GitHubExternalObjectSnapshot, ResolveFailure>;

// ---------------------------------------------------------------------------
// Fetcher + TokenProvider abstractions
// ---------------------------------------------------------------------------

/// Reason a [`GitHubFetcher::fetch`] call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// Could not connect / DNS / TLS / timeout — Node upstream maps this to
    /// `github_fetch_failed` (liveness = Unreachable).
    Transport(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(s) => write!(f, "transport error: {s}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// A successful subset of the GitHub HTTP response.
///
/// The provider only needs the fields it inspects in Node; any mapping from
/// `reqwest::Response` to this struct happens at the integration boundary.
#[derive(Debug, Clone, Default)]
pub struct GitHubFetchResult {
    pub status: u16,
    pub etag: Option<String>,
    pub retry_after: Option<String>,
    pub x_ratelimit_reset: Option<String>,
    pub x_ratelimit_remaining: Option<String>,
    /// Parsed JSON body — `None` if the response had no body or failed to parse.
    pub body: Option<Value>,
}

impl GitHubFetchResult {
    /// Reduce the result down to the headers [`crate::retry`] cares about.
    #[must_use]
    pub fn retry_headers(&self) -> RetryAfterResponse {
        RetryAfterResponse::new(self.retry_after.as_deref(), self.x_ratelimit_reset.as_deref())
    }
}

/// Boxed future returned by [`GitHubFetcher::fetch`].
pub type FetchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<GitHubFetchResult, FetchError>> + Send + 'a>>;

/// Pluggable HTTP fetcher. The integration layer supplies a `reqwest`-backed
/// impl; tests supply a closure-shaped mock.
pub trait GitHubFetcher: Send + Sync + 'static {
    fn fetch(&self, url: &str, headers: HashMap<String, String>) -> FetchFuture<'_>;
}

/// Boxed future for [`GitHubTokenProvider::token_for`].
pub type TokenFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<String>, FetchError>> + Send + 'a>>;

/// Returns the GitHub API token for a given company, or `None` if none is
/// configured. Mirrors the Node `tokenProvider` callback shape.
pub trait GitHubTokenProvider: Send + Sync + 'static {
    fn token_for(&self, company_id: &str) -> TokenFuture<'_>;
}

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Detects GitHub external objects in a list of canonical URLs.
///
/// Mirrors the Node `detector: ExternalObjectDetector` returned by
/// `createGitHubExternalObjectProvider`.
#[derive(Debug, Clone, Copy)]
pub struct GitHubExternalObjectDetector;

impl GitHubExternalObjectDetector {
    #[must_use]
    pub const fn key(&self) -> &'static str {
        "github"
    }

    /// Detect all GitHub external objects in `urls`.
    ///
    /// `canonical_urls` is treated as opaque (only `scheme + host + path`
    /// are read); the caller decides which URLs to canonicalize first.
    pub fn detect(&self, canonical_urls: &[SimpleCanonicalUrl]) -> Vec<GitHubExternalObjectDetection> {
        let mut out = Vec::new();
        for (idx, url) in canonical_urls.iter().enumerate() {
            let Ok(id) = parse_github_canonical_url(&url.scheme, &url.host, &url.path) else {
                continue;
            };
            out.push(GitHubExternalObjectDetection {
                canonical_index: idx,
                object_type: id.object_type,
                external_id: external_id_for(&id),
                display_key: display_key_for(&id),
                icon_key: "github",
                display_title: display_title_for(&id),
                confidence: DetectionConfidence::Exact,
            });
        }
        out
    }
}

/// Minimal canonical-URL view (only the fields the detector needs).
///
/// Mirrors the relevant subset of [`crate::identity::parse_github_canonical_url`]'s
/// inputs. The integration layer is expected to construct these from the
/// DTO returned by `pc-external-objects-server::canonicalize_external_object_url`.
#[derive(Debug, Clone)]
pub struct SimpleCanonicalUrl {
    pub scheme: String,
    pub host: String,
    pub path: String,
}

impl SimpleCanonicalUrl {
    #[must_use]
    pub fn new(scheme: impl Into<String>, host: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            scheme: scheme.into(),
            host: host.into(),
            path: path.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// A stored GitHub external object — the `object` parameter passed by the
/// upstream ExternalObjectResolver interface.
///
/// Mirrors the Node `ExternalObjectRecord` shape that the resolver receives:
/// we only care about the two identity-bearing fields.
#[derive(Debug, Clone)]
pub struct GitHubExternalObjectRecord {
    pub external_id: String,
    pub sanitized_canonical_url: Option<String>,
}

/// One of the two resolvers returned by
/// [`create_github_external_object_provider`].
pub struct GitHubExternalObjectResolver {
    object_type: ObjectType,
    fetcher: Arc<dyn GitHubFetcher>,
    token_provider: Arc<dyn GitHubTokenProvider>,
}

impl GitHubExternalObjectResolver {
    #[must_use]
    pub const fn provider_key(&self) -> &'static str {
        "github"
    }

    #[must_use]
    pub const fn object_type(&self) -> ObjectType {
        self.object_type
    }

    /// Resolve a stored external object to a fresh snapshot (or typed error).
    pub async fn resolve(
        &self,
        company_id: &str,
        object: &GitHubExternalObjectRecord,
    ) -> GitHubExternalObjectResolveResult {
        // 1. Verify the stored identity actually matches this resolver's
        //    object type.
        let identity = match parse_github_object(
            &object.external_id,
            object.sanitized_canonical_url.as_deref(),
        ) {
            Ok(id) if id.object_type == self.object_type => id,
            Ok(_) | Err(_) => {
                return Err(ResolveFailure {
                    liveness: LivenessState::Unreachable,
                    error_code: ErrorCode::GithubUnreachable,
                    error_message: "GitHub object identity is invalid.".to_string(),
                    retry_after_seconds: GITHUB_OBJECT_TTL_SECONDS,
                });
            }
        };

        // 2. Resolve the auth token (may throw — Node uses try/catch).
        let raw_token = match self.token_provider.token_for(company_id).await {
            Ok(t) => t,
            Err(_) => {
                return Err(ResolveFailure {
                    liveness: LivenessState::AuthRequired,
                    error_code: ErrorCode::GithubAuthRequired,
                    error_message:
                        "Configured GitHub credentials could not be resolved.".to_string(),
                    retry_after_seconds: GITHUB_OBJECT_TTL_SECONDS,
                });
            }
        };
        let token = raw_token.and_then(|t| {
            let trimmed = t.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        // 3. Build headers + URL.
        let mut headers: HashMap<String, String> = HashMap::new();
        headers.insert("accept".to_string(), "application/vnd.github+json".to_string());
        headers.insert(
            "user-agent".to_string(),
            "paperclip-external-object-resolver".to_string(),
        );
        headers.insert("x-github-api-version".to_string(), "2022-11-28".to_string());
        if let Some(ref t) = token {
            headers.insert("authorization".to_string(), format!("Bearer {t}"));
        }

        let api_kind = match self.object_type {
            ObjectType::PullRequest => "pulls",
            ObjectType::Issue => "issues",
        };
        let url = format!(
            "{}/repos/{}/{}/{}/{}",
            pc_github_fetch::git_hub_api_base(&identity.host),
            urlencode(&identity.owner),
            urlencode(&identity.repo),
            api_kind,
            identity.number
        );

        // 4. Fetch (network errors → unreachable).
        let fetch_result = match self.fetcher.fetch(&url, headers).await {
            Ok(r) => r,
            Err(_) => {
                return Err(ResolveFailure {
                    liveness: LivenessState::Unreachable,
                    error_code: ErrorCode::GithubUnreachable,
                    error_message: "GitHub could not be reached while refreshing this object."
                        .to_string(),
                    retry_after_seconds: GITHUB_OBJECT_TTL_SECONDS,
                });
            }
        };

        // 5. Status-code dispatch (Node: failureFromGitHubResponse).
        if fetch_result.status == 404 {
            return Ok(not_found_snapshot(&identity, fetch_result.etag));
        }
        if let Some(fail) = failure_from_github_response(
            fetch_result.status,
            fetch_result.x_ratelimit_remaining.as_deref(),
            &fetch_result.retry_headers(),
        ) {
            return Err(fail);
        }
        if !(200..300).contains(&fetch_result.status) {
            let retry = retry_after_seconds(&fetch_result.retry_headers());
            return Err(ResolveFailure {
                liveness: LivenessState::Unreachable,
                error_code: ErrorCode::GithubUnreachable,
                error_message: format!(
                    "GitHub returned HTTP {} while refreshing this object.",
                    fetch_result.status
                ),
                retry_after_seconds: retry,
            });
        }

        // 6. Body must be a JSON object.
        let Some(body) = fetch_result.body else {
            return Err(ResolveFailure {
                liveness: LivenessState::Unreachable,
                error_code: ErrorCode::GithubUnreachable,
                error_message: "GitHub returned an invalid object response.".to_string(),
                retry_after_seconds: GITHUB_OBJECT_TTL_SECONDS,
            });
        };
        let Some(body_obj) = body.as_object() else {
            return Err(ResolveFailure {
                liveness: LivenessState::Unreachable,
                error_code: ErrorCode::GithubUnreachable,
                error_message: "GitHub returned an invalid object response.".to_string(),
                retry_after_seconds: GITHUB_OBJECT_TTL_SECONDS,
            });
        };
        let body_map: HashMap<String, Value> = body_obj
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // 7. Dispatch by object type.
        Ok(match self.object_type {
            ObjectType::PullRequest => pull_request_snapshot(&identity, &body_map, fetch_result.etag),
            ObjectType::Issue => issue_snapshot(&identity, &body_map, fetch_result.etag),
        })
    }
}

// ---------------------------------------------------------------------------
// Public factory
// ---------------------------------------------------------------------------

/// Provider bundle — Node returns `{ detector, resolvers: [...] }`; we expose
/// them via methods so callers always go through the factory.
pub struct GitHubExternalObjectProvider {
    fetcher: Arc<dyn GitHubFetcher>,
    token_provider: Arc<dyn GitHubTokenProvider>,
}

impl GitHubExternalObjectProvider {
    /// Construct a new provider. `fetcher` is mandatory (HTTP), `token_provider`
    /// is mandatory (auth) — matching Node's "always default if absent"
    /// semantics without coupling this crate to the DB secrets layer.
    pub fn new<F, T>(fetcher: F, token_provider: T) -> Self
    where
        F: GitHubFetcher,
        T: GitHubTokenProvider,
    {
        Self {
            fetcher: Arc::new(fetcher),
            token_provider: Arc::new(token_provider),
        }
    }

    #[must_use]
    pub fn detector(&self) -> GitHubExternalObjectDetector {
        GitHubExternalObjectDetector
    }

    /// Both resolvers (PR + issue) — order matches Node upstream.
    #[must_use]
    pub fn resolvers(&self) -> [GitHubExternalObjectResolver; 2] {
        [
            GitHubExternalObjectResolver {
                object_type: ObjectType::PullRequest,
                fetcher: Arc::clone(&self.fetcher),
                token_provider: Arc::clone(&self.token_provider),
            },
            GitHubExternalObjectResolver {
                object_type: ObjectType::Issue,
                fetcher: Arc::clone(&self.fetcher),
                token_provider: Arc::clone(&self.token_provider),
            },
        ]
    }
}

/// Convenience factory — Node's `createGitHubExternalObjectProvider(db, opts)`.
#[must_use]
pub fn create_github_external_object_provider<F, T>(
    fetcher: F,
    token_provider: T,
) -> GitHubExternalObjectProvider
where
    F: GitHubFetcher,
    T: GitHubTokenProvider,
{
    GitHubExternalObjectProvider::new(fetcher, token_provider)
}

// ---------------------------------------------------------------------------
// Internal snapshot helpers — pure, easy to unit-test.
// ---------------------------------------------------------------------------

/// Percent-encode owner/repo path segments — mirrors Node `encodeURIComponent`
/// for the special characters allowed by the regex (we don't need full RFC 3986
/// here because the GitHub API tolerates the same set).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf);
            for byte in encoded.as_bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

fn as_string(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string)
}

fn as_bool(v: Option<&Value>) -> Option<bool> {
    v.and_then(Value::as_bool)
}

fn as_nested_string(m: &HashMap<String, Value>, key: &str, nested_key: &str) -> Option<String> {
    m.get(key)
        .and_then(|v| v.as_object())
        .and_then(|o| o.get(nested_key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn not_found_snapshot(
    identity: &GitHubObjectIdentity,
    etag: Option<String>,
) -> GitHubExternalObjectSnapshot {
    GitHubExternalObjectSnapshot {
        display_key: display_key_for(identity),
        icon_key: SnapshotIconKey::Github,
        display_title: display_title_for(identity),
        status_key: "not_found".to_string(),
        status_label: "Not found".to_string(),
        status_icon_key: SnapshotIconKey::Archive,
        status_category: StatusCategory::Archived,
        status_tone: StatusTone::Muted,
        is_terminal: true,
        remote_version: None,
        etag,
        ttl_seconds: GITHUB_OBJECT_TTL_SECONDS,
        data: SnapshotData::NotFound {
            provider: "github",
            owner: identity.owner.clone(),
            repo: identity.repo.clone(),
            number: identity.number,
        },
    }
}

fn pull_request_snapshot(
    identity: &GitHubObjectIdentity,
    body: &HashMap<String, Value>,
    etag: Option<String>,
) -> GitHubExternalObjectSnapshot {
    let state = as_string(body.get("state")).unwrap_or_else(|| "unknown".to_string());
    let draft = as_bool(body.get("draft")).unwrap_or(false);
    let merged_at = as_string(body.get("merged_at"));
    let merged_body = as_bool(body.get("merged")).unwrap_or(false);
    let merged = merged_body || merged_at.is_some();

    let author_login = as_nested_string(body, "user", "login");
    let head_ref = as_nested_string(body, "head", "ref");
    let base_ref = as_nested_string(body, "base", "ref");
    let review_decision = as_string(body.get("review_decision"));

    let (status_key, status_label, status_icon_key, status_category, status_tone, is_terminal) =
        if merged {
            (
                "merged".to_string(),
                "Merged".to_string(),
                SnapshotIconKey::GitMerge,
                StatusCategory::Succeeded,
                StatusTone::Success,
                true,
            )
        } else if state == "closed" {
            (
                "closed".to_string(),
                "Closed".to_string(),
                SnapshotIconKey::XCircle,
                StatusCategory::Closed,
                StatusTone::Muted,
                true,
            )
        } else if draft {
            (
                "draft".to_string(),
                "Draft".to_string(),
                SnapshotIconKey::Clock,
                StatusCategory::Waiting,
                StatusTone::Warning,
                false,
            )
        } else {
            let (label, cat, tone) = if state == "open" {
                ("Open", StatusCategory::Open, StatusTone::Info)
            } else {
                ("Unknown", StatusCategory::Unknown, StatusTone::Neutral)
            };
            (state.clone(), label.to_string(), SnapshotIconKey::GitPullRequest, cat, tone, false)
        };

    let title = as_string(body.get("title"));
    let base_title = display_title_for(identity);
    let display_title = match title {
        Some(t) if !t.is_empty() => format!("{base_title}: {t}"),
        _ => base_title,
    };

    GitHubExternalObjectSnapshot {
        display_key: display_key_for(identity),
        icon_key: SnapshotIconKey::Github,
        display_title,
        status_key,
        status_label,
        status_icon_key,
        status_category,
        status_tone,
        is_terminal,
        remote_version: as_string(body.get("updated_at")),
        etag,
        ttl_seconds: GITHUB_OBJECT_TTL_SECONDS,
        data: SnapshotData::PullRequest(PullRequestData {
            provider: "github",
            owner: identity.owner.clone(),
            repo: identity.repo.clone(),
            number: identity.number,
            state,
            merged,
            draft,
            author_login,
            head_ref,
            base_ref,
            review_decision,
        }),
    }
}

fn issue_snapshot(
    identity: &GitHubObjectIdentity,
    body: &HashMap<String, Value>,
    etag: Option<String>,
) -> GitHubExternalObjectSnapshot {
    let state = as_string(body.get("state")).unwrap_or_else(|| "unknown".to_string());
    let state_reason = as_string(body.get("state_reason"));
    let author_login = as_nested_string(body, "user", "login");

    let status_key = if state == "closed" {
        state_reason
            .as_deref()
            .map(|r| format!("closed_{r}"))
            .unwrap_or_else(|| "closed".to_string())
    } else {
        state.clone()
    };
    let status_label = match state.as_str() {
        "closed" => state_reason
            .as_deref()
            .map(|r| format!("Closed: {}", r.replace('_', " ")))
            .unwrap_or_else(|| "Closed".to_string()),
        "open" => "Open".to_string(),
        _ => "Unknown".to_string(),
    };
    let status_icon_key = if state == "closed" {
        SnapshotIconKey::Circle
    } else {
        SnapshotIconKey::CircleDot
    };
    let (status_category, status_tone) = match state.as_str() {
        "open" => (StatusCategory::Open, StatusTone::Info),
        "closed" => (StatusCategory::Closed, StatusTone::Muted),
        _ => (StatusCategory::Unknown, StatusTone::Neutral),
    };
    let is_terminal = state == "closed";

    let title = as_string(body.get("title"));
    let base_title = display_title_for(identity);
    let display_title = match title {
        Some(t) if !t.is_empty() => format!("{base_title}: {t}"),
        _ => base_title,
    };

    GitHubExternalObjectSnapshot {
        display_key: display_key_for(identity),
        icon_key: SnapshotIconKey::Github,
        display_title,
        status_key,
        status_label,
        status_icon_key,
        status_category,
        status_tone,
        is_terminal,
        remote_version: as_string(body.get("updated_at")),
        etag,
        ttl_seconds: GITHUB_OBJECT_TTL_SECONDS,
        data: SnapshotData::Issue(IssueData {
            provider: "github",
            owner: identity.owner.clone(),
            repo: identity.repo.clone(),
            number: identity.number,
            state,
            state_reason,
            author_login,
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc as StdArc;

    // ----- Test fixtures -----

    /// Mock fetcher — closure-driven, captures last URL + headers.
    #[derive(Clone)]
    struct MockFetcher {
        response: StdArc<Mutex<Result<GitHubFetchResult, ()>>>,
        last_url: StdArc<Mutex<Option<String>>>,
        last_headers: StdArc<Mutex<Option<HashMap<String, String>>>>,
    }

    impl MockFetcher {
        fn new(response: Result<GitHubFetchResult, ()>) -> Self {
            Self {
                response: StdArc::new(Mutex::new(response)),
                last_url: StdArc::new(Mutex::new(None)),
                last_headers: StdArc::new(Mutex::new(None)),
            }
        }
    }

    impl GitHubFetcher for MockFetcher {
        fn fetch(&self, url: &str, headers: HashMap<String, String>) -> FetchFuture<'_> {
            *self.last_url.lock() = Some(url.to_string());
            *self.last_headers.lock() = Some(headers);
            let resp = self.response.lock().clone();
            Box::pin(async move {
                resp.map_err(|()| FetchError::Transport("mocked".into()))
            })
        }
    }

    /// Mock token provider — returns a fixed token (or None).
    struct MockTokenProvider(Option<String>);

    impl GitHubTokenProvider for MockTokenProvider {
        fn token_for(&self, _company_id: &str) -> TokenFuture<'_> {
            let t = self.0.clone();
            Box::pin(async move { Ok(t) })
        }
    }

    struct MockTokenProviderError;

    impl GitHubTokenProvider for MockTokenProviderError {
        fn token_for(&self, _company_id: &str) -> TokenFuture<'_> {
            Box::pin(async move { Err(FetchError::Transport("db failure".into())) })
        }
    }

    fn identity_pr() -> GitHubObjectIdentity {
        GitHubObjectIdentity {
            host: "github.com".into(),
            owner: "acme".into(),
            repo: "app".into(),
            number: 12,
            path_kind: crate::identity::PathKind::Pull,
            object_type: ObjectType::PullRequest,
        }
    }

    fn identity_issue() -> GitHubObjectIdentity {
        GitHubObjectIdentity {
            host: "github.com".into(),
            owner: "acme".into(),
            repo: "app".into(),
            number: 34,
            path_kind: crate::identity::PathKind::Issues,
            object_type: ObjectType::Issue,
        }
    }

    fn pr_record() -> GitHubExternalObjectRecord {
        GitHubExternalObjectRecord {
            external_id: "acme/app#pull/12".into(),
            sanitized_canonical_url: Some("https://github.com/acme/app/pull/12".into()),
        }
    }

    fn issue_record() -> GitHubExternalObjectRecord {
        GitHubExternalObjectRecord {
            external_id: "acme/app#issues/34".into(),
            sanitized_canonical_url: Some("https://github.com/acme/app/issues/34".into()),
        }
    }

    fn json_body(pairs: &[(&str, Value)]) -> Value {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), v.clone());
        }
        Value::Object(m)
    }

    // ----- Detector tests -----

    #[test]
    fn r767_detect_extracts_pull_request_url() {
        let detector = GitHubExternalObjectDetector;
        let urls = vec![SimpleCanonicalUrl::new(
            "https",
            "github.com",
            "/acme/app/pull/1",
        )];
        let detections = detector.detect(&urls);
        assert_eq!(detections.len(), 1);
        let d = &detections[0];
        assert_eq!(d.object_type, ObjectType::PullRequest);
        assert_eq!(d.external_id, "acme/app#pull/1");
        assert_eq!(d.display_key, "GitHub Pull Request");
        assert_eq!(d.icon_key, "github");
        assert_eq!(d.display_title, "acme/app#1");
        assert_eq!(d.confidence, DetectionConfidence::Exact);
        assert_eq!(d.canonical_index, 0);
    }

    #[test]
    fn r767_detect_extracts_issue_url() {
        let detector = GitHubExternalObjectDetector;
        let urls = vec![SimpleCanonicalUrl::new(
            "https",
            "github.com",
            "/acme/app/issues/2",
        )];
        let detections = detector.detect(&urls);
        assert_eq!(detections.len(), 1);
        assert_eq!(detections[0].object_type, ObjectType::Issue);
        assert_eq!(detections[0].external_id, "acme/app#issues/2");
        assert_eq!(detections[0].display_key, "GitHub Issue");
    }

    #[test]
    fn r767_detect_skips_non_github_urls() {
        let detector = GitHubExternalObjectDetector;
        let urls = vec![
            SimpleCanonicalUrl::new("https", "example.com", "/foo/bar"),
            SimpleCanonicalUrl::new("https", "github.com", "/acme/app/pull/5"),
        ];
        let detections = detector.detect(&urls);
        assert_eq!(detections.len(), 1);
        assert_eq!(detections[0].canonical_index, 1);
    }

    #[test]
    fn r767_detect_returns_empty_for_no_github_input() {
        let detector = GitHubExternalObjectDetector;
        let urls = vec![SimpleCanonicalUrl::new("https", "example.com", "/foo")];
        assert!(detector.detect(&urls).is_empty());
    }

    #[test]
    fn r767_detect_handles_www_normalization() {
        let detector = GitHubExternalObjectDetector;
        let urls = vec![SimpleCanonicalUrl::new(
            "https",
            "www.github.com",
            "/acme/app/pull/9",
        )];
        let detections = detector.detect(&urls);
        assert_eq!(detections.len(), 1);
        assert_eq!(detections[0].external_id, "acme/app#pull/9");
    }

    // ----- Snapshot constructor tests -----

    #[test]
    fn r767_not_found_snapshot_is_terminal_archived() {
        let snap = not_found_snapshot(&identity_pr(), Some("etag-1".into()));
        assert_eq!(snap.status_key, "not_found");
        assert_eq!(snap.status_label, "Not found");
        assert_eq!(snap.status_icon_key, SnapshotIconKey::Archive);
        assert_eq!(snap.status_category, StatusCategory::Archived);
        assert_eq!(snap.status_tone, StatusTone::Muted);
        assert!(snap.is_terminal);
        assert_eq!(snap.ttl_seconds, GITHUB_OBJECT_TTL_SECONDS);
        assert_eq!(snap.etag.as_deref(), Some("etag-1"));
        match snap.data {
            SnapshotData::NotFound { provider, owner, repo, number } => {
                assert_eq!(provider, "github");
                assert_eq!(owner, "acme");
                assert_eq!(repo, "app");
                assert_eq!(number, 12);
            }
            _ => panic!("expected NotFound"),
        }
    }

    #[test]
    fn r767_pull_request_snapshot_open_with_title() {
        let body = json_body(&[
            ("title", Value::String("Add feature".into())),
            ("state", Value::String("open".into())),
            ("updated_at", Value::String("2025-01-01T00:00:00Z".into())),
        ]);
        let map: HashMap<String, Value> = body.as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        let snap = pull_request_snapshot(&identity_pr(), &map, Some("etag-2".into()));
        assert_eq!(snap.status_key, "open");
        assert_eq!(snap.status_label, "Open");
        assert_eq!(snap.status_icon_key, SnapshotIconKey::GitPullRequest);
        assert_eq!(snap.status_category, StatusCategory::Open);
        assert_eq!(snap.status_tone, StatusTone::Info);
        assert!(!snap.is_terminal);
        assert_eq!(snap.display_title, "acme/app#12: Add feature");
        assert_eq!(snap.remote_version.as_deref(), Some("2025-01-01T00:00:00Z"));
        let SnapshotData::PullRequest(data) = snap.data else { panic!("expected PR") };
        assert_eq!(data.state, "open");
        assert!(!data.merged);
        assert!(!data.draft);
    }

    #[test]
    fn r767_pull_request_snapshot_merged_takes_precedence_over_closed() {
        let body = json_body(&[
            ("state", Value::String("closed".into())),
            ("merged", Value::Bool(true)),
            ("merged_at", Value::String("2025-01-02T00:00:00Z".into())),
        ]);
        let map: HashMap<String, Value> = body.as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        let snap = pull_request_snapshot(&identity_pr(), &map, None);
        assert_eq!(snap.status_key, "merged");
        assert_eq!(snap.status_label, "Merged");
        assert_eq!(snap.status_icon_key, SnapshotIconKey::GitMerge);
        assert_eq!(snap.status_category, StatusCategory::Succeeded);
        assert_eq!(snap.status_tone, StatusTone::Success);
        assert!(snap.is_terminal);
        let SnapshotData::PullRequest(d) = snap.data else { panic!() };
        assert!(d.merged);
    }

    #[test]
    fn r767_pull_request_snapshot_merged_via_merged_at_alone() {
        // merged=false but merged_at set — Node ORs these.
        let body = json_body(&[
            ("state", Value::String("closed".into())),
            ("merged_at", Value::String("2025-01-02T00:00:00Z".into())),
        ]);
        let map: HashMap<String, Value> = body.as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        let snap = pull_request_snapshot(&identity_pr(), &map, None);
        let SnapshotData::PullRequest(d) = snap.data else { panic!() };
        assert!(d.merged, "merged_at alone should imply merged=true");
    }

    #[test]
    fn r767_pull_request_snapshot_closed_not_merged_uses_x_circle() {
        let body = json_body(&[("state", Value::String("closed".into()))]);
        let map: HashMap<String, Value> = body.as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        let snap = pull_request_snapshot(&identity_pr(), &map, None);
        assert_eq!(snap.status_icon_key, SnapshotIconKey::XCircle);
        assert!(snap.is_terminal);
        assert_eq!(snap.status_category, StatusCategory::Closed);
        assert_eq!(snap.status_tone, StatusTone::Muted);
    }

    #[test]
    fn r767_pull_request_snapshot_draft_uses_clock_icon() {
        let body = json_body(&[
            ("state", Value::String("open".into())),
            ("draft", Value::Bool(true)),
        ]);
        let map: HashMap<String, Value> = body.as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        let snap = pull_request_snapshot(&identity_pr(), &map, None);
        assert_eq!(snap.status_key, "draft");
        assert_eq!(snap.status_label, "Draft");
        assert_eq!(snap.status_icon_key, SnapshotIconKey::Clock);
        assert_eq!(snap.status_category, StatusCategory::Waiting);
        assert_eq!(snap.status_tone, StatusTone::Warning);
        assert!(!snap.is_terminal);
    }

    #[test]
    fn r767_pull_request_snapshot_captures_review_decision() {
        let body = json_body(&[
            ("state", Value::String("open".into())),
            ("review_decision", Value::String("APPROVED".into())),
        ]);
        let map: HashMap<String, Value> = body.as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        let snap = pull_request_snapshot(&identity_pr(), &map, None);
        let SnapshotData::PullRequest(d) = snap.data else { panic!() };
        assert_eq!(d.review_decision.as_deref(), Some("APPROVED"));
    }

    #[test]
    fn r767_issue_snapshot_open() {
        let body = json_body(&[
            ("title", Value::String("Fix bug".into())),
            ("state", Value::String("open".into())),
            ("updated_at", Value::String("2025-02-02T00:00:00Z".into())),
        ]);
        let map: HashMap<String, Value> = body.as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        let snap = issue_snapshot(&identity_issue(), &map, None);
        assert_eq!(snap.status_key, "open");
        assert_eq!(snap.status_label, "Open");
        assert_eq!(snap.status_icon_key, SnapshotIconKey::CircleDot);
        assert_eq!(snap.status_category, StatusCategory::Open);
        assert_eq!(snap.status_tone, StatusTone::Info);
        assert!(!snap.is_terminal);
        assert_eq!(snap.display_title, "acme/app#34: Fix bug");
        let SnapshotData::Issue(d) = snap.data else { panic!() };
        assert_eq!(d.state, "open");
        assert_eq!(d.state_reason, None);
    }

    #[test]
    fn r767_issue_snapshot_closed_with_reason_has_compound_status_key() {
        let body = json_body(&[
            ("state", Value::String("closed".into())),
            ("state_reason", Value::String("not_planned".into())),
        ]);
        let map: HashMap<String, Value> = body.as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        let snap = issue_snapshot(&identity_issue(), &map, None);
        assert_eq!(snap.status_key, "closed_not_planned");
        assert_eq!(snap.status_label, "Closed: not planned");
        assert_eq!(snap.status_icon_key, SnapshotIconKey::Circle);
        assert_eq!(snap.status_category, StatusCategory::Closed);
        assert_eq!(snap.status_tone, StatusTone::Muted);
        assert!(snap.is_terminal);
        let SnapshotData::Issue(d) = snap.data else { panic!() };
        assert_eq!(d.state_reason.as_deref(), Some("not_planned"));
    }

    #[test]
    fn r767_issue_snapshot_closed_no_reason_uses_plain_label() {
        let body = json_body(&[("state", Value::String("closed".into()))]);
        let map: HashMap<String, Value> = body.as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        let snap = issue_snapshot(&identity_issue(), &map, None);
        assert_eq!(snap.status_key, "closed");
        assert_eq!(snap.status_label, "Closed");
    }

    #[test]
    fn r767_issue_snapshot_unknown_state() {
        let body = json_body(&[]);
        let map: HashMap<String, Value> = body.as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        let snap = issue_snapshot(&identity_issue(), &map, None);
        assert_eq!(snap.status_key, "unknown");
        assert_eq!(snap.status_label, "Unknown");
        assert_eq!(snap.status_category, StatusCategory::Unknown);
        assert_eq!(snap.status_tone, StatusTone::Neutral);
    }

    #[test]
    fn r767_snapshot_no_title_falls_back_to_display_title() {
        let body = json_body(&[("state", Value::String("open".into()))]);
        let map: HashMap<String, Value> = body.as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        let snap = issue_snapshot(&identity_issue(), &map, None);
        assert_eq!(snap.display_title, "acme/app#34");
    }

    // ----- Resolver tests -----

    #[tokio::test]
    async fn r767_resolve_pr_open_hits_pulls_endpoint_with_bearer() {
        let resp = GitHubFetchResult {
            status: 200,
            etag: Some("W/\"weak\"".into()),
            body: Some(json_body(&[
                ("title", Value::String("My PR".into())),
                ("state", Value::String("open".into())),
            ])),
            ..Default::default()
        };
        let fetcher = MockFetcher::new(Ok(resp));
        let last_url = StdArc::clone(&fetcher.last_url);
        let last_headers = StdArc::clone(&fetcher.last_headers);
        let provider = create_github_external_object_provider(
            fetcher.clone(),
            MockTokenProvider(Some("tok-1".into())),
        );
        let [pr_resolver, _issue_resolver] = provider.resolvers();
        let snap = pr_resolver
            .resolve("company-a", &pr_record())
            .await
            .expect("resolve OK");
        match snap.data {
            SnapshotData::PullRequest(d) => assert_eq!(d.state, "open"),
            _ => panic!("expected PR snapshot"),
        }
        let url = last_url.lock().clone().expect("captured url");
        assert_eq!(
            url,
            "https://api.github.com/repos/acme/app/pulls/12"
        );
        let headers = last_headers.lock().clone().expect("captured headers");
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer tok-1")
        );
        assert_eq!(
            headers.get("accept").map(String::as_str),
            Some("application/vnd.github+json")
        );
        assert_eq!(
            headers.get("x-github-api-version").map(String::as_str),
            Some("2022-11-28")
        );
        assert_eq!(
            headers.get("user-agent").map(String::as_str),
            Some("paperclip-external-object-resolver")
        );
    }

    #[tokio::test]
    async fn r767_resolve_issue_hits_issues_endpoint() {
        let resp = GitHubFetchResult {
            status: 200,
            etag: Some("e1".into()),
            body: Some(json_body(&[
                ("state", Value::String("open".into())),
                ("title", Value::String("Issue".into())),
            ])),
            ..Default::default()
        };
        let fetcher = MockFetcher::new(Ok(resp));
        let last_url = StdArc::clone(&fetcher.last_url);
        let provider = create_github_external_object_provider(
            fetcher.clone(),
            MockTokenProvider(None),
        );
        let [_, issue_resolver] = provider.resolvers();
        let snap = issue_resolver
            .resolve("co", &issue_record())
            .await
            .expect("resolve OK");
        assert_eq!(snap.etag.as_deref(), Some("e1"));
        let url = last_url.lock().clone().unwrap();
        assert_eq!(
            url,
            "https://api.github.com/repos/acme/app/issues/34"
        );
        // No token → no Authorization header.
        let provider = create_github_external_object_provider(
            fetcher,
            MockTokenProvider(None),
        );
        let [_, resolver_b] = provider.resolvers();
        let _ = resolver_b; // silence unused
    }

    #[tokio::test]
    async fn r767_resolve_pr_resolver_rejects_issue_record() {
        // PR resolver should reject a PR-shaped record (wrong kind), so
        // we expect the failure path triggered by parse_github_object
        // finding objectType=Issue + comparing to PullRequest.
        let fetcher = MockFetcher::new(Ok(GitHubFetchResult {
            status: 200,
            body: Some(json_body(&[])),
            ..Default::default()
        }));
        let provider = create_github_external_object_provider(
            fetcher,
            MockTokenProvider(None),
        );
        let [pr_resolver, _] = provider.resolvers();
        let result = pr_resolver
            .resolve("co", &issue_record())
            .await;
        let err = result.expect_err("rejected");
        assert_eq!(err.liveness, LivenessState::Unreachable);
        assert_eq!(err.error_code, ErrorCode::GithubUnreachable);
    }

    #[tokio::test]
    async fn r767_resolve_returns_invalid_identity_for_garbage() {
        let fetcher = MockFetcher::new(Ok(GitHubFetchResult::default()));
        let provider = create_github_external_object_provider(
            fetcher,
            MockTokenProvider(None),
        );
        let [pr_resolver, _] = provider.resolvers();
        let garbage = GitHubExternalObjectRecord {
            external_id: "not-a-real-id".into(),
            sanitized_canonical_url: None,
        };
        let err = pr_resolver
            .resolve("co", &garbage)
            .await
            .expect_err("rejected");
        assert_eq!(err.error_code, ErrorCode::GithubUnreachable);
        assert_eq!(err.retry_after_seconds, GITHUB_OBJECT_TTL_SECONDS);
    }

    #[tokio::test]
    async fn r767_resolve_404_returns_not_found_snapshot() {
        let fetcher = MockFetcher::new(Ok(GitHubFetchResult {
            status: 404,
            etag: Some("etag-404".into()),
            ..Default::default()
        }));
        let provider = create_github_external_object_provider(
            fetcher,
            MockTokenProvider(None),
        );
        let [pr_resolver, _] = provider.resolvers();
        let snap = pr_resolver
            .resolve("co", &pr_record())
            .await
            .expect("404 should yield snapshot");
        assert_eq!(snap.status_key, "not_found");
        assert!(snap.is_terminal);
        assert_eq!(snap.etag.as_deref(), Some("etag-404"));
    }

    #[tokio::test]
    async fn r767_resolve_401_maps_to_auth_required() {
        let fetcher = MockFetcher::new(Ok(GitHubFetchResult {
            status: 401,
            retry_after: Some("120".into()),
            ..Default::default()
        }));
        let provider = create_github_external_object_provider(
            fetcher,
            MockTokenProvider(None),
        );
        let [pr_resolver, _] = provider.resolvers();
        let err = pr_resolver
            .resolve("co", &pr_record())
            .await
            .expect_err("401 fail");
        assert_eq!(err.liveness, LivenessState::AuthRequired);
        assert_eq!(err.error_code, ErrorCode::GithubAuthRequired);
        assert_eq!(err.retry_after_seconds, 120);
    }

    #[tokio::test]
    async fn r767_resolve_403_rate_limit_zero_maps_to_rate_limited() {
        let fetcher = MockFetcher::new(Ok(GitHubFetchResult {
            status: 403,
            x_ratelimit_remaining: Some("0".into()),
            retry_after: Some("45".into()),
            ..Default::default()
        }));
        let provider = create_github_external_object_provider(
            fetcher,
            MockTokenProvider(None),
        );
        let [pr_resolver, _] = provider.resolvers();
        let err = pr_resolver
            .resolve("co", &pr_record())
            .await
            .expect_err("403 rate limited");
        assert_eq!(err.liveness, LivenessState::Unreachable);
        assert_eq!(err.error_code, ErrorCode::GithubRateLimited);
        assert_eq!(err.retry_after_seconds, 45);
    }

    #[tokio::test]
    async fn r767_resolve_500_maps_to_unreachable() {
        let fetcher = MockFetcher::new(Ok(GitHubFetchResult {
            status: 500,
            ..Default::default()
        }));
        let provider = create_github_external_object_provider(
            fetcher,
            MockTokenProvider(None),
        );
        let [pr_resolver, _] = provider.resolvers();
        let err = pr_resolver
            .resolve("co", &pr_record())
            .await
            .expect_err("500");
        assert_eq!(err.error_code, ErrorCode::GithubUnreachable);
        assert!(err.error_message.contains("HTTP 500"));
    }

    #[tokio::test]
    async fn r767_resolve_transport_error_maps_to_fetch_failed_unreachable() {
        let fetcher = MockFetcher::new(Err(()));
        let provider = create_github_external_object_provider(
            fetcher,
            MockTokenProvider(None),
        );
        let [pr_resolver, _] = provider.resolvers();
        let err = pr_resolver
            .resolve("co", &pr_record())
            .await
            .expect_err("transport");
        assert_eq!(err.liveness, LivenessState::Unreachable);
        assert_eq!(err.error_code, ErrorCode::GithubUnreachable);
    }

    #[tokio::test]
    async fn r767_resolve_token_provider_error_maps_to_auth_required() {
        let fetcher = MockFetcher::new(Ok(GitHubFetchResult::default()));
        let provider = create_github_external_object_provider(
            fetcher,
            MockTokenProviderError,
        );
        let [pr_resolver, _] = provider.resolvers();
        let err = pr_resolver
            .resolve("co", &pr_record())
            .await
            .expect_err("token provider err");
        assert_eq!(err.liveness, LivenessState::AuthRequired);
        assert_eq!(err.error_code, ErrorCode::GithubAuthRequired);
    }

    #[tokio::test]
    async fn r767_resolve_token_with_whitespace_is_trimmed() {
        let resp = GitHubFetchResult {
            status: 200,
            body: Some(json_body(&[("state", Value::String("open".into()))])),
            ..Default::default()
        };
        let fetcher = MockFetcher::new(Ok(resp));
        let last_headers = StdArc::clone(&fetcher.last_headers);
        let provider = create_github_external_object_provider(
            fetcher.clone(),
            MockTokenProvider(Some("  tok-trim  ".into())),
        );
        let [pr_resolver, _] = provider.resolvers();
        pr_resolver.resolve("co", &pr_record()).await.expect("ok");
        let headers = last_headers.lock().clone().unwrap();
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer tok-trim")
        );
    }

    #[tokio::test]
    async fn r767_resolve_invalid_body_maps_to_unreachable() {
        let fetcher = MockFetcher::new(Ok(GitHubFetchResult {
            status: 200,
            body: Some(Value::String("not an object".into())),
            ..Default::default()
        }));
        let provider = create_github_external_object_provider(
            fetcher,
            MockTokenProvider(None),
        );
        let [pr_resolver, _] = provider.resolvers();
        let err = pr_resolver
            .resolve("co", &pr_record())
            .await
            .expect_err("invalid body");
        assert_eq!(err.error_code, ErrorCode::GithubUnreachable);
    }

    #[tokio::test]
    async fn r767_resolve_missing_body_maps_to_unreachable() {
        let fetcher = MockFetcher::new(Ok(GitHubFetchResult {
            status: 200,
            body: None,
            ..Default::default()
        }));
        let provider = create_github_external_object_provider(
            fetcher,
            MockTokenProvider(None),
        );
        let [pr_resolver, _] = provider.resolvers();
        let err = pr_resolver
            .resolve("co", &pr_record())
            .await
            .expect_err("missing body");
        assert_eq!(err.error_code, ErrorCode::GithubUnreachable);
    }

    #[test]
    fn r767_urlencode_handles_valid_owner_repo_chars() {
        // All valid GitHub owner/repo characters should pass through unchanged.
        assert_eq!(urlencode("acme-app"), "acme-app");
        assert_eq!(urlencode("acme.app"), "acme.app");
        assert_eq!(urlencode("acme_app"), "acme_app");
        assert_eq!(urlencode("acme123"), "acme123");
        // Slash should be encoded.
        assert_eq!(urlencode("a/b"), "a%2Fb");
        assert_eq!(urlencode("a b"), "a%20b");
    }

    #[test]
    fn r767_factory_returns_detector_and_two_resolvers() {
        let provider = create_github_external_object_provider(
            MockFetcher::new(Ok(GitHubFetchResult::default())),
            MockTokenProvider(None),
        );
        let _ = provider.detector();
        let [pr, issue] = provider.resolvers();
        assert_eq!(pr.provider_key(), "github");
        assert_eq!(pr.object_type(), ObjectType::PullRequest);
        assert_eq!(issue.object_type(), ObjectType::Issue);
    }

    #[test]
    fn r767_status_category_and_tone_strings_match_node() {
        // The Node upstream uses these literal strings in the snapshot JSON.
        for (cat, s) in [
            (StatusCategory::Open, "open"),
            (StatusCategory::Closed, "closed"),
            (StatusCategory::Succeeded, "succeeded"),
            (StatusCategory::Archived, "archived"),
            (StatusCategory::Waiting, "waiting"),
            (StatusCategory::Unknown, "unknown"),
        ] {
            assert_eq!(cat.as_str(), s);
        }
        for (tone, s) in [
            (StatusTone::Info, "info"),
            (StatusTone::Success, "success"),
            (StatusTone::Warning, "warning"),
            (StatusTone::Muted, "muted"),
            (StatusTone::Neutral, "neutral"),
        ] {
            assert_eq!(tone.as_str(), s);
        }
    }

    #[test]
    fn r767_icon_keys_match_node_string_literals() {
        // Used verbatim in the snapshot payload; verify we don't drift.
        assert_eq!(SnapshotIconKey::Github.as_str(), "github");
        assert_eq!(SnapshotIconKey::GitMerge.as_str(), "git-merge");
        assert_eq!(SnapshotIconKey::GitPullRequest.as_str(), "git-pull-request");
        assert_eq!(SnapshotIconKey::XCircle.as_str(), "x-circle");
        assert_eq!(SnapshotIconKey::Clock.as_str(), "clock");
        assert_eq!(SnapshotIconKey::Circle.as_str(), "circle");
        assert_eq!(SnapshotIconKey::CircleDot.as_str(), "circle-dot");
        assert_eq!(SnapshotIconKey::Archive.as_str(), "archive");
    }
}
