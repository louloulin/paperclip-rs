//! R419 — Integration tests for `pc-adapter-codex-local::execute_helpers`.
//!
//! Mirrors Node `packages/adapters/codex-local/src/server/execute.ts`:
//! - `resolveCodexBillingType` (L159-162)
//! - `resolveCodexBiller` (L164-168)
//! - `resolveCodexSkillsDir` (L237-239)
//! - `readCodexTransientFallbackMode` (L254-265)
//! - `fallbackModeUsesSaferInvocation` (L267-269)
//! - `fallbackModeUsesFreshSession` (L271-273)
//!
//! Unit tests inside `execute_helpers::tests` cover each function in isolation;
//! this integration suite verifies the complete helper API surface end-to-end.

use pc_adapter_codex_local::{
    fallback_mode_uses_fresh_session, fallback_mode_uses_safer_invocation,
    read_codex_transient_fallback_mode, resolve_codex_biller, resolve_codex_billing_type,
    resolve_codex_skills_dir, CodexBillingType, CodexTransientFallbackMode,
};
use std::collections::BTreeMap;

fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

// ---------------------------------------------------------------------------
// resolve_codex_billing_type
// ---------------------------------------------------------------------------

#[test]
fn billing_默认subscription() {
    assert_eq!(
        resolve_codex_billing_type(&env_from(&[])),
        CodexBillingType::Subscription
    );
}

#[test]
fn billing_api_key_有值_Api() {
    let env = env_from(&[("OPENAI_API_KEY", "sk-test")]);
    assert_eq!(resolve_codex_billing_type(&env), CodexBillingType::Api);
}

#[test]
fn billing_api_key_空白_Subscription() {
    // API key 为空白 → 视为未设置。
    let env = env_from(&[("OPENAI_API_KEY", "  ")]);
    assert_eq!(
        resolve_codex_billing_type(&env),
        CodexBillingType::Subscription
    );
}

#[test]
fn billing_as_str_映射() {
    assert_eq!(CodexBillingType::Api.as_str(), "api");
    assert_eq!(CodexBillingType::Subscription.as_str(), "subscription");
}

// ---------------------------------------------------------------------------
// resolve_codex_biller
// ---------------------------------------------------------------------------

#[test]
fn biller_openrouter_优先() {
    let env = env_from(&[
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENROUTER_API_KEY", "sk-or-test"),
    ]);
    assert_eq!(
        resolve_codex_biller(&env, CodexBillingType::Api),
        "openrouter"
    );
}

#[test]
fn biller_subscription_chatgpt() {
    let env = env_from(&[]);
    assert_eq!(
        resolve_codex_biller(&env, CodexBillingType::Subscription),
        "chatgpt"
    );
}

#[test]
fn biller_api_openai_fallback() {
    let env = env_from(&[("OPENAI_API_KEY", "sk-test")]);
    assert_eq!(resolve_codex_biller(&env, CodexBillingType::Api), "openai");
}

#[test]
fn biller_api_openrouter_via_base_url() {
    // OpenRouter 通过 base URL 命中。
    let env = env_from(&[
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENAI_BASE_URL", "https://openrouter.ai/api/v1"),
    ]);
    assert_eq!(
        resolve_codex_biller(&env, CodexBillingType::Api),
        "openrouter"
    );
}

#[test]
fn biller_subscription_openrouter_env() {
    // 即使是 subscription 模式，OpenRouter 仍优先。
    let env = env_from(&[("OPENROUTER_API_KEY", "sk-or-test")]);
    assert_eq!(
        resolve_codex_biller(&env, CodexBillingType::Subscription),
        "openrouter"
    );
}

// ---------------------------------------------------------------------------
// read_codex_transient_fallback_mode
// ---------------------------------------------------------------------------

#[test]
fn fallback_mode_四种合法值() {
    assert_eq!(
        read_codex_transient_fallback_mode(
            &serde_json::json!({"codexTransientFallbackMode": "same_session"})
        ),
        Some(CodexTransientFallbackMode::SameSession)
    );
    assert_eq!(
        read_codex_transient_fallback_mode(
            &serde_json::json!({"codexTransientFallbackMode": "safer_invocation"})
        ),
        Some(CodexTransientFallbackMode::SaferInvocation)
    );
    assert_eq!(
        read_codex_transient_fallback_mode(
            &serde_json::json!({"codexTransientFallbackMode": "fresh_session"})
        ),
        Some(CodexTransientFallbackMode::FreshSession)
    );
    assert_eq!(
        read_codex_transient_fallback_mode(
            &serde_json::json!({"codexTransientFallbackMode": "fresh_session_safer_invocation"})
        ),
        Some(CodexTransientFallbackMode::FreshSessionSaferInvocation)
    );
}

#[test]
fn fallback_mode_含空格trim() {
    // Node 行为：`asString(...).trim()`。
    assert_eq!(
        read_codex_transient_fallback_mode(
            &serde_json::json!({"codexTransientFallbackMode": "  safer_invocation  "})
        ),
        Some(CodexTransientFallbackMode::SaferInvocation)
    );
}

#[test]
fn fallback_mode_非法值_None() {
    assert_eq!(
        read_codex_transient_fallback_mode(
            &serde_json::json!({"codexTransientFallbackMode": "invalid"})
        ),
        None
    );
    assert_eq!(
        read_codex_transient_fallback_mode(&serde_json::json!({"codexTransientFallbackMode": ""})),
        None
    );
}

#[test]
fn fallback_mode_字段缺失() {
    assert_eq!(
        read_codex_transient_fallback_mode(&serde_json::json!({})),
        None
    );
    assert_eq!(
        read_codex_transient_fallback_mode(&serde_json::Value::Null),
        None
    );
}

#[test]
fn fallback_mode_非字符串() {
    assert_eq!(
        read_codex_transient_fallback_mode(&serde_json::json!({"codexTransientFallbackMode": 123})),
        None
    );
    assert_eq!(
        read_codex_transient_fallback_mode(
            &serde_json::json!({"codexTransientFallbackMode": null})
        ),
        None
    );
}

#[test]
fn fallback_mode_as_str_映射() {
    assert_eq!(
        CodexTransientFallbackMode::SameSession.as_str(),
        "same_session"
    );
    assert_eq!(
        CodexTransientFallbackMode::SaferInvocation.as_str(),
        "safer_invocation"
    );
    assert_eq!(
        CodexTransientFallbackMode::FreshSession.as_str(),
        "fresh_session"
    );
    assert_eq!(
        CodexTransientFallbackMode::FreshSessionSaferInvocation.as_str(),
        "fresh_session_safer_invocation"
    );
}

// ---------------------------------------------------------------------------
// fallback_mode_uses_safer_invocation / fresh_session
// ---------------------------------------------------------------------------

#[test]
fn safer_invocation_判断() {
    assert!(fallback_mode_uses_safer_invocation(Some(
        CodexTransientFallbackMode::SaferInvocation
    )));
    assert!(fallback_mode_uses_safer_invocation(Some(
        CodexTransientFallbackMode::FreshSessionSaferInvocation
    )));
    assert!(!fallback_mode_uses_safer_invocation(Some(
        CodexTransientFallbackMode::SameSession
    )));
    assert!(!fallback_mode_uses_safer_invocation(Some(
        CodexTransientFallbackMode::FreshSession
    )));
    assert!(!fallback_mode_uses_safer_invocation(None));
}

#[test]
fn fresh_session_判断() {
    assert!(fallback_mode_uses_fresh_session(Some(
        CodexTransientFallbackMode::FreshSession
    )));
    assert!(fallback_mode_uses_fresh_session(Some(
        CodexTransientFallbackMode::FreshSessionSaferInvocation
    )));
    assert!(!fallback_mode_uses_fresh_session(Some(
        CodexTransientFallbackMode::SameSession
    )));
    assert!(!fallback_mode_uses_fresh_session(Some(
        CodexTransientFallbackMode::SaferInvocation
    )));
    assert!(!fallback_mode_uses_fresh_session(None));
}

#[test]
fn 综合策略_从context提取() {
    // 模拟从 context 完整读出并判断策略。
    let context = serde_json::json!({
        "codexTransientFallbackMode": "fresh_session_safer_invocation"
    });
    let mode = read_codex_transient_fallback_mode(&context);
    assert_eq!(
        mode,
        Some(CodexTransientFallbackMode::FreshSessionSaferInvocation)
    );
    assert!(fallback_mode_uses_safer_invocation(mode));
    assert!(fallback_mode_uses_fresh_session(mode));
}

// ---------------------------------------------------------------------------
// resolve_codex_skills_dir
// ---------------------------------------------------------------------------

#[test]
fn skills_dir_基本路径() {
    assert_eq!(
        resolve_codex_skills_dir("/home/u/.codex"),
        "/home/u/.codex/skills"
    );
    assert_eq!(
        resolve_codex_skills_dir("/home/u/.codex/"),
        "/home/u/.codex/skills"
    );
}

#[test]
fn skills_dir_根路径() {
    assert_eq!(resolve_codex_skills_dir("/"), "/skills");
}

#[test]
fn skills_dir_空输入() {
    assert_eq!(resolve_codex_skills_dir(""), "skills");
}

#[test]
fn skills_dir_相对路径() {
    assert_eq!(resolve_codex_skills_dir(".codex"), ".codex/skills");
    assert_eq!(resolve_codex_skills_dir(".codex/"), ".codex/skills");
}

// ---------------------------------------------------------------------------
// 综合场景
// ---------------------------------------------------------------------------

#[test]
fn 综合_企业_api_via_openrouter() {
    // 模拟：使用 OpenRouter 的 OPENAI_BASE_URL，无 OPENROUTER_API_KEY
    // （OpenAI compat 仍能识别）。
    let env = env_from(&[
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENAI_BASE_URL", "https://openrouter.ai/api/v1"),
    ]);
    assert_eq!(resolve_codex_billing_type(&env), CodexBillingType::Api);
    assert_eq!(
        resolve_codex_biller(&env, CodexBillingType::Api),
        "openrouter"
    );
}

#[test]
fn 综合_个人_chatgpt订阅() {
    let env = env_from(&[]);
    assert_eq!(
        resolve_codex_billing_type(&env),
        CodexBillingType::Subscription
    );
    assert_eq!(
        resolve_codex_biller(&env, CodexBillingType::Subscription),
        "chatgpt"
    );
}

#[test]
fn 综合_开发_openai_api() {
    let env = env_from(&[("OPENAI_API_KEY", "sk-test")]);
    assert_eq!(resolve_codex_billing_type(&env), CodexBillingType::Api);
    assert_eq!(resolve_codex_biller(&env, CodexBillingType::Api), "openai");
}
