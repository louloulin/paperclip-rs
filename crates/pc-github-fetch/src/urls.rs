//! Pure URL builders for GitHub / GitHub Enterprise.
//!
//! No IO, no reqwest — these functions are safe to call from sync code,
//! in tests, and at module-init time.

/// `true` iff `hostname` is the public `github.com` or its `www.` prefix.
#[must_use]
pub fn is_git_hub_dot_com(hostname: &str) -> bool {
    let lower = hostname.to_lowercase();
    lower == "github.com" || lower == "www.github.com"
}

/// Base URL for the REST API: `https://api.github.com` for dotcom,
/// `https://{host}/api/v3` for GitHub Enterprise.
#[must_use]
pub fn git_hub_api_base(hostname: &str) -> String {
    if is_git_hub_dot_com(hostname) {
        "https://api.github.com".to_string()
    } else {
        format!("https://{hostname}/api/v3")
    }
}

/// Build the raw file URL for `owner/repo/ref/path`.
///
/// Dotcom: `https://raw.githubusercontent.com/{owner}/{repo}/{ref}/{path}`
/// GHE:    `https://{hostname}/raw/{owner}/{repo}/{ref}/{path}`
///
/// Leading slashes in `file_path` are stripped (Node upstream behaviour).
#[must_use]
pub fn resolve_raw_git_hub_url(
    hostname: &str,
    owner: &str,
    repo: &str,
    ref_name: &str,
    file_path: &str,
) -> String {
    let p = file_path.trim_start_matches('/');
    if is_git_hub_dot_com(hostname) {
        format!("https://raw.githubusercontent.com/{owner}/{repo}/{ref_name}/{p}")
    } else {
        format!("https://{hostname}/raw/{owner}/{repo}/{ref_name}/{p}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r523_is_git_hub_dot_com_recognises_dotcom_and_www() {
        assert!(is_git_hub_dot_com("github.com"));
        assert!(is_git_hub_dot_com("www.github.com"));
        assert!(is_git_hub_dot_com("GITHUB.COM")); // case-insensitive
        assert!(is_git_hub_dot_com("Www.GitHub.Com"));
    }

    #[test]
    fn r523_is_git_hub_dot_com_rejects_enterprise() {
        assert!(!is_git_hub_dot_com("github.example.com"));
        assert!(!is_git_hub_dot_com("ghe.company.io"));
        assert!(!is_git_hub_dot_com(""));
        assert!(!is_git_hub_dot_com("api.github.com")); // API host ≠ web host
    }

    #[test]
    fn r523_git_hub_api_base_dotcom_returns_api_github_com() {
        assert_eq!(git_hub_api_base("github.com"), "https://api.github.com");
        assert_eq!(git_hub_api_base("www.github.com"), "https://api.github.com");
    }

    #[test]
    fn r523_git_hub_api_base_enterprise_uses_host_api_v3() {
        assert_eq!(
            git_hub_api_base("github.example.com"),
            "https://github.example.com/api/v3"
        );
        assert_eq!(
            git_hub_api_base("ghe.acme.io"),
            "https://ghe.acme.io/api/v3"
        );
    }

    #[test]
    fn r523_resolve_raw_dotcom_url() {
        assert_eq!(
            resolve_raw_git_hub_url("github.com", "rust-lang", "cargo", "main", "README.md"),
            "https://raw.githubusercontent.com/rust-lang/cargo/main/README.md"
        );
    }

    #[test]
    fn r523_resolve_raw_strips_leading_slashes_from_path() {
        assert_eq!(
            resolve_raw_git_hub_url("github.com", "o", "r", "v1", "/docs/intro.md"),
            "https://raw.githubusercontent.com/o/r/v1/docs/intro.md"
        );
        // Multiple leading slashes also stripped (Node upstream uses regex `/^\/+/`).
        assert_eq!(
            resolve_raw_git_hub_url("github.com", "o", "r", "v1", "////a.md"),
            "https://raw.githubusercontent.com/o/r/v1/a.md"
        );
    }

    #[test]
    fn r523_resolve_raw_enterprise_url() {
        assert_eq!(
            resolve_raw_git_hub_url("github.example.com", "o", "r", "main", "README.md"),
            "https://github.example.com/raw/o/r/main/README.md"
        );
    }

    #[test]
    fn r523_resolve_raw_with_ref_containing_slash() {
        // Branch names like "feature/foo" — kept as-is.
        assert_eq!(
            resolve_raw_git_hub_url("github.com", "o", "r", "feature/foo", "f.md"),
            "https://raw.githubusercontent.com/o/r/feature/foo/f.md"
        );
    }
}
