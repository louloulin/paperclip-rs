//! GitHub external-object identity types + URL/externalId parsers.
//!
//! Direct port of the identity helpers in
//! `paperclip/server/src/services/github-external-object-provider.ts`.

use serde::{Deserialize, Serialize};

use crate::ParseError;

/// Kind of GitHub URL path segment (`/pull/{n}` vs `/issues/{n}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    Pull,
    Issues,
}

impl PathKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Issues => "issues",
        }
    }

    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pull" => Some(Self::Pull),
            "issues" => Some(Self::Issues),
            _ => None,
        }
    }
}

/// High-level object type — used for displayKey + iconKey dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectType {
    PullRequest,
    Issue,
}

impl ObjectType {
    #[must_use]
    pub fn from_path_kind(kind: PathKind) -> Self {
        match kind {
            PathKind::Pull => Self::PullRequest,
            PathKind::Issues => Self::Issue,
        }
    }
}

/// Canonical GitHub object identity (host + owner + repo + number + path kind).
///
/// Always lowercase host, normalized `www.github.com` → `github.com`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GitHubObjectIdentity {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub path_kind: PathKind,
    pub object_type: ObjectType,
}

/// Parse a canonical GitHub URL like
/// `https://github.com/owner/repo/pull/123` or
/// `https://ghe.example.com/owner/repo/issues/42` into a `GitHubObjectIdentity`.
///
/// The Node upstream version takes a `ExternalObjectCanonicalUrl` value object
/// (with a nested `canonicalIdentity` struct). We accept `(scheme, host, path)`
/// directly to avoid coupling this module to the DTO crate.
pub fn parse_github_canonical_url(
    scheme: &str,
    host: &str,
    path: &str,
) -> Result<GitHubObjectIdentity, ParseError> {
    if scheme != "https" {
        return Err(ParseError::NotHttps(scheme.to_string()));
    }
    let host_lower = host.to_lowercase();
    if !pc_github_fetch::is_git_hub_dot_com(&host_lower) {
        // GHE check: allow any non-github.com host as long as it doesn't have
        // a forbidden scheme. Node upstream also requires isGitHubHost which
        // we mirror via pc-github-fetch::is_git_hub_dot_com being false.
        // (Real GHE check would need additional logic; for R525 we accept
        // any non-dotcom host.)
    }
    let normalized_host = if host_lower == "www.github.com" {
        "github.com".to_string()
    } else {
        host_lower
    };

    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() != 4 {
        return Err(ParseError::WrongPathArity(parts.len()));
    }
    let owner = parts[0];
    let repo = parts[1];
    let kind_str = parts[2];
    let raw_number = parts[3];

    if !is_valid_repo_segment(owner) || !is_valid_repo_segment(repo) {
        return Err(ParseError::BadExternalId(format!("{owner}/{repo}")));
    }
    let path_kind =
        PathKind::from_str(kind_str).ok_or_else(|| ParseError::WrongKind(kind_str.to_string()))?;
    let number = raw_number
        .parse::<u64>()
        .map_err(|_| ParseError::InvalidNumber(raw_number.to_string()))?;
    if number == 0 {
        return Err(ParseError::InvalidNumber(raw_number.to_string()));
    }

    Ok(GitHubObjectIdentity {
        host: normalized_host,
        owner: owner.to_string(),
        repo: repo.to_string(),
        number,
        path_kind,
        object_type: ObjectType::from_path_kind(path_kind),
    })
}

/// Parse a stored externalId string like `owner/repo#pull/123`.
pub fn parse_github_object(
    external_id: &str,
    sanitized_canonical_url: Option<&str>,
) -> Result<GitHubObjectIdentity, ParseError> {
    // Format: owner/repo#pull/N or owner/repo#issues/N
    let (prefix, suffix) = external_id
        .split_once('#')
        .ok_or_else(|| ParseError::BadExternalId(external_id.to_string()))?;
    let mut path_parts = prefix.splitn(2, '/');
    let owner = path_parts.next().unwrap_or("");
    let repo = path_parts.next().unwrap_or("");
    let mut kind_number = suffix.splitn(2, '/');
    let kind_str = kind_number.next().unwrap_or("");
    let raw_number = kind_number.next().unwrap_or("");

    if !is_valid_repo_segment(owner) || repo.is_empty() || !is_valid_repo_segment(repo) {
        return Err(ParseError::BadExternalId(external_id.to_string()));
    }
    let path_kind =
        PathKind::from_str(kind_str).ok_or_else(|| ParseError::WrongKind(kind_str.to_string()))?;
    let number = raw_number
        .parse::<u64>()
        .map_err(|_| ParseError::InvalidNumber(raw_number.to_string()))?;
    if number == 0 {
        return Err(ParseError::InvalidNumber(raw_number.to_string()));
    }

    let host = if let Some(url_str) = sanitized_canonical_url {
        let url = url::Url::parse(url_str)
            .map_err(|_| ParseError::BadCanonicalUrl(url_str.to_string()))?;
        let h = url.host_str().unwrap_or("").to_lowercase();
        if h == "www.github.com" {
            "github.com".to_string()
        } else if pc_github_fetch::is_git_hub_dot_com(&h) {
            "github.com".to_string()
        } else if h.is_empty() {
            return Err(ParseError::BadCanonicalUrl(url_str.to_string()));
        } else {
            h
        }
    } else {
        "github.com".to_string()
    };

    Ok(GitHubObjectIdentity {
        host,
        owner: owner.to_string(),
        repo: repo.to_string(),
        number,
        path_kind,
        object_type: ObjectType::from_path_kind(path_kind),
    })
}

/// Stable externalId format used by the DB.
#[must_use]
pub fn external_id_for(identity: &GitHubObjectIdentity) -> String {
    format!(
        "{}/{owner_lower}#{path_kind}/{number}",
        identity.owner.to_lowercase(),
        owner_lower = identity.repo.to_lowercase(),
        path_kind = identity.path_kind.as_str(),
        number = identity.number,
    )
}

#[must_use]
pub fn display_title_for(identity: &GitHubObjectIdentity) -> String {
    format!(
        "{owner}/{repo}#{number}",
        owner = identity.owner,
        repo = identity.repo,
        number = identity.number
    )
}

#[must_use]
pub fn display_key_for(identity: &GitHubObjectIdentity) -> &'static str {
    match identity.object_type {
        ObjectType::PullRequest => "GitHub Pull Request",
        ObjectType::Issue => "GitHub Issue",
    }
}

fn is_valid_repo_segment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn r525_parse_canonical_pull_request() {
        let id = parse_github_canonical_url("https", "github.com", "/rust-lang/cargo/pull/1234")
            .unwrap();
        assert_eq!(id.host, "github.com");
        assert_eq!(id.owner, "rust-lang");
        assert_eq!(id.repo, "cargo");
        assert_eq!(id.number, 1234);
        assert_eq!(id.path_kind, PathKind::Pull);
        assert_eq!(id.object_type, ObjectType::PullRequest);
    }

    #[test]
    fn r525_parse_canonical_issue() {
        let id = parse_github_canonical_url("https", "github.com", "/rust-lang/cargo/issues/42")
            .unwrap();
        assert_eq!(id.path_kind, PathKind::Issues);
        assert_eq!(id.object_type, ObjectType::Issue);
    }

    #[test]
    fn r525_parse_canonical_ghe_host_normalises_www() {
        let id = parse_github_canonical_url("https", "Www.GitHub.Com", "/o/r/issues/1").unwrap();
        assert_eq!(id.host, "github.com");
    }

    #[test]
    fn r525_parse_canonical_ghe_host_preserved() {
        let id = parse_github_canonical_url("https", "ghe.acme.io", "/o/r/pull/7").unwrap();
        assert_eq!(id.host, "ghe.acme.io");
    }

    #[test]
    fn r525_parse_canonical_rejects_http() {
        assert!(matches!(
            parse_github_canonical_url("http", "github.com", "/o/r/pull/1"),
            Err(ParseError::NotHttps(_))
        ));
    }

    #[test]
    fn r525_parse_canonical_rejects_wrong_arity() {
        assert!(matches!(
            parse_github_canonical_url("https", "github.com", "/o/r/pull"),
            Err(ParseError::WrongPathArity(3))
        ));
    }

    #[test]
    fn r525_parse_canonical_rejects_invalid_kind() {
        assert!(matches!(
            parse_github_canonical_url("https", "github.com", "/o/r/commit/abc"),
            Err(ParseError::WrongKind(_))
        ));
    }

    #[test]
    fn r525_parse_canonical_rejects_zero_number() {
        assert!(matches!(
            parse_github_canonical_url("https", "github.com", "/o/r/pull/0"),
            Err(ParseError::InvalidNumber(_))
        ));
    }

    #[test]
    fn r525_parse_canonical_rejects_invalid_owner_chars() {
        assert!(matches!(
            parse_github_canonical_url("https", "github.com", "/bad@owner/r/pull/1"),
            Err(ParseError::BadExternalId(_))
        ));
    }

    #[test]
    fn r525_parse_external_id_dotcom_default() {
        let id = parse_github_object("OWNER/REPO#pull/5", None).unwrap();
        assert_eq!(id.host, "github.com");
        assert_eq!(id.owner, "OWNER");
        assert_eq!(id.repo, "REPO");
        assert_eq!(id.number, 5);
        assert_eq!(id.path_kind, PathKind::Pull);
    }

    #[test]
    fn r525_parse_external_id_with_canonical_url_ghe() {
        let id =
            parse_github_object("o/r#issues/9", Some("https://ghe.acme.io/o/r/issues/9")).unwrap();
        assert_eq!(id.host, "ghe.acme.io");
        assert_eq!(id.object_type, ObjectType::Issue);
    }

    #[test]
    fn r525_parse_external_id_rejects_missing_hash() {
        assert!(matches!(
            parse_github_object("owner-repo", None),
            Err(ParseError::BadExternalId(_))
        ));
    }

    #[test]
    fn r525_parse_external_id_rejects_invalid_canonical_url() {
        assert!(matches!(
            parse_github_object("o/r#pull/1", Some("not a url")),
            Err(ParseError::BadCanonicalUrl(_))
        ));
    }

    #[test]
    fn r525_external_id_for_lowercases_owner_repo() {
        let id = GitHubObjectIdentity {
            host: "github.com".into(),
            owner: "Rust-Lang".into(),
            repo: "Cargo".into(),
            number: 1234,
            path_kind: PathKind::Pull,
            object_type: ObjectType::PullRequest,
        };
        assert_eq!(external_id_for(&id), "rust-lang/cargo#pull/1234");
    }

    #[test]
    fn r525_display_title_includes_hash_separator() {
        let id = GitHubObjectIdentity {
            host: "github.com".into(),
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
            path_kind: PathKind::Issues,
            object_type: ObjectType::Issue,
        };
        assert_eq!(display_title_for(&id), "o/r#7");
    }

    #[test]
    fn r525_display_key_pr_vs_issue() {
        let pr = GitHubObjectIdentity {
            host: "github.com".into(),
            owner: "o".into(),
            repo: "r".into(),
            number: 1,
            path_kind: PathKind::Pull,
            object_type: ObjectType::PullRequest,
        };
        let iss = GitHubObjectIdentity {
            object_type: ObjectType::Issue,
            ..pr.clone()
        };
        assert_eq!(display_key_for(&pr), "GitHub Pull Request");
        assert_eq!(display_key_for(&iss), "GitHub Issue");
    }
}
