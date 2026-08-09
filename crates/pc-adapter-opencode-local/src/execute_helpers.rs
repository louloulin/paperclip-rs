//! Opencode-local execute 助手函数。
//!
//! 完整复刻 Node `packages/adapters/opencode-local/src/server/execute.ts`
//! 中与 model provider 解析、biller 解析、claude skills home 路径相关的纯函数。
//!
//! 通用 `parse_model_provider` / `parse_model_id` 复用 `pc_acpx::model_id`
//! （R422 抽取），与 pi-local 共享。

use std::collections::BTreeMap;

use pc_acpx::billing::infer_openai_compatible_biller;
use pc_acpx::model_id::parse_model_provider;

/// 解析 Opencode 的 biller（成本归属）。
///
/// Node 等价：`resolveOpenCodeBiller`。
/// - OpenAI-compatible hint 命中 → 例如 `"openrouter"` / `"openai"`。
/// - 否则 provider 字符串 → 例如 `"anthropic"`。
/// - 都没有 → `"unknown"`。
pub fn resolve_opencode_biller(env: &BTreeMap<String, String>, provider: Option<&str>) -> String {
    infer_openai_compatible_biller(env, None)
        .or_else(|| provider.map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// 解析 Opencode claude skills 目录路径。
///
/// Node 等价：`claudeSkillsHome`（注意：opencode 使用 `~/.claude/skills`，
/// 即使是 OpenCode 本地 CLI）。返回 `<homedir>/.claude/skills`。
pub fn claude_skills_home(homedir: &str) -> String {
    let home_trimmed = homedir.trim_end_matches('/');
    if home_trimmed.is_empty() || home_trimmed == "/" {
        return "/.claude/skills".to_owned();
    }
    format!("{home_trimmed}/.claude/skills")
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

    // -----------------------------------------------------------------
    // resolve_opencode_biller
    // -----------------------------------------------------------------

    #[test]
    fn biller_openrouter_优先() {
        let env = env_from(&[("OPENROUTER_API_KEY", "sk-or-test")]);
        assert_eq!(
            resolve_opencode_biller(&env, Some("anthropic")),
            "openrouter"
        );
    }

    #[test]
    fn biller_provider_fallback() {
        let env = env_from(&[]);
        assert_eq!(
            resolve_opencode_biller(&env, Some("anthropic")),
            "anthropic"
        );
    }

    #[test]
    fn biller_默认unknown() {
        let env = env_from(&[]);
        assert_eq!(resolve_opencode_biller(&env, None), "unknown");
    }

    #[test]
    fn biller_openrouter_env空不触发() {
        let env = env_from(&[("OPENROUTER_API_KEY", "")]);
        assert_eq!(resolve_opencode_biller(&env, Some("google")), "google");
    }

    // -----------------------------------------------------------------
    // claude_skills_home
    // -----------------------------------------------------------------

    #[test]
    fn skills_home_标准路径() {
        assert_eq!(claude_skills_home("/home/u"), "/home/u/.claude/skills");
    }

    #[test]
    fn skills_home_尾斜杠() {
        assert_eq!(claude_skills_home("/home/u/"), "/home/u/.claude/skills");
    }

    #[test]
    fn skills_home_根路径() {
        assert_eq!(claude_skills_home("/"), "/.claude/skills");
    }

    #[test]
    fn skills_home_空输入() {
        assert_eq!(claude_skills_home(""), "/.claude/skills");
    }

    // -----------------------------------------------------------------
    // pc_acpx::model_id 复用验证
    // -----------------------------------------------------------------

    #[test]
    fn provider_拆分通过pc_acpx() {
        // 验证 pc_acpx::model_id 行为符合预期。
        assert_eq!(
            parse_model_provider(Some("anthropic/claude-sonnet-4")),
            Some("anthropic".to_owned())
        );
        assert_eq!(
            parse_model_provider(Some("openai/gpt-4")),
            Some("openai".to_owned())
        );
        assert_eq!(parse_model_provider(None), None);
    }
}
