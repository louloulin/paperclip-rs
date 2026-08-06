//! `workspace_runtime_dev_service_recognition` — Paperclip 自身 dev service 识别。
//!
//! 与 Node `looksLikeWorkspaceDevServerCommand` /
//! `isPaperclipDevRuntimeService` / `resolveWorkspaceRuntimeReadinessTimeoutSec` /
//! `resolveRuntimeServiceHealthUrl` / `resolveShell` 1:1 对齐。
//!
//! 设计目标：纯函数模块；不读取真实环境变量或调用 IO。`resolveShell` 的
//! 输入是平台 + shell 路径字符串，由调用方负责传入。
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

// ============================================================================
// resolveShell
// ============================================================================

/// `resolveShell(platform, env_shell)`：解析可用 shell。
///
/// 与 Node 1:1 对齐：
/// - fallback: `process.platform === "win32" ? "sh" : "/bin/sh"`
/// - `SHELL` 不存在 → fallback
/// - `SHELL` 是绝对路径但文件不存在 → fallback
/// - 否则返回 `SHELL.trim()`
pub fn resolve_shell(
    platform_is_windows: bool,
    env_shell: Option<&str>,
    shell_exists: bool,
) -> String {
    let fallback = if platform_is_windows { "sh" } else { "/bin/sh" };
    let shell = env_shell.map(|s| s.trim()).unwrap_or("");
    if shell.is_empty() {
        return fallback.to_string();
    }
    if Path::new(shell).is_absolute() && !shell_exists {
        return fallback.to_string();
    }
    shell.to_string()
}

// ============================================================================
// looksLikeWorkspaceDevServerCommand
// ============================================================================

/// `looksLikeWorkspaceDevServerCommand(command)`：判断命令是否启动 dev server。
///
/// 与 Node 1:1 对齐：`(?:^|\s)(?:pnpm|npm|yarn|bun)\s+(?:run\s+)?dev(?:\s|$)`
pub fn looks_like_workspace_dev_server_command(command: &str) -> bool {
    let normalized = command.trim().to_lowercase();
    if normalized.is_empty() {
        return false;
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?:^|\s)(?:pnpm|npm|yarn|bun)\s+(?:run\s+)?dev(?:\s|$)").unwrap()
    });
    re.is_match(&normalized)
}

// ============================================================================
// isPaperclipDevRuntimeService
// ============================================================================

/// `isPaperclipDevRuntimeService(input)`：判断 service 是否是 Paperclip 自身 dev service。
///
/// 与 Node 1:1 对齐：
/// - serviceName ∈ {"paperclip-dev", "paperclip-dev-once"} → true
/// - command 同时包含 "dev:once" 和 "tailscale-auth" → true
/// - 否则 → false
pub fn is_paperclip_dev_runtime_service(service_name: Option<&str>, command: Option<&str>) -> bool {
    let service_name = service_name.unwrap_or("").trim().to_lowercase();
    let command = command.unwrap_or("").trim().to_lowercase();
    if service_name == "paperclip-dev" || service_name == "paperclip-dev-once" {
        return true;
    }
    command.contains("dev:once") && command.contains("tailscale-auth")
}

// ============================================================================
// resolveWorkspaceRuntimeReadinessTimeoutSec
// ============================================================================

/// `resolveWorkspaceRuntimeReadinessTimeoutSec(service)`：计算 readiness timeout。
///
/// 与 Node 1:1 对齐：
/// - readiness.timeoutSec > 0 → max(1, timeoutSec)
/// - 否则：dev server 命令 → 90；其它 → 30
pub fn resolve_workspace_runtime_readiness_timeout_sec(service: &Map<String, Value>) -> i64 {
    let readiness = service.get("readiness").and_then(|v| v.as_object());
    let explicit_timeout_sec = readiness
        .and_then(|r| r.get("timeoutSec").and_then(|v| v.as_i64()))
        .unwrap_or(0);
    if explicit_timeout_sec > 0 {
        return std::cmp::max(1, explicit_timeout_sec);
    }
    let command = service
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if looks_like_workspace_dev_server_command(command) {
        90
    } else {
        30
    }
}

// ============================================================================
// resolveRuntimeServiceHealthUrl
// ============================================================================

/// `resolveRuntimeServiceHealthUrl(url, serviceName, command)`：把 url 改写到 health endpoint。
///
/// 与 Node 1:1 对齐：
/// - url 为 null → null
/// - 不是 Paperclip dev service → 原 url
/// - URL parse 失败 → 原 url
/// - pathname 是 "/" 或 "" → pathname="/api/health", search="", hash=""
pub fn resolve_runtime_service_health_url(
    url: Option<&str>,
    service_name: Option<&str>,
    command: Option<&str>,
) -> Option<String> {
    let url = url?;
    if !is_paperclip_dev_runtime_service(service_name, command) {
        return Some(url.to_string());
    }
    let mut parsed = match parse_http_url(url) {
        Some(p) => p,
        None => return Some(url.to_string()),
    };
    if parsed.path == "/" || parsed.path.is_empty() {
        parsed.path = "/api/health".to_string();
        parsed.query = String::new();
        parsed.fragment = String::new();
        return Some(format_http_url(&parsed));
    }
    Some(url.to_string())
}

// ============================================================================
// 简单 URL 解析（仅支持 http/https）
// ============================================================================

#[derive(Debug, Default)]
struct ParsedUrl {
    scheme: String,
    host: String,
    port: Option<u16>,
    path: String,
    query: String,
    fragment: String,
}

/// 仅支持 `scheme://host[:port][/path][?query][#fragment]` 形式。
/// 解析失败返回 None。
fn parse_http_url(url: &str) -> Option<ParsedUrl> {
    let mut rest = url;
    // 1. scheme
    let scheme_end = rest.find("://")?;
    let scheme = rest[..scheme_end].to_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    rest = &rest[scheme_end + 3..];

    // 2. host[:port] — 遇到第一个 '/'、'?'、'#' 为止
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    rest = &rest[auth_end..];

    let (host, port) = if let Some(colon) = authority.rfind(':') {
        let port_str = &authority[colon + 1..];
        let port = port_str.parse::<u16>().ok()?;
        (authority[..colon].to_string(), Some(port))
    } else {
        (authority.to_string(), None)
    };
    if host.is_empty() {
        return None;
    }

    // 3. path
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    let path = rest[..path_end].to_string();
    rest = &rest[path_end..];

    // 4. query
    let (query, rest) = if let Some(q_start) = rest.strip_prefix('?').map(|_| 1) {
        let q_end_rel = rest[q_start..].find('#').unwrap_or(rest.len() - q_start);
        let q = rest[q_start..q_start + q_end_rel].to_string();
        (q, &rest[q_start + q_end_rel..])
    } else {
        (String::new(), rest)
    };

    // 5. fragment
    let fragment = rest.strip_prefix('#').unwrap_or("").to_string();

    Some(ParsedUrl {
        scheme,
        host,
        port,
        path,
        query,
        fragment,
    })
}

fn format_http_url(p: &ParsedUrl) -> String {
    let mut out = format!("{}://{}", p.scheme, p.host);
    if let Some(port) = p.port {
        out.push_str(&format!(":{}", port));
    }
    out.push_str(&p.path);
    if !p.query.is_empty() {
        out.push('?');
        out.push_str(&p.query);
    }
    if !p.fragment.is_empty() {
        out.push('#');
        out.push_str(&p.fragment);
    }
    out
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ----- resolve_shell -----

    #[test]
    fn resolve_shell_unix_with_valid_shell() {
        let s = resolve_shell(false, Some("/bin/zsh"), true);
        assert_eq!(s, "/bin/zsh");
    }

    #[test]
    fn resolve_shell_unix_missing_shell_returns_fallback() {
        let s = resolve_shell(false, None, false);
        assert_eq!(s, "/bin/sh");
    }

    #[test]
    fn resolve_shell_unix_absolute_but_missing_returns_fallback() {
        let s = resolve_shell(false, Some("/does/not/exist"), false);
        assert_eq!(s, "/bin/sh");
    }

    #[test]
    fn resolve_shell_windows_uses_sh_fallback() {
        let s = resolve_shell(true, None, false);
        assert_eq!(s, "sh");
    }

    #[test]
    fn resolve_shell_trims_whitespace() {
        let s = resolve_shell(false, Some("  /bin/zsh  "), true);
        assert_eq!(s, "/bin/zsh");
    }

    // ----- looks_like_workspace_dev_server_command -----

    #[test]
    fn dev_server_pnpm_run_dev() {
        assert!(looks_like_workspace_dev_server_command("pnpm run dev"));
        assert!(looks_like_workspace_dev_server_command("pnpm dev"));
    }

    #[test]
    fn dev_server_npm() {
        assert!(looks_like_workspace_dev_server_command("npm run dev"));
        assert!(looks_like_workspace_dev_server_command("yarn dev"));
        assert!(looks_like_workspace_dev_server_command("bun run dev"));
    }

    #[test]
    fn dev_server_with_prefix() {
        assert!(looks_like_workspace_dev_server_command(
            "cd /repo && pnpm dev"
        ));
        assert!(looks_like_workspace_dev_server_command(
            "npm run dev -- --port=3000"
        ));
    }

    #[test]
    fn dev_server_negative_cases() {
        assert!(!looks_like_workspace_dev_server_command(""));
        assert!(!looks_like_workspace_dev_server_command("pnpm build"));
        assert!(!looks_like_workspace_dev_server_command("pnpm install"));
        assert!(!looks_like_workspace_dev_server_command("node server.js"));
    }

    #[test]
    fn dev_server_case_insensitive() {
        assert!(looks_like_workspace_dev_server_command("PNPM RUN DEV"));
    }

    // ----- is_paperclip_dev_runtime_service -----

    #[test]
    fn paperclip_dev_by_service_name() {
        assert!(is_paperclip_dev_runtime_service(
            Some("paperclip-dev"),
            None
        ));
        assert!(is_paperclip_dev_runtime_service(
            Some("paperclip-dev-once"),
            None
        ));
    }

    #[test]
    fn paperclip_dev_by_command_pattern() {
        assert!(is_paperclip_dev_runtime_service(
            None,
            Some("npm run dev:once -- --tailscale-auth")
        ));
    }

    #[test]
    fn paperclip_dev_negative() {
        assert!(!is_paperclip_dev_runtime_service(Some("web"), None));
        assert!(!is_paperclip_dev_runtime_service(None, Some("npm start")));
    }

    // ----- resolve_workspace_runtime_readiness_timeout_sec -----

    #[test]
    fn readiness_timeout_explicit() {
        let mut s = Map::new();
        let mut readiness = Map::new();
        readiness.insert(
            "timeoutSec".into(),
            Value::Number(serde_json::Number::from(45)),
        );
        s.insert("readiness".into(), Value::Object(readiness));
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&s), 45);
    }

    #[test]
    fn readiness_timeout_explicit_min_one() {
        let mut s = Map::new();
        let mut readiness = Map::new();
        readiness.insert(
            "timeoutSec".into(),
            Value::Number(serde_json::Number::from(0)),
        );
        s.insert("readiness".into(), Value::Object(readiness));
        // 0 时不会进 max(1, ...) 分支，落在 dev server 检测
        s.insert("command".into(), Value::String("node server.js".into()));
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&s), 30);
    }

    #[test]
    fn readiness_timeout_dev_server_default_90() {
        let mut s = Map::new();
        s.insert("command".into(), Value::String("pnpm run dev".into()));
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&s), 90);
    }

    #[test]
    fn readiness_timeout_non_dev_default_30() {
        let mut s = Map::new();
        s.insert("command".into(), Value::String("node server.js".into()));
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&s), 30);
    }

    #[test]
    fn readiness_timeout_no_service_command_30() {
        let s = Map::new();
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&s), 30);
    }

    // ----- resolve_runtime_service_health_url -----

    #[test]
    fn health_url_non_paperclip_dev_returns_unchanged() {
        let url =
            resolve_runtime_service_health_url(Some("http://localhost:3000/"), Some("web"), None);
        assert_eq!(url, Some("http://localhost:3000/".to_string()));
    }

    #[test]
    fn health_url_paperclip_dev_root_rewrites() {
        let url = resolve_runtime_service_health_url(
            Some("http://localhost:3000/"),
            Some("paperclip-dev"),
            None,
        );
        assert_eq!(url, Some("http://localhost:3000/api/health".to_string()));
    }

    #[test]
    fn health_url_paperclip_dev_non_root_unchanged() {
        let url = resolve_runtime_service_health_url(
            Some("http://localhost:3000/foo"),
            Some("paperclip-dev"),
            None,
        );
        assert_eq!(url, Some("http://localhost:3000/foo".to_string()));
    }

    #[test]
    fn health_url_invalid_url_returns_unchanged() {
        let url =
            resolve_runtime_service_health_url(Some("not a url"), Some("paperclip-dev"), None);
        // url::Url::parse 失败 → 原 url 返回
        assert_eq!(url, Some("not a url".to_string()));
    }

    #[test]
    fn health_url_none_returns_none() {
        let url = resolve_runtime_service_health_url(None, None, None);
        assert!(url.is_none());
    }

    #[test]
    fn health_url_paperclip_dev_command_recognition() {
        let url = resolve_runtime_service_health_url(
            Some("http://localhost:3000/"),
            None,
            Some("npm run dev:once --tailscale-auth"),
        );
        assert_eq!(url, Some("http://localhost:3000/api/health".to_string()));
    }
}
