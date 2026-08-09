//! R422 — Integration tests for `pc-adapter-opencode-local::execute_helpers`.
//!
//! Mirrors Node `packages/adapters/opencode-local/src/server/execute.ts`:
//! - `parseModelProvider` (L70-75) — 复用 `pc_acpx::model_id`
//! - `resolveOpenCodeBiller` (L77-79)
//! - `claudeSkillsHome` (L155-157)
//!
//! Unit tests inside `execute_helpers::tests` cover each function in isolation;
//! this integration suite verifies the complete helper API surface end-to-end.

use pc_acpx::model_id::parse_model_provider;
use pc_adapter_opencode_local::{claude_skills_home, resolve_opencode_biller};
use std::collections::BTreeMap;

fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

// ---------------------------------------------------------------------------
// resolve_opencode_biller
// ---------------------------------------------------------------------------

#[test]
fn biller_openrouter_优先() {
    let env = env_from(&[("OPENROUTER_API_KEY", "sk-or-test")]);
    assert_eq!(
        resolve_opencode_biller(&env, Some("anthropic")),
        "openrouter"
    );
}

#[test]
fn biller_provider作为fallback() {
    let env = env_from(&[]);
    assert_eq!(
        resolve_opencode_biller(&env, Some("anthropic")),
        "anthropic"
    );
    assert_eq!(resolve_opencode_biller(&env, Some("google")), "google");
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

#[test]
fn biller_openrouter_via_base_url() {
    let env = env_from(&[("OPENAI_BASE_URL", "https://openrouter.ai/api/v1")]);
    assert_eq!(
        resolve_opencode_biller(&env, Some("anthropic")),
        "openrouter"
    );
}

// ---------------------------------------------------------------------------
// claude_skills_home
// ---------------------------------------------------------------------------

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

#[test]
fn skills_home_opencode使用claude_skill店() {
    // OpenCode 本地 CLI 把 skill 注入到 ~/.claude/skills（与 Claude 本地共享）。
    assert!(claude_skills_home("/home/u").ends_with(".claude/skills"));
}

// ---------------------------------------------------------------------------
// parse_model_provider (pc_acpx 复用)
// ---------------------------------------------------------------------------

#[test]
fn provider_标准拆分() {
    assert_eq!(
        parse_model_provider(Some("anthropic/claude-sonnet-4")),
        Some("anthropic".to_owned())
    );
    assert_eq!(
        parse_model_provider(Some("openai/gpt-4")),
        Some("openai".to_owned())
    );
}

#[test]
fn provider_无斜杠_None() {
    assert_eq!(parse_model_provider(Some("claude-sonnet-4")), None);
}

#[test]
fn provider_空输入_None() {
    assert_eq!(parse_model_provider(None), None);
    assert_eq!(parse_model_provider(Some("")), None);
    assert_eq!(parse_model_provider(Some("   ")), None);
}

#[test]
fn provider_空前缀_None() {
    assert_eq!(parse_model_provider(Some("/model")), None);
}

#[test]
fn provider_多斜杠只切第一个() {
    assert_eq!(parse_model_provider(Some("a/b/c")), Some("a".to_owned()));
}

// ---------------------------------------------------------------------------
// 综合场景
// ---------------------------------------------------------------------------

#[test]
fn 综合_企业_anthropic_via_openrouter() {
    let env = env_from(&[
        ("OPENROUTER_API_KEY", "sk-or-test"),
        ("OPENAI_BASE_URL", "https://openrouter.ai/api/v1"),
    ]);
    let provider = parse_model_provider(Some("anthropic/claude-sonnet-4"));
    assert_eq!(provider.as_deref(), Some("anthropic"));
    assert_eq!(
        resolve_opencode_biller(&env, provider.as_deref()),
        "openrouter"
    );
}

#[test]
fn 综合_默认未指定_走unknown() {
    let env = env_from(&[]);
    let provider = parse_model_provider(None);
    assert_eq!(provider, None);
    assert_eq!(
        resolve_opencode_biller(&env, provider.as_deref()),
        "unknown"
    );
}

#[test]
fn 综合_个人_anthropic() {
    let env = env_from(&[("ANTHROPIC_API_KEY", "sk-test")]);
    let provider = parse_model_provider(Some("anthropic/claude-sonnet-4"));
    assert_eq!(provider.as_deref(), Some("anthropic"));
    // 无 OpenRouter env → provider 字面量保持。
    assert_eq!(
        resolve_opencode_biller(&env, provider.as_deref()),
        "anthropic"
    );
}
