#![forbid(unsafe_code)]

//! External object URL canonicalization, identity hashing, and mention source key derivation.
//!
//! R533: Direct port of `paperclip/packages/shared/src/external-objects-server.ts`.
//!
//! 设计原则:
//! - 所有 pub fn 都是纯函数 (无 IO, 无副作用)
//! - 复用 R528 `pc-issue-references` 的 `strip_markdown_code` + `trim_trailing_punctuation` (跨 crate)
//! - 用 `sha2` crate 替代 Node `crypto.createHash` (无运行时开销)
//! - 用 `url::Url::parse` 替代 Node `URL` constructor + try/catch
//!
//! 范围 (本 crate):
//! - [`find_external_object_url_matches`] — 从 markdown 找出 external URL match
//! - [`canonicalize_external_object_url`] — 规范化 URL + 计算 identity hash
//! - [`extract_external_object_canonical_urls`] — extract + dedup by identity hash
//! - [`build_external_object_scoped_identity_key`] — company-scoped identity key
//! - [`build_external_object_mention_source_key`] — mention source key
//!
//! **不** 范围 (留给集成层):
//! - `pc-github-external-objects` (R525) 是另一相关模块: GitHub URL 解析 + retry-after + status
//! - server `services/external-objects.ts` 的 DB 持久化 + mention replacement
//! - UI `useMentionedExternalObjects` 渲染
//!
//! Node 上游在 `server/src/services/external-objects.ts` 等多处用;
//! Rust port 让 pc-server 业务层直接调用 5 个 derive fn, 不重复实现.

use pc_issue_references::{
    parse_issue_reference_href, strip_markdown_code, trim_trailing_punctuation,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use once_cell::sync::Lazy;

/// Match `https://...` and `http://...` URLs (case-insensitive).
///
/// Mirrors Node upstream `EXTERNAL_URL_TOKEN_RE = /https?:\/\/[^\s<>()]+/gi`.
static EXTERNAL_URL_TOKEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)https?://[^\s<>()]+").expect("valid regex pattern"));

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single external URL match extracted from markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalObjectUrlMatch {
    pub index: usize,
    pub length: usize,
    pub matched_text: String,
}

/// Canonical identity for an external URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalObjectCanonicalIdentity {
    pub scheme: String,
    pub host: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub query_param_hashes: std::collections::BTreeMap<String, String>,
}

/// Options for URL canonicalization.
#[derive(Debug, Clone, Default)]
pub struct ExternalObjectUrlCanonicalizationOptions {
    /// Query params that contribute to identity (hashed, not stored in plaintext).
    pub identity_query_params: Vec<String>,
}

/// Result of URL canonicalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalObjectCanonicalUrl {
    pub sanitized_canonical_url: String,
    pub sanitized_display_url: String,
    pub canonical_identity: ExternalObjectCanonicalIdentity,
    pub canonical_identity_hash: String,
    pub redacted_matched_text: String,
}

/// Source metadata for an external object mention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalObjectMentionSource {
    pub company_id: Option<String>,
    pub source_issue_id: Option<String>,
    pub source_kind: String,
    pub source_record_id: Option<String>,
    pub document_key: Option<String>,
    pub property_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Find external URL matches in markdown.
///
/// - Skips inline code (single backtick) and fenced code blocks (``` or ~~~)
/// - Trims trailing punctuation from each match
/// - Skips matches that are issue references (`parseIssueReferenceHref` succeeds)
/// - Returns matches in source order
#[must_use]
pub fn find_external_object_url_matches(markdown: &str) -> Vec<ExternalObjectUrlMatch> {
    if markdown.is_empty() {
        return Vec::new();
    }

    let scrubbed = strip_markdown_code(markdown);
    let mut matches: Vec<ExternalObjectUrlMatch> = Vec::new();

    for caps in EXTERNAL_URL_TOKEN_RE.find_iter(&scrubbed) {
        let matched_text = trim_trailing_punctuation(caps.as_str());
        if matched_text.is_empty() {
            continue;
        }
        if parse_issue_reference_href(&matched_text).is_some() {
            continue;
        }
        matches.push(ExternalObjectUrlMatch {
            index: caps.start(),
            length: matched_text.len(),
            matched_text,
        });
    }
    matches
}

/// Canonicalize an external URL and compute its identity hash.
///
/// Returns `None` when:
/// - URL cannot be parsed
/// - Protocol is not http/https
/// - URL has userinfo (`username` or `password`)
#[must_use]
pub fn canonicalize_external_object_url(
    value: &str,
    options: &ExternalObjectUrlCanonicalizationOptions,
) -> Option<ExternalObjectCanonicalUrl> {
    let url = Url::parse(value.trim()).ok()?;

    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }

    let scheme = url.scheme().to_string();
    let host = url.host_str().unwrap_or("").to_lowercase();
    let path = normalize_pathname(url.path());

    let mut query_param_hashes: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut identity_params: Vec<&str> = options
        .identity_query_params
        .iter()
        .map(String::as_str)
        .collect();
    identity_params.sort();
    for key in identity_params {
        let values: Vec<String> = url
            .query_pairs()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .collect();
        if !values.is_empty() {
            query_param_hashes.insert(key.to_string(), sha256_hex(&values.join("\u{0}")));
        }
    }

    let canonical_identity = ExternalObjectCanonicalIdentity {
        scheme: scheme.clone(),
        host: host.clone(),
        path: path.clone(),
        query_param_hashes,
    };

    let sanitized_canonical_url = format!("{scheme}://{host}{path}");
    let canonical_identity_hash = sha256_hex(&stable_stringify(&canonical_identity)?);

    Some(ExternalObjectCanonicalUrl {
        sanitized_display_url: sanitized_canonical_url.clone(),
        redacted_matched_text: sanitized_canonical_url.clone(),
        sanitized_canonical_url,
        canonical_identity,
        canonical_identity_hash,
    })
}

/// Extract unique canonical URLs from markdown, deduplicated by identity hash.
#[must_use]
pub fn extract_external_object_canonical_urls(
    markdown: &str,
    options: &ExternalObjectUrlCanonicalizationOptions,
) -> Vec<ExternalObjectCanonicalUrl> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ordered: Vec<ExternalObjectCanonicalUrl> = Vec::new();

    for m in find_external_object_url_matches(markdown) {
        let Some(canonical) = canonicalize_external_object_url(&m.matched_text, options) else {
            continue;
        };
        if seen.insert(canonical.canonical_identity_hash.clone()) {
            ordered.push(canonical);
        }
    }
    ordered
}

/// Build a company-scoped identity key for an external object.
#[must_use]
pub fn build_external_object_scoped_identity_key(
    company_id: &str,
    provider_key: &str,
    object_type: &str,
    canonical_identity_hash: &str,
) -> String {
    format!("{company_id}:{provider_key}:{object_type}:{canonical_identity_hash}")
}

/// Build a stable key identifying a mention source (used to replace existing mentions).
#[must_use]
pub fn build_external_object_mention_source_key(source: &ExternalObjectMentionSource) -> String {
    let company_id = source.company_id.as_deref().unwrap_or("");
    let source_issue_id = source.source_issue_id.as_deref().unwrap_or("");
    let source_record_id = source.source_record_id.as_deref().unwrap_or("");
    let document_key = source.document_key.as_deref().unwrap_or("");
    let property_key = source.property_key.as_deref().unwrap_or("");
    format!(
        "{company_id}:{source_issue_id}:{kind}:{source_record_id}:{document_key}:{property_key}",
        kind = source.source_kind,
    )
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// SHA-256 hex of a string.
///
/// Mirrors Node upstream `sha256Hex(value)`.
fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut out, "{byte:02x}").unwrap();
    }
    out
}

/// Stable JSON-like stringification with sorted object keys.
///
/// Mirrors Node upstream `stableStringify(value)`.
/// Uses `serde_json::Value` for the actual JSON formatting, then sorts object keys.
fn stable_stringify(value: &impl Serialize) -> Option<String> {
    let json = serde_json::to_value(value).ok()?;
    Some(stable_stringify_value(&json))
}

fn stable_stringify_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "null".into()),
        serde_json::Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(stable_stringify_value).collect();
            format!("[{}]", parts.join(","))
        }
        serde_json::Value::Object(obj) => {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    let key_json = serde_json::to_string(k).unwrap_or_else(|_| "null".into());
                    format!("{key_json}:{}", stable_stringify_value(&obj[k]))
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// Empty pathname becomes "/". Mirrors Node upstream `normalizePathname`.
fn normalize_pathname(pathname: &str) -> String {
    if pathname.is_empty() {
        "/".to_string()
    } else {
        pathname.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- find_external_object_url_matches -----

    #[test]
    fn r533_url_matches_extracts_external_only() {
        // Upstream test 1: external URL after internal issue refs.
        let matches = find_external_object_url_matches(
            "See PAP-1, /issues/PAP-2, https://paperclip.ing/PAP/issues/PAP-3, and https://github.com/acme/app/pull/4.",
        );
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].matched_text,
            "https://github.com/acme/app/pull/4"
        );
        assert_eq!(matches[0].index, 70);
        assert_eq!(matches[0].length, 34);
    }

    #[test]
    fn r533_url_matches_ignores_code_blocks() {
        // Upstream test 2
        let markdown = [
            "Use https://github.com/acme/app/pull/1 here.",
            "`https://github.com/acme/app/pull/2` should not count.",
            "```",
            "https://github.com/acme/app/pull/3",
            "```",
        ]
        .join("\n");

        let matches = find_external_object_url_matches(&markdown);
        let matched: Vec<&str> = matches.iter().map(|m| m.matched_text.as_str()).collect();
        assert_eq!(matched, vec!["https://github.com/acme/app/pull/1"]);
    }

    #[test]
    fn r533_url_matches_skips_issue_references() {
        // /issues/PAP-123 is an issue reference, not external.
        let matches = find_external_object_url_matches("see /issues/PAP-1 and https://example.com");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "https://example.com");
    }

    #[test]
    fn r533_url_matches_trims_trailing_punctuation() {
        let matches = find_external_object_url_matches("see https://example.com/page.");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "https://example.com/page");
    }

    #[test]
    fn r533_url_matches_empty_input() {
        assert!(find_external_object_url_matches("").is_empty());
    }

    // ----- canonicalize_external_object_url -----

    #[test]
    fn r533_canonicalize_strips_query_and_fragment_by_default() {
        // Upstream test 3
        let opts = ExternalObjectUrlCanonicalizationOptions::default();
        let result = canonicalize_external_object_url(
            "HTTPS://GitHub.com/acme/app/pull/1?token=secret#discussion",
            &opts,
        )
        .expect("canonicalization should succeed");
        assert_eq!(
            result.sanitized_canonical_url,
            "https://github.com/acme/app/pull/1"
        );
        assert_eq!(
            result.sanitized_display_url,
            "https://github.com/acme/app/pull/1"
        );
        assert_eq!(result.canonical_identity.scheme, "https");
        assert_eq!(result.canonical_identity.host, "github.com");
        assert_eq!(result.canonical_identity.path, "/acme/app/pull/1");
    }

    #[test]
    fn r533_canonicalize_rejects_userinfo() {
        // Upstream test 4
        let opts = ExternalObjectUrlCanonicalizationOptions::default();
        assert!(canonicalize_external_object_url(
            "https://token:secret@github.com/acme/app/pull/1",
            &opts
        )
        .is_none());
    }

    #[test]
    fn r533_canonicalize_hashes_identity_query_params() {
        // Upstream test 5
        let mut opts = ExternalObjectUrlCanonicalizationOptions::default();
        opts.identity_query_params = vec!["id".to_string()];

        let first = canonicalize_external_object_url(
            "https://deploy.test/run?id=secret-run&token=drop",
            &opts,
        )
        .expect("canonicalization should succeed");
        let second = canonicalize_external_object_url(
            "https://deploy.test/run?id=secret-run&token=other",
            &opts,
        )
        .expect("canonicalization should succeed");

        assert_eq!(first.sanitized_canonical_url, "https://deploy.test/run");
        let id_hash = first
            .canonical_identity
            .query_param_hashes
            .get("id")
            .expect("id hash should be present");
        assert_eq!(id_hash.len(), 64, "sha256 hex should be 64 chars");
        assert!(!id_hash.contains("secret-run"));
        // Same id value → same identity hash
        assert_eq!(
            second.canonical_identity_hash,
            first.canonical_identity_hash
        );
    }

    #[test]
    fn r533_canonicalize_rejects_non_http_protocols() {
        let opts = ExternalObjectUrlCanonicalizationOptions::default();
        assert!(canonicalize_external_object_url("ftp://example.com/file", &opts).is_none());
        assert!(canonicalize_external_object_url("file:///etc/passwd", &opts).is_none());
    }

    #[test]
    fn r533_canonicalize_rejects_malformed_url() {
        let opts = ExternalObjectUrlCanonicalizationOptions::default();
        assert!(canonicalize_external_object_url("not a url", &opts).is_none());
        assert!(canonicalize_external_object_url("ht://bad", &opts).is_none());
    }

    #[test]
    fn r533_canonicalize_normalizes_empty_path_to_slash() {
        let opts = ExternalObjectUrlCanonicalizationOptions::default();
        let result =
            canonicalize_external_object_url("https://example.com", &opts).expect("should succeed");
        assert_eq!(result.canonical_identity.path, "/");
    }

    #[test]
    fn r533_canonicalize_lowercases_host() {
        let opts = ExternalObjectUrlCanonicalizationOptions::default();
        let result =
            canonicalize_external_object_url("https://GitHub.COM/Acme/App", &opts).unwrap();
        assert_eq!(result.canonical_identity.host, "github.com");
        assert_eq!(result.canonical_identity.path, "/Acme/App"); // path NOT lowercased
    }

    // ----- extract_external_object_canonical_urls -----

    #[test]
    fn r533_extract_dedupes_by_identity_hash() {
        // Upstream test 6
        let opts = ExternalObjectUrlCanonicalizationOptions::default();
        let urls = extract_external_object_canonical_urls(
            "https://github.com/acme/app/pull/1?token=a and https://github.com/acme/app/pull/1#discussion",
            &opts,
        );
        let canonical_urls: Vec<&str> = urls
            .iter()
            .map(|u| u.sanitized_canonical_url.as_str())
            .collect();
        assert_eq!(canonical_urls, vec!["https://github.com/acme/app/pull/1"]);
    }

    #[test]
    fn r533_extract_keeps_distinct_urls() {
        let opts = ExternalObjectUrlCanonicalizationOptions::default();
        let urls = extract_external_object_canonical_urls(
            "https://github.com/a/b/1 and https://github.com/a/b/2",
            &opts,
        );
        assert_eq!(urls.len(), 2);
    }

    // ----- build_external_object_scoped_identity_key -----

    #[test]
    fn r533_scoped_key_includes_company_id() {
        // Upstream test 7
        let base_args = || ("github", "pull_request", "hash");
        let a = build_external_object_scoped_identity_key(
            "company-a",
            base_args().0,
            base_args().1,
            base_args().2,
        );
        let b = build_external_object_scoped_identity_key(
            "company-b",
            base_args().0,
            base_args().1,
            base_args().2,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn r533_scoped_key_format_is_colon_joined() {
        let k = build_external_object_scoped_identity_key("co", "github", "issue", "abc");
        assert_eq!(k, "co:github:issue:abc");
    }

    // ----- build_external_object_mention_source_key -----

    #[test]
    fn r533_mention_source_key_stable_for_same_input() {
        // Upstream test 8
        let mk = || ExternalObjectMentionSource {
            company_id: Some("company-a".into()),
            source_issue_id: Some("issue-1".into()),
            source_kind: "comment".into(),
            source_record_id: Some("comment-1".into()),
            document_key: None,
            property_key: None,
        };
        assert_eq!(
            build_external_object_mention_source_key(&mk()),
            build_external_object_mention_source_key(&mk()),
        );
    }

    #[test]
    fn r533_mention_source_key_differs_by_company() {
        let mk = |co: &str| ExternalObjectMentionSource {
            company_id: Some(co.into()),
            source_issue_id: Some("issue-1".into()),
            source_kind: "comment".into(),
            source_record_id: Some("comment-1".into()),
            document_key: None,
            property_key: None,
        };
        assert_ne!(
            build_external_object_mention_source_key(&mk("company-a")),
            build_external_object_mention_source_key(&mk("company-b")),
        );
    }

    #[test]
    fn r533_mention_source_key_format_includes_all_fields() {
        let s = ExternalObjectMentionSource {
            company_id: Some("co".into()),
            source_issue_id: Some("issue".into()),
            source_kind: "document".into(),
            source_record_id: None,
            document_key: Some("plan".into()),
            property_key: None,
        };
        assert_eq!(
            build_external_object_mention_source_key(&s),
            "co:issue:document::plan:"
        );
    }

    // ----- stable_stringify_value -----

    #[test]
    fn r533_stable_stringify_sorts_object_keys() {
        let v: serde_json::Value = serde_json::json!({"b": 1, "a": 2});
        assert_eq!(stable_stringify_value(&v), "{\"a\":2,\"b\":1}");
    }

    #[test]
    fn r533_stable_stringify_handles_nested() {
        let v: serde_json::Value = serde_json::json!({"x": [{"c": 1, "b": 2}], "a": null});
        let s = stable_stringify_value(&v);
        // Top-level key order: a, x. Inside array, order preserved. Inside nested object: b, c.
        assert_eq!(s, "{\"a\":null,\"x\":[{\"b\":2,\"c\":1}]}");
    }
}
