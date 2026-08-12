//! HTTP 成功日志静默策略 — 等价于 Node `middleware/http-log-policy.ts`。
//!
//! 用于避免高频轮询端点（health / activity / dashboard / heartbeat-runs /
//! issues / live-runs / sidebar-badges / run log）与静态资源刷屏访问日志。

/// GET/HEAD 成功响应才考虑静默（Node `SILENCED_SUCCESS_METHODS`）。
const SILENCED_SUCCESS_METHODS: [&str; 2] = ["GET", "HEAD"];

/// API 静默模式：段列表 + 通配（`*` 匹配恰好一个非空段）。
/// 对应 Node `SILENCED_SUCCESS_API_PATHS` 的正则（均以 `(?:\/|$)` 结尾）。
const SILENCED_SUCCESS_API_PATTERNS: [&[&str]; 8] = [
    &["api", "health"],
    &["api", "companies", "*", "activity"],
    &["api", "companies", "*", "dashboard"],
    &["api", "companies", "*", "heartbeat-runs"],
    &["api", "companies", "*", "issues"],
    &["api", "companies", "*", "live-runs"],
    &["api", "companies", "*", "sidebar-badges"],
    &["api", "heartbeat-runs", "*", "log"],
];

/// 静态资源前缀（Node `SILENCED_SUCCESS_STATIC_PREFIXES`）。
const SILENCED_SUCCESS_STATIC_PREFIXES: [&str; 8] = [
    "/@fs/",
    "/@id/",
    "/@react-refresh",
    "/@vite/",
    "/_plugins/",
    "/assets/",
    "/node_modules/",
    "/src/",
];

/// 静态资源精确路径（Node `SILENCED_SUCCESS_STATIC_PATHS`）。
const SILENCED_SUCCESS_STATIC_PATHS: [&str; 5] = [
    "/",
    "/index.html",
    "/favicon.ico",
    "/site.webmanifest",
    "/sw.js",
];

/// 归一化 URL：trim、去掉 query、空值回退 "/"（Node `normalizePath`）。
pub fn normalize_path(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return "/".to_string();
    }
    let pathname = trimmed.split("?").next().unwrap_or("").trim();
    if pathname.is_empty() {
        "/".to_string()
    } else {
        pathname.to_string()
    }
}

fn segments(pathname: &str) -> Vec<&str> {
    pathname.split("/").filter(|s| !s.is_empty()).collect()
}

/// 段模式匹配：`*` 匹配任意单段；匹配后允许更多段（对应 `(?:\/|$)` 结尾）。
fn api_pattern_matches(segs: &[&str], pattern: &[&str]) -> bool {
    if segs.len() < pattern.len() {
        return false;
    }
    segs.iter()
        .zip(pattern.iter())
        .all(|(seg, pat)| *pat == "*" || *seg == *pat)
}

/// 是否静默该成功日志（Node `shouldSilenceHttpSuccessLog`）。
pub fn should_silence_http_success_log(
    method: Option<&str>,
    url: Option<&str>,
    status_code: u16,
) -> bool {
    if status_code >= 400 {
        return false;
    }
    if status_code == 304 {
        return true;
    }
    let (Some(method), Some(url)) = (method, url) else {
        return false;
    };
    if !SILENCED_SUCCESS_METHODS.contains(&method.to_uppercase().as_str()) {
        return false;
    }
    let pathname = normalize_path(url);
    if SILENCED_SUCCESS_STATIC_PATHS.contains(&pathname.as_str()) {
        return true;
    }
    if SILENCED_SUCCESS_STATIC_PREFIXES
        .iter()
        .any(|prefix| pathname.starts_with(prefix))
    {
        return true;
    }
    let segs = segments(&pathname);
    SILENCED_SUCCESS_API_PATTERNS
        .iter()
        .any(|pattern| api_pattern_matches(&segs, pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_never_silenced() {
        assert!(!should_silence_http_success_log(
            Some("GET"),
            Some("/api/health"),
            500
        ));
        assert!(!should_silence_http_success_log(
            Some("GET"),
            Some("/api/health"),
            404
        ));
    }

    #[test]
    fn not_modified_always_silenced() {
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/api/x"),
            304
        ));
    }

    #[test]
    fn non_get_methods_not_silenced() {
        assert!(!should_silence_http_success_log(
            Some("POST"),
            Some("/"),
            200
        ));
        assert!(!should_silence_http_success_log(
            Some("OPTIONS"),
            Some("/"),
            200
        ));
    }

    #[test]
    fn static_paths_silenced() {
        assert!(should_silence_http_success_log(Some("GET"), Some("/"), 200));
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/index.html"),
            200
        ));
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/favicon.ico"),
            200
        ));
    }

    #[test]
    fn static_prefixes_silenced() {
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/assets/app.js"),
            200
        ));
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/src/main.tsx"),
            200
        ));
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/@vite/client"),
            200
        ));
    }

    #[test]
    fn api_health_silenced() {
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/api/health"),
            200
        ));
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/api/health?x=1"),
            200
        ));
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/api/health/deep"),
            200
        ));
    }

    #[test]
    fn company_scoped_patterns_silenced() {
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/api/companies/abc/activity"),
            200
        ));
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/api/companies/abc/issues?status=open"),
            200
        ));
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/api/companies/abc/heartbeat-runs"),
            200
        ));
        assert!(should_silence_http_success_log(
            Some("GET"),
            Some("/api/heartbeat-runs/run-1/log"),
            200
        ));
    }

    #[test]
    fn api_non_matching_not_silenced() {
        assert!(!should_silence_http_success_log(
            Some("GET"),
            Some("/api/companies"),
            200
        ));
        assert!(!should_silence_http_success_log(
            Some("GET"),
            Some("/api/companies/abc/activity-log"),
            200
        ));
        assert!(!should_silence_http_success_log(
            Some("GET"),
            Some("/api/runs"),
            200
        ));
    }

    #[test]
    fn normalize_path_rules() {
        assert_eq!(normalize_path("  /a/b?x=1  "), "/a/b");
        assert_eq!(normalize_path(""), "/");
        assert_eq!(normalize_path("   "), "/");
        assert_eq!(normalize_path("?q=1"), "/");
    }
}
