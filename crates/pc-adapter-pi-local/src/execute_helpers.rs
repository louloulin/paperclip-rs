//! Pi-local execute 助手函数。
//!
//! 完整复刻 Node `packages/adapters/pi-local/src/server/execute.ts` 中
//! 与 session 解析、模型拆分、biller 解析、resume 决策相关的纯函数。
//! 这些都是高 ROI、可独立测试、与 fs / runtime 解耦的小函数。

use std::collections::BTreeMap;

pub use pc_acpx::paths::{cwds_match, normalize_cwd};
use pc_acpx::{billing::infer_openai_compatible_biller, paths};

use crate::pi_stream_json::is_pi_unknown_session_error;

/// 解析 "provider/model" 形式的 provider 前缀。
///
/// Node 等价：`parseModelProvider`。无 `/` 或全空白返回 `None`。
pub fn model_provider(model: Option<&str>) -> Option<String> {
    let trimmed = model?.trim();
    if !trimmed.contains('/') {
        return None;
    }
    let idx = trimmed.find('/').unwrap();
    let prefix = trimmed[..idx].trim();
    if prefix.is_empty() {
        None
    } else {
        Some(prefix.to_owned())
    }
}

/// 解析 "provider/model" 形式的 model id。
///
/// Node 等价：`parseModelId`。无 `/` 时返回整串作为 model id。
pub fn model_id(model: Option<&str>) -> Option<String> {
    let trimmed = model?.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains('/') {
        return Some(trimmed.to_owned());
    }
    let idx = trimmed.find('/').unwrap();
    let suffix = trimmed[idx + 1..].trim();
    if suffix.is_empty() {
        None
    } else {
        Some(suffix.to_owned())
    }
}

/// 解析 biller：env 中 OpenAI 兼容 hint 优先，否则 fallback 到 provider，最后 "unknown"。
///
/// Node 等价：`resolvePiBiller`。
pub fn resolve_pi_biller(
    env: &BTreeMap<String, String>,
    provider: Option<&str>,
) -> String {
    infer_openai_compatible_biller(env, None)
        .or_else(|| provider.map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// 读取 session 文件第一行 JSON 头里的 cwd（用于 resume 决策）。
///
/// Node 等价：`readSessionHeaderCwd`。第一行必须是 `{ "type": "session", "cwd": "..." }`。
pub fn parse_session_header_cwd(raw: &str) -> Option<String> {
    let header_line = raw
        .split('\n')
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let value: serde_json::Value = serde_json::from_str(header_line).ok()?;
    if value.get("type").and_then(serde_json::Value::as_str) != Some("session") {
        return None;
    }
    let cwd = value.get("cwd").and_then(serde_json::Value::as_str)?;
    let trimmed = cwd.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// 决策：stdout/stderr 是否触发 clear_session。
///
/// 等价 Node 的 `isPiUnknownSessionError(stdout, stderr)` 调用。
pub fn should_clear_session(stdout: &str, stderr: &str) -> bool {
    is_pi_unknown_session_error(stdout, stderr)
}

/// 决策：是否可 resume（saved_cwd 与 current_cwd 匹配且 saved_cwd 非空）。
///
/// Node 等价：saved cwd 与 current cwd 比较后置 `canResumeSession = true`。
pub fn should_resume(saved_cwd: Option<&str>, current_cwd: &str) -> bool {
    match saved_cwd {
        Some(saved) if !saved.trim().is_empty() => paths::cwds_match(saved, current_cwd),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn model_provider_拆分() {
        assert_eq!(
            model_provider(Some("anthropic/claude-sonnet-4")),
            Some("anthropic".to_owned())
        );
        assert_eq!(
            model_provider(Some("openai / gpt-5")),
            Some("openai".to_owned())
        );
    }

    #[test]
    fn model_provider_无斜杠返回None() {
        assert_eq!(model_provider(Some("claude-sonnet-4")), None);
        assert_eq!(model_provider(None), None);
        assert_eq!(model_provider(Some("")), None);
        assert_eq!(model_provider(Some("   ")), None);
    }

    #[test]
    fn model_provider_空前缀返回None() {
        // "/model" — provider 部分为空
        assert_eq!(model_provider(Some("/claude")), None);
    }

    #[test]
    fn model_id_拆分() {
        assert_eq!(
            model_id(Some("anthropic/claude-sonnet-4")),
            Some("claude-sonnet-4".to_owned())
        );
    }

    #[test]
    fn model_id_无斜杠返回整串() {
        assert_eq!(
            model_id(Some("claude-sonnet-4")),
            Some("claude-sonnet-4".to_owned())
        );
    }

    #[test]
    fn model_id_空后缀返回None() {
        assert_eq!(model_id(Some("anthropic/")), None);
    }

    #[test]
    fn model_id_空白输入返回None() {
        assert_eq!(model_id(None), None);
        assert_eq!(model_id(Some("")), None);
        assert_eq!(model_id(Some("   ")), None);
    }

    #[test]
    fn resolve_pi_biller_默认unknown() {
        let env = env_from(&[]);
        assert_eq!(resolve_pi_biller(&env, None), "unknown");
        assert_eq!(resolve_pi_biller(&env, Some("anthropic")), "anthropic");
    }

    #[test]
    fn resolve_pi_biller_openrouter_env优先() {
        let env = env_from(&[("OPENROUTER_API_KEY", "sk-or-test")]);
        assert_eq!(resolve_pi_biller(&env, Some("anthropic")), "openrouter");
    }

    #[test]
    fn resolve_pi_biller_provider作为fallback() {
        let env = env_from(&[]);
        assert_eq!(resolve_pi_biller(&env, Some("google")), "google");
    }

    #[test]
    fn parse_session_header_cwd_合法() {
        let raw = "{\"type\":\"session\",\"cwd\":\"/home/u/proj\",\"timestamp\":\"2026-08-08T00:00:00Z\"}\n";
        assert_eq!(parse_session_header_cwd(raw).as_deref(), Some("/home/u/proj"));
    }

    #[test]
    fn parse_session_header_cwd_非session_type() {
        let raw = "{\"type\":\"message\",\"cwd\":\"/x\"}\n";
        assert_eq!(parse_session_header_cwd(raw), None);
    }

    #[test]
    fn parse_session_header_cwd_损坏JSON返回None() {
        assert_eq!(parse_session_header_cwd("not-json\n"), None);
    }

    #[test]
    fn parse_session_header_cwd_空cwd返回None() {
        let raw = "{\"type\":\"session\",\"cwd\":\"\"}\n";
        assert_eq!(parse_session_header_cwd(raw), None);
    }

    #[test]
    fn parse_session_header_cwd_空输入返回None() {
        assert_eq!(parse_session_header_cwd(""), None);
        assert_eq!(parse_session_header_cwd("\n\n\n"), None);
    }

    #[test]
    fn parse_session_header_cwd_忽略前导空行() {
        let raw = "\n\n  {\"type\":\"session\",\"cwd\":\"/a\"}  \n";
        assert_eq!(parse_session_header_cwd(raw).as_deref(), Some("/a"));
    }

    #[test]
    fn should_clear_session_触发() {
        assert!(should_clear_session("", "unknown session id: abc"));
        assert!(should_clear_session("", "Session not found"));
        assert!(should_clear_session("stdout x", "no session"));
        assert!(should_clear_session("session abc not found", ""));
    }

    #[test]
    fn should_clear_session_正常文本() {
        assert!(!should_clear_session("ok", ""));
        assert!(!should_clear_session("", ""));
    }

    #[test]
    fn should_resume_cwd匹配() {
        assert!(should_resume(Some("/home/u/proj"), "/home/u/proj"));
        assert!(should_resume(Some("/home/u/proj/."), "/home/u/proj"));
        assert!(!should_resume(Some("/home/u/proj/sub"), "/home/u/proj/sub/.."));
    }

    #[test]
    fn should_resume_cwd不匹配() {
        assert!(!should_resume(Some("/home/u/proj"), "/home/u/other"));
        assert!(!should_resume(Some("/home/u/proj"), "/home/u/proj/sub"));
    }

    #[test]
    fn should_resume_saved_cwd为空() {
        assert!(!should_resume(None, "/any"));
        assert!(!should_resume(Some(""), "/any"));
        assert!(!should_resume(Some("   "), "/any"));
    }
}
