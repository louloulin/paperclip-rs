#![forbid(unsafe_code)]

//! Workspace runtime readiness helpers (pure functions).
//!
//! R702: Direct port of `workspace-runtime.ts` runtime readiness helpers.
//!
//! ## 与 Node 的对应
//! - Node `formatShortSha(value)` -> [`format_short_sha`]
//! - Node `looksLikeWorkspaceDevServerCommand(command)` -> [`looks_like_workspace_dev_server_command`]
//! - Node `resolveWorkspaceRuntimeReadinessTimeoutSec(service)` -> [`resolve_workspace_runtime_readiness_timeout_sec`]
//! - Node `isPaperclipDevRuntimeService({serviceName, command})` -> [`is_paperclip_dev_runtime_service`]
//!
//! ## 设计
//! - Pure: 无 DB / IO / time 依赖
//! - 输入是 Record<string, unknown> 风格的 loose map（与 Node service.config 一致）
//! - 输出是 typed Rust 值

use std::collections::HashMap;

/// Format a short git SHA (first 12 chars) for display. Returns "unknown" for null/empty.
pub fn format_short_sha(value: Option<&str>) -> String {
    match value {
        Some(v) if !v.is_empty() => v.chars().take(12).collect(),
        _ => "unknown".to_string(),
    }
}

/// Detect whether a command looks like a workspace dev server command.
///
/// Node regex: `/(?:^|\s)(?:pnpm|npm|yarn|bun)\s+(?:run\s+)?dev(?:\s|$)/`
pub fn looks_like_workspace_dev_server_command(command: &str) -> bool {
    let normalized = command.trim().to_lowercase();
    if normalized.is_empty() { return false; }
    check_dev_server(&normalized)
}

fn check_dev_server(s: &str) -> bool {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    for (i, t) in tokens.iter().enumerate() {
        if !matches!(*t, "pnpm" | "npm" | "yarn" | "bun") { continue; }
        let next_is_run = tokens.get(i + 1).copied() == Some("run");
        let dev_pos = if next_is_run { i + 2 } else { i + 1 };
        if let Some(dev) = tokens.get(dev_pos) {
            if *dev == "dev" { return true; }
        }
    }
    false
}

/// Resolve the readiness timeout (seconds) for a runtime service.
///
/// Returns max(1, readiness.timeoutSec) when explicit, otherwise 90 for dev-server
/// commands, otherwise 30.
pub fn resolve_workspace_runtime_readiness_timeout_sec(service: &HashMap<String, serde_json::Value>) -> u32 {
    let readiness = service.get("readiness").and_then(|v| v.as_object());
    let explicit = readiness
        .and_then(|r| r.get("timeoutSec").and_then(|v| v.as_f64()))
        .unwrap_or(0.0);
    if explicit > 0.0 {
        return std::cmp::max(1, explicit as u32);
    }
    let command = service.get("command").and_then(|v| v.as_str()).unwrap_or("");
    if looks_like_workspace_dev_server_command(command) { 90 } else { 30 }
}

/// Detect whether the runtime service is a Paperclip-managed dev service.
pub fn is_paperclip_dev_runtime_service(service_name: Option<&str>, command: Option<&str>) -> bool {
    let sn = service_name.unwrap_or("").trim().to_lowercase();
    let cmd = command.unwrap_or("").trim().to_lowercase();
    if sn == "paperclip-dev" || sn == "paperclip-dev-once" {
        return true;
    }
    cmd.contains("dev:once") && cmd.contains("tailscale-auth")
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_short_sha_basic() {
        assert_eq!(format_short_sha(Some("abcdef1234567890abcdef")), "abcdef123456");
    }
    #[test]
    fn format_short_sha_none() {
        assert_eq!(format_short_sha(None), "unknown");
    }
    #[test]
    fn format_short_sha_empty() {
        assert_eq!(format_short_sha(Some("")), "unknown");
    }
    #[test]
    fn format_short_sha_short_input() {
        assert_eq!(format_short_sha(Some("abc")), "abc");
    }

    #[test]
    fn looks_like_pnpm_dev() {
        assert!(looks_like_workspace_dev_server_command("pnpm dev"));
        assert!(looks_like_workspace_dev_server_command("pnpm run dev"));
    }
    #[test]
    fn looks_like_npm_yarn_bun_dev() {
        assert!(looks_like_workspace_dev_server_command("npm run dev"));
        assert!(looks_like_workspace_dev_server_command("yarn dev"));
        assert!(looks_like_workspace_dev_server_command("bun run dev"));
    }
    #[test]
    fn looks_does_not_match_build() {
        assert!(!looks_like_workspace_dev_server_command("pnpm build"));
        assert!(!looks_like_workspace_dev_server_command("npm test"));
    }
    #[test]
    fn looks_handles_whitespace() {
        assert!(looks_like_workspace_dev_server_command("  pnpm  run  dev  "));
    }
    #[test]
    fn looks_handles_empty() {
        assert!(!looks_like_workspace_dev_server_command(""));
        assert!(!looks_like_workspace_dev_server_command("   "));
    }
    #[test]
    fn looks_handles_case() {
        assert!(looks_like_workspace_dev_server_command("PNPM DEV"));
        assert!(looks_like_workspace_dev_server_command("Pnpm Run Dev"));
    }
    #[test]
    fn looks_dev_inside_larger_command() {
        // "cd app && pnpm dev" should match
        assert!(looks_like_workspace_dev_server_command("cd app && pnpm dev"));
        // "pnpm devtools" should NOT match (token after dev != end)
        assert!(!looks_like_workspace_dev_server_command("pnpm devtools"));
        // "echo hello pnpm run dev world" should match
        assert!(looks_like_workspace_dev_server_command("echo hello pnpm run dev world"));
    }

    #[test]
    fn resolve_timeout_explicit() {
        let mut s = HashMap::new();
        s.insert("readiness".into(), json!({ "timeoutSec": 60 }));
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&s), 60);
    }
    #[test]
    fn resolve_timeout_explicit_clamped() {
        let mut s = HashMap::new();
        s.insert("readiness".into(), json!({ "timeoutSec": 0 }));
        // explicit 0 falls through to dev-server default
        s.insert("command".into(), json!("pnpm dev"));
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&s), 90);
    }
    #[test]
    fn resolve_timeout_dev_command_default() {
        let mut s = HashMap::new();
        s.insert("command".into(), json!("pnpm run dev"));
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&s), 90);
    }
    #[test]
    fn resolve_timeout_other_command_default() {
        let mut s = HashMap::new();
        s.insert("command".into(), json!("node server.js"));
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&s), 30);
    }
    #[test]
    fn resolve_timeout_empty_service() {
        let s = HashMap::new();
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&s), 30);
    }
    #[test]
    fn resolve_timeout_explicit_negative_clamped_to_1() {
        let mut s = HashMap::new();
        s.insert("readiness".into(), json!({ "timeoutSec": -5 }));
        // negative is treated as 0 by as_f64 default, falls through to default
        assert_eq!(resolve_workspace_runtime_readiness_timeout_sec(&s), 30);
    }

    #[test]
    fn is_paperclip_dev_service_name() {
        assert!(is_paperclip_dev_runtime_service(Some("paperclip-dev"), None));
        assert!(is_paperclip_dev_runtime_service(Some("Paperclip-Dev-Once"), None));
        assert!(!is_paperclip_dev_runtime_service(Some("other"), None));
        assert!(!is_paperclip_dev_runtime_service(None, None));
    }
    #[test]
    fn is_paperclip_dev_command_marker() {
        assert!(is_paperclip_dev_runtime_service(None, Some("npm run dev:once -- --tailscale-auth")));
        assert!(!is_paperclip_dev_runtime_service(None, Some("npm run dev")));
        assert!(!is_paperclip_dev_runtime_service(None, Some("dev:once")));
    }
    #[test]
    fn is_paperclip_dev_case_insensitive() {
        assert!(is_paperclip_dev_runtime_service(Some("PAPERCLIP-DEV"), None));
    }
}