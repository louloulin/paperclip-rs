//! `pc-acpx::billing` — port of `billing.ts` from Node
//! `paperclip/packages/adapter-utils/src/`.
//!
//! Mirrors the single exported helper `inferOpenAiCompatibleBiller`:
//!
//! - [`infer_openai_compatible_biller`] resolves which biller
//!   (provider) an OpenAI-compatible agent run should be charged to
//!   based on env signals.
//!
//! The Node implementation prefers an explicit `OPENROUTER_API_KEY` and
//! falls back to scanning `OPENAI_BASE_URL` / `OPENAI_API_BASE` /
//! `OPENAI_API_BASE_URL` for an OpenRouter-style host. The port keeps
//! the same precedence and returns the fallback value when nothing
//! matches.

/// Resolve the OpenAI-compatible biller identifier based on environment
/// signals. Mirrors Node `inferOpenAiCompatibleBiller` exactly:
///
/// 1. If `OPENROUTER_API_KEY` is set (non-empty), the biller is
///    `"openrouter"`.
/// 2. Otherwise inspect `OPENAI_BASE_URL`, `OPENAI_API_BASE`, and
///    `OPENAI_API_BASE_URL`. If any of them matches `/openrouter\.ai/i`,
///    the biller is `"openrouter"`.
/// 3. Otherwise the caller-supplied fallback (default `"openai"`) is
///    returned.
#[must_use]
pub fn infer_openai_compatible_biller(
    env: &std::collections::BTreeMap<String, String>,
    fallback: Option<&str>,
) -> Option<String> {
    if let Some(value) = env.get("OPENROUTER_API_KEY") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some("openrouter".to_string());
        }
    }
    let openrouter_re = regex::Regex::new(r"(?i)openrouter\.ai").unwrap();
    for key in ["OPENAI_BASE_URL", "OPENAI_API_BASE", "OPENAI_API_BASE_URL"] {
        if let Some(value) = env.get(key) {
            if openrouter_re.is_match(value) {
                return Some("openrouter".to_string());
            }
        }
    }
    fallback.map(|value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn empty_env() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn returns_openrouter_when_explicit_api_key_set() {
        let mut env = empty_env();
        env.insert("OPENROUTER_API_KEY".to_string(), "sk-or-v1-xyz".to_string());
        assert_eq!(
            infer_openai_compatible_biller(&env, Some("openai")),
            Some("openrouter".to_string())
        );
    }

    #[test]
    fn explicit_key_wins_even_when_base_url_points_elsewhere() {
        let mut env = empty_env();
        env.insert("OPENROUTER_API_KEY".to_string(), "sk-or-v1-xyz".to_string());
        env.insert(
            "OPENAI_BASE_URL".to_string(),
            "https://api.example.com".to_string(),
        );
        assert_eq!(
            infer_openai_compatible_biller(&env, Some("openai")),
            Some("openrouter".to_string())
        );
    }

    #[test]
    fn empty_api_key_is_ignored() {
        let mut env = empty_env();
        env.insert("OPENROUTER_API_KEY".to_string(), "   ".to_string());
        assert_eq!(
            infer_openai_compatible_biller(&env, Some("openai")),
            Some("openai".to_string())
        );
    }

    #[test]
    fn base_url_openrouter_match_promotes_biller() {
        let mut env = empty_env();
        env.insert(
            "OPENAI_BASE_URL".to_string(),
            "https://openrouter.ai/api/v1".to_string(),
        );
        assert_eq!(
            infer_openai_compatible_biller(&env, Some("openai")),
            Some("openrouter".to_string())
        );
    }

    #[test]
    fn all_three_base_url_variants_are_inspected() {
        let mut env = empty_env();
        env.insert(
            "OPENAI_API_BASE_URL".to_string(),
            "https://openrouter.ai/api/v1".to_string(),
        );
        assert_eq!(
            infer_openai_compatible_biller(&env, Some("openai")),
            Some("openrouter".to_string())
        );

        let mut env = empty_env();
        env.insert(
            "OPENAI_API_BASE".to_string(),
            "https://openrouter.ai/api/v1".to_string(),
        );
        assert_eq!(
            infer_openai_compatible_biller(&env, Some("openai")),
            Some("openrouter".to_string())
        );
    }

    #[test]
    fn falls_back_when_no_signal_matches() {
        let mut env = empty_env();
        env.insert(
            "OPENAI_BASE_URL".to_string(),
            "https://api.openai.com".to_string(),
        );
        assert_eq!(
            infer_openai_compatible_biller(&env, Some("openai")),
            Some("openai".to_string())
        );
    }

    #[test]
    fn returns_none_when_no_signal_and_no_fallback() {
        let env = empty_env();
        assert_eq!(infer_openai_compatible_biller(&env, None), None);
    }

    #[test]
    fn case_insensitive_match_for_openrouter_host() {
        let mut env = empty_env();
        env.insert(
            "OPENAI_BASE_URL".to_string(),
            "https://OPENROUTER.AI/api/v1".to_string(),
        );
        assert_eq!(
            infer_openai_compatible_biller(&env, Some("openai")),
            Some("openrouter".to_string())
        );
    }
}
