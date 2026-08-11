//! `pc-github-fetch` —— GitHub URL helpers + fetch wrapper。
//!
//! 1:1 port of Node `server/src/services/github-fetch.ts`（25 行）。
//! 下沉自 `pc-github-fetch` crate（原 crate 已删除）。
//!
//!
//! 设计目标：1:1 复刻 GitHub.com vs GitHub Enterprise 的 base URL 选择，
//! 并把 `fetch` 失败包装成本地错误（"无法连接到 hostname"）。
//!
//! ## 公共 API
//!
//! - [`is_github_dot_com`] / [`git_hub_api_base`] / [`resolve_raw_github_url`] — URL helpers
//! - [`GhFetchError`] / [`GhFetcher`] / [`gh_fetch`] — fetch 包装（注入式）

/// GitHub.com 域名集合（含 www）。
const GITHUB_DOT_COM_HOSTS: &[&str] = &["github.com", "www.github.com"];

/// 判断 hostname 是否为 github.com。
pub fn is_github_dot_com(hostname: &str) -> bool {
    GITHUB_DOT_COM_HOSTS.contains(&hostname.to_lowercase().as_str())
}

/// 取得 GitHub API base URL。
///
/// - `github.com` / `www.github.com` → `https://api.github.com`
/// - 其它（如 enterprise）→ `https://{hostname}/api/v3`
pub fn git_hub_api_base(hostname: &str) -> String {
    if is_github_dot_com(hostname) {
        "https://api.github.com".to_string()
    } else {
        format!("https://{}/api/v3", hostname)
    }
}

/// 解析 raw.githubusercontent.com / enterprise raw URL。
///
/// - github.com → `https://raw.githubusercontent.com/{owner}/{repo}/{ref}/{path}`
/// - 其它 → `https://{hostname}/raw/{owner}/{repo}/{ref}/{path}`
pub fn resolve_raw_github_url(
    hostname: &str,
    owner: &str,
    repo: &str,
    ref_: &str,
    file_path: &str,
) -> String {
    let p = file_path.trim_start_matches('/');
    if is_github_dot_com(hostname) {
        format!(
            "https://raw.githubusercontent.com/{}/{}/{}/{}",
            owner, repo, ref_, p
        )
    } else {
        format!("https://{}/raw/{}/{}/{}/{}", hostname, owner, repo, ref_, p)
    }
}

/// `gh_fetch` 调用错误 —— 与 Node `unprocessable(...)` 抛错 1:1 对齐。
#[derive(Debug, thiserror::Error)]
pub enum GhFetchError {
    #[error("Could not connect to {hostname} — ensure the URL points to a GitHub or GitHub Enterprise instance")]
    CannotConnect { hostname: String },
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
}

/// `GhFetcher` trait —— service 层注入实际 fetch 实现（HTTP 客户端 / mock）。
#[async_trait::async_trait]
pub trait GhFetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<String, GhFetchError>;
}

/// 顶层 `ghFetch` 包装：
/// - 解析 URL
/// - 调注入的 fetcher
/// - 失败时返回 [`GhFetchError::CannotConnect`]（带 hostname）
pub async fn gh_fetch<F: GhFetcher + ?Sized>(
    fetcher: &F,
    url: &str,
) -> Result<String, GhFetchError> {
    let parsed = url::Url::parse(url).map_err(|e| GhFetchError::InvalidUrl(e.to_string()))?;
    let url_hostname = parsed.host_str().unwrap_or("").to_string();
    match fetcher.fetch(url).await {
        Ok(body) => Ok(body),
        Err(GhFetchError::CannotConnect { hostname }) => {
            // 透传内层 hostname
            Err(GhFetchError::CannotConnect { hostname })
        }
        Err(_) => {
            // 其它错误（如 InvalidUrl）也包装为 CannotConnect，使用 URL hostname
            Err(GhFetchError::CannotConnect {
                hostname: url_hostname,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn r691_is_github_dot_com_matches() {
        assert!(is_github_dot_com("github.com"));
        assert!(is_github_dot_com("www.github.com"));
        assert!(is_github_dot_com("GITHUB.COM"));
        assert!(is_github_dot_com("WwW.GitHub.Com"));
    }

    #[test]
    fn r691_is_github_dot_com_rejects_enterprise() {
        assert!(!is_github_dot_com("github.enterprise.local"));
        assert!(!is_github_dot_com("api.github.com"));
        assert!(!is_github_dot_com("ghe.example.com"));
    }

    #[test]
    fn r691_git_hub_api_base_dot_com() {
        assert_eq!(git_hub_api_base("github.com"), "https://api.github.com");
        assert_eq!(git_hub_api_base("WWW.GITHUB.COM"), "https://api.github.com");
    }

    #[test]
    fn r691_git_hub_api_base_enterprise() {
        assert_eq!(
            git_hub_api_base("ghe.example.com"),
            "https://ghe.example.com/api/v3"
        );
    }

    #[test]
    fn r691_resolve_raw_github_url_dot_com() {
        let u = resolve_raw_github_url("github.com", "rust-lang", "rust", "main", "README.md");
        assert_eq!(
            u,
            "https://raw.githubusercontent.com/rust-lang/rust/main/README.md"
        );
    }

    #[test]
    fn r691_resolve_raw_github_url_dot_com_strips_leading_slash() {
        let u = resolve_raw_github_url("github.com", "o", "r", "main", "/src/lib.rs");
        assert_eq!(u, "https://raw.githubusercontent.com/o/r/main/src/lib.rs");
    }

    #[test]
    fn r691_resolve_raw_github_url_enterprise() {
        let u = resolve_raw_github_url("ghe.example.com", "o", "r", "v1.0", "README.md");
        assert_eq!(u, "https://ghe.example.com/raw/o/r/v1.0/README.md");
    }

    struct MockFetcher {
        should_fail: bool,
        captured: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl GhFetcher for MockFetcher {
        async fn fetch(&self, url: &str) -> Result<String, GhFetchError> {
            self.captured.lock().unwrap().push(url.to_string());
            if self.should_fail {
                Err(GhFetchError::CannotConnect {
                    hostname: "unreachable".to_string(),
                })
            } else {
                Ok("body".to_string())
            }
        }
    }

    #[tokio::test]
    async fn r691_gh_fetch_ok() {
        let m = MockFetcher {
            should_fail: false,
            captured: Mutex::new(Vec::new()),
        };
        let body = gh_fetch(&m, "https://api.github.com/repos/foo/bar")
            .await
            .unwrap();
        assert_eq!(body, "body");
        assert_eq!(
            *m.captured.lock().unwrap(),
            vec!["https://api.github.com/repos/foo/bar".to_string()]
        );
    }

    #[tokio::test]
    async fn r691_gh_fetch_connection_error_carries_inner_hostname() {
        let m = MockFetcher {
            should_fail: true,
            captured: Mutex::new(Vec::new()),
        };
        let err = gh_fetch(&m, "https://ghe.example.com/api/v3/repos")
            .await
            .unwrap_err();
        match err {
            GhFetchError::CannotConnect { hostname } => {
                // 透传 inner hostname 优先
                assert_eq!(hostname, "unreachable");
            }
            other => panic!("expected CannotConnect, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn r691_gh_fetch_invalid_url() {
        let m = MockFetcher {
            should_fail: false,
            captured: Mutex::new(Vec::new()),
        };
        let err = gh_fetch(&m, "not a url").await.unwrap_err();
        assert!(matches!(err, GhFetchError::InvalidUrl(_)));
    }

    #[tokio::test]
    async fn r691_gh_fetch_other_error_falls_back_to_url_hostname() {
        struct FailWithInvalid;
        #[async_trait::async_trait]
        impl GhFetcher for FailWithInvalid {
            async fn fetch(&self, _url: &str) -> Result<String, GhFetchError> {
                Err(GhFetchError::InvalidUrl("inner".into()))
            }
        }
        let err = gh_fetch(&FailWithInvalid, "https://example.com/x")
            .await
            .unwrap_err();
        match err {
            GhFetchError::CannotConnect { hostname } => {
                assert_eq!(hostname, "example.com");
            }
            other => panic!("expected CannotConnect, got {:?}", other),
        }
    }
}
