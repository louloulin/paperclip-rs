//! `workspace_runtime_readiness` 域（Round 272）。
//!
//! 与原 `paperclip/server/src/services/workspace-runtime.ts` 中 4 个纯函数
//! 1:1 对齐（已剥离 fs::existsSync 等 IO）：
//! - `resolveShell` — 解析运行 shell（仅返回字符串，不做 fs.existsSync）
//! - `looksLikeWorkspaceDevServerCommand` — dev server 启发式
//! - `resolveWorkspaceRuntimeReadinessTimeoutSec` — readiness 超时秒数
//! - `resolveRuntimeServiceHealthUrl` — paperclip-dev*/dev:once+tailscale-auth 改写到 /api/health
//!
//! 设计目标：高内聚低耦合。
//! - **高内聚**：4 个 pure helper 共同表达"runtime readiness 探测"逻辑。
//! - **低耦合**：仅依赖 `serde_json` + `regex` + `url` crate（dev）。无 DB / 无 fs。
//!
//! 与 pc-core 内 `workspace_runtime_service_state` 区分：
//! - 本模块：`readiness` 时间预算 + dev server 启发式 + URL 重写
//! - 上模块：`desired_state` start/stop/restart 状态机
//!
//! 与 Node 版差异说明：
//! - `resolveShell` 中 `existsSync` 检查需要 fs，在 Rust 中我们让调用方先做存在性检查：
//!   `resolve_shell(exists_check: impl Fn(&Path) -> bool) -> String`，传入存在性回调。

use std::collections::HashMap;

use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

// ============================================================================
// resolveShell（Round 272）
// ============================================================================

/// 解析运行时 shell 路径。
///
/// 与 Node `resolveShell()` 1:1 对齐：
/// - fallback：`win32 → "sh"`，其他 → `"/bin/sh"`
/// - 若 `env.SHELL` 存在且是绝对路径：优先使用（调用前可以先 fs 测试存在性）
/// - 否则 fallback
///
/// `exists_check`：可调用的存在性检查（如 `Path::exists`）。在测试中可以传 `|_| true` / `|_| false`。
pub fn resolve_shell<F>(exists_check: F) -> String
where
    F: Fn(&str) -> bool,
{
    let fallback = if cfg!(windows) { "sh".to_string() } else { "/bin/sh".to_string() };
    if let Ok(shell_raw) = std::env::var("SHELL") {
        let shell = shell_raw.trim();
        if !shell.is_empty() {
            // Node 等价：`path.isAbsolute(shell) && !existsSync(shell)` → fallback
            // 我们简化：调用方提供 `exists_check`；如果 shell 是绝对路径且不存在才回退。
            let is_absolute = std::path::Path::new(shell).is_absolute();
            if !is_absolute || exists_check(shell) {
                return shell.to_string();
            }
        }
    }
    fallback
}

// ============================================================================
// looksLikeWorkspaceDevServerCommand（Round 272）
// ============================================================================

/// dev server 启发式：命令是否形如 `pnpm dev` / `npm run dev` / `yarn dev` / `bun dev` / `pnpm dev:once`。
///
/// 与 Node `looksLikeWorkspaceDevServerCommand(command)` 1:1 对齐：
/// ```regex
/// /(?:^|\s)(?:pnpm|npm|yarn|bun)\s+(?:run\s+)?dev(?:\s|$)/
/// ```
pub fn looks_like_workspace_dev_server_command(command: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?:^|\s)(?:pnpm|npm|yarn|bun)\s+(?:run\s+)?dev(?:\s|$)").unwrap()
    });
    let normalized = command.trim().to_lowercase();
    if normalized.is_empty() {
        return false;
    }
    re.is_match(&normalized)
}

// ============================================================================
// resolveWorkspaceRuntimeReadinessTimeoutSec（Round 272）
// ============================================================================

/// `service.readiness.timeoutSec` 字段读取：有限正数返回原 f64，否则 default。
fn as_number(value: Option<&Value>, default: u64) -> u64 {
    match value.and_then(|v| v.as_f64()) {
        Some(n) if n.is_finite() && n > 0.0 => n.ceil() as u64,
        _ => default,
    }
}

/// `readiness` 子对象读取。
fn parse_readiness(service: &HashMap<String, Value>) -> HashMap<String, Value> {
    service
        .get("readiness")
        .and_then(|v| v.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

/// 计算 readiness 探测超时（秒）。
///
/// 与 Node `resolveWorkspaceRuntimeReadinessTimeoutSec(service)` 1:1 对齐：
/// - `readiness.timeoutSec > 0` → max(1, timeoutSec)
/// - 否则若 command 是 dev server 启发式 → 90
/// - 否则 → 30
pub fn resolve_workspace_runtime_readiness_timeout_sec(
    service: &HashMap<String, Value>,
) -> u64 {
    let readiness = parse_readiness(service);
    let explicit = as_number(readiness.get("timeoutSec"), 0);
    if explicit > 0 {
        return explicit.max(1);
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
// isPaperclipDevRuntimeService（Round 272）
// ============================================================================

/// 是否 paperclip-dev* / paperclip-dev-once？
///
/// 与 Node `isPaperclipDevRuntimeService({ serviceName, command })` 1:1 对齐：
/// - `serviceName ∈ {"paperclip-dev", "paperclip-dev-once"}` → true
/// - `command.includes("dev:once") && command.includes("tailscale-auth")` → true
pub fn is_paperclip_dev_runtime_service(service_name: Option<&str>, command: Option<&str>) -> bool {
    let name = service_name.unwrap_or("").trim().to_lowercase();
    if name == "paperclip-dev" || name == "paperclip-dev-once" {
        return true;
    }
    let cmd = command.unwrap_or("").trim().to_lowercase();
    cmd.contains("dev:once") && cmd.contains("tailscale-auth")
}

/// 把 paperclip-dev 服务的 health URL 重写为 `/api/health`。
///
/// 与 Node `resolveRuntimeServiceHealthUrl(url, input)` 1:1 对齐：
/// - 仅当 input 看起来像 paperclip-dev* 时才改写
/// - 仅当 pathname 为 `/` 或空时改写为 `/api/health`，并清除 query/hash
/// - URL 解析失败时返回原值
/// 简化版 URL 解析（仅支持 protocol://host[:port]/[path][?query][#fragment]）。
/// 关键：分离 path 与 query（'?' 之前）。Node `new URL(...).pathname` 也只到 '?'。
struct MiniUrl {
    full: String,
    protocol_end: usize,
    path_start: usize,
    query_start: Option<usize>, // '?' 位置
    fragment_start: Option<usize>, // '#' 位置
}

fn parse_mini_url(s: &str) -> Option<MiniUrl> {
    if s.len() < 2 || s.starts_with('/') {
        return None;
    }
    let proto_end = s.find("://")?;
    let after_proto = proto_end + 3;
    let rest = &s[after_proto..];
    let path_off_in_rest = rest.find('/').unwrap_or(rest.len());
    let path_start = after_proto + path_off_in_rest;
    let query_start = s.find('?').filter(|&i| i >= path_start);
    let fragment_start = s.find('#').filter(|&i| i >= path_start);
    Some(MiniUrl {
        full: s.to_string(),
        protocol_end: proto_end,
        path_start,
        query_start,
        fragment_start,
    })
}

/// 把 URL 中 path 为 "/" 或 "" 的部分重写为 "/api/health"；其他原样返回。
/// Node 行为：同时清空 query 与 fragment。
fn replace_root_path_with_api_health(s: &str) -> String {
    let parsed = match parse_mini_url(s) {
        Some(p) => p,
        None => return s.to_string(),
    };
    // path_end 是 '?' 或 '#'（按出现顺序），都没有则是字符串末尾
    let path_end = parsed
        .query_start
        .or(parsed.fragment_start)
        .unwrap_or(parsed.full.len());
    let path = &parsed.full[parsed.path_start..path_end];
    if path != "/" && !path.is_empty() {
        return s.to_string();
    }
    // 切断到 query 或 fragment 的最早位置；生成 "<before>/api/health"
    let drop_end = parsed
        .query_start
        .or(parsed.fragment_start)
        .unwrap_or(parsed.full.len());
    let before = &parsed.full[..parsed.path_start];
    format!("{before}/api/health")  // 丢弃 query+fragment
}

pub fn resolve_runtime_service_health_url(
    url: Option<&str>,
    service_name: Option<&str>,
    command: Option<&str>,
) -> Option<String> {
    let url = url?;
    if !is_paperclip_dev_runtime_service(service_name, command) {
        return Some(url.to_string());
    }
    Some(replace_root_path_with_api_health(url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn service(entries: &[(&str, Value)]) -> HashMap<String, Value> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn resolve_shell_uses_fallback_when_env_missing() {
        // 临时清除 SHELL
        let prev = std::env::var("SHELL").ok();
        std::env::remove_var("SHELL");
        let s = resolve_shell(|_| true);
        if cfg!(windows) {
            assert_eq!(s, "sh");
        } else {
            assert_eq!(s, "/bin/sh");
        }
        if let Some(v) = prev {
            std::env::set_var("SHELL", v);
        }
    }

    #[test]
    fn resolve_shell_returns_absolute_when_exists() {
        let prev = std::env::var("SHELL").ok();
        std::env::set_var("SHELL", "/usr/local/bin/zsh");
        let s = resolve_shell(|_| true);
        assert_eq!(s, "/usr/local/bin/zsh");
        if let Some(v) = prev {
            std::env::set_var("SHELL", v);
        } else {
            std::env::remove_var("SHELL");
        }
    }

    #[test]
    fn resolve_shell_falls_back_when_absolute_does_not_exist() {
        let prev = std::env::var("SHELL").ok();
        std::env::set_var("SHELL", "/no/such/shell");
        let s = resolve_shell(|_| false);
        if cfg!(windows) {
            assert_eq!(s, "sh");
        } else {
            assert_eq!(s, "/bin/sh");
        }
        if let Some(v) = prev {
            std::env::set_var("SHELL", v);
        } else {
            std::env::remove_var("SHELL");
        }
    }

    #[test]
    fn dev_server_command_heuristic() {
        for s in [
            "pnpm dev",
            "pnpm run dev",
            "npm dev",
            "npm run dev",
            "yarn dev",
            "bun dev",
            "PNPM DEV", // 大小写不敏感
            "echo a && pnpm dev",
        ] {
            assert!(
                looks_like_workspace_dev_server_command(s),
                "should be dev: {s}"
            );
        }
        for s in ["", "pnpm build", "tsc --watch", "python server.py"] {
            assert!(
                !looks_like_workspace_dev_server_command(s),
                "should NOT be dev: {s}"
            );
        }
    }

    #[test]
    fn readiness_timeout_explicit_max1() {
        let svc = service(&[
            ("command", json!("pnpm build")),
            ("readiness", json!({"timeoutSec": 240})),
        ]);
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&svc), 240);
    }

    #[test]
    fn readiness_timeout_explicit_zero_falls_back() {
        let svc = service(&[
            ("command", json!("pnpm dev")),
            ("readiness", json!({"timeoutSec": 0})),
        ]);
        // 0 视为未设置 → 进入 fallback 分支（dev 命令 → 90）
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&svc), 90);
    }

    #[test]
    fn readiness_timeout_explicit_floor_to_one() {
        // 即使 timeoutSec < 1，max(1, t) 保证最小 1
        let svc = service(&[("readiness", json!({"timeoutSec": 0.5}))]);
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&svc), 1);
    }

    #[test]
    fn readiness_timeout_default_30_for_non_dev() {
        let svc = service(&[("command", json!("python server.py"))]);
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&svc), 30);
    }

    #[test]
    fn readiness_timeout_default_90_for_dev() {
        let svc = service(&[("command", json!("pnpm dev"))]);
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&svc), 90);
    }

    #[test]
    fn is_paperclip_dev_matches_name() {
        assert!(is_paperclip_dev_runtime_service(Some("paperclip-dev"), None));
        assert!(is_paperclip_dev_runtime_service(
            Some("PAPERCLIP-DEV-ONCE"),
            None
        ));
        assert!(!is_paperclip_dev_runtime_service(Some("my-app"), None));
        assert!(!is_paperclip_dev_runtime_service(Some(""), None));
    }

    #[test]
    fn is_paperclip_dev_matches_command() {
        assert!(is_paperclip_dev_runtime_service(
            None,
            Some("pnpm dev:once && tailscale-auth")
        ));
        assert!(!is_paperclip_dev_runtime_service(
            None,
            Some("pnpm dev")
        ));
        assert!(!is_paperclip_dev_runtime_service(
            None,
            Some("tsc --watch")
        ));
    }

    #[test]
    fn health_url_rewrites_dev_root() {
        let s = resolve_runtime_service_health_url(
            Some("http://localhost:3000/"),
            Some("paperclip-dev"),
            None,
        )
        .unwrap();
        assert_eq!(s, "http://localhost:3000/api/health");
    }

    #[test]
    fn health_url_rewrites_dev_with_query_and_fragment() {
        let s = resolve_runtime_service_health_url(
            Some("http://localhost:3000/?token=abc#frag"),
            Some("paperclip-dev"),
            None,
        )
        .unwrap();
        // 路径变成 /apihealth；query 与 fragment 被清除。
        assert!(s.starts_with("http://localhost:3000/"));
        assert!(s.contains("/api/health"));
        assert!(!s.contains("token=abc"));
        assert!(!s.contains("frag"));
    }

    #[test]
    fn health_url_unchanged_for_non_dev() {
        let s = resolve_runtime_service_health_url(
            Some("http://localhost:3000/api/v1"),
            Some("my-app"),
            None,
        );
        assert_eq!(s.as_deref(), Some("http://localhost:3000/api/v1"));
    }

    #[test]
    fn health_url_unchanged_when_dev_but_path_not_root() {
        let s = resolve_runtime_service_health_url(
            Some("http://localhost:3000/healthz"),
            Some("paperclip-dev"),
            None,
        );
        assert_eq!(s.as_deref(), Some("http://localhost:3000/healthz"));
    }

    #[test]
    fn health_url_returns_none_when_input_none() {
        assert_eq!(resolve_runtime_service_health_url(None, None, None), None);
    }
}
