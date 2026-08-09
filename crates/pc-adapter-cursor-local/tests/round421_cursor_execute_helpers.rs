//! R421 — Integration tests for `pc-adapter-cursor-local::execute_helpers`.
//!
//! Mirrors Node `packages/adapters/cursor-local/src/server/execute.ts`:
//! - `resolveCursorBillingType` (L70-74)
//! - `resolveCursorBiller` (L76-86)
//! - `resolveProviderFromModel` (L87-95)
//! - `normalizeMode` (L97-101)
//! - `cursorSkillsHome` (L117-119)
//!
//! Unit tests inside `execute_helpers::tests` cover each function in isolation;
//! this integration suite verifies the complete helper API surface end-to-end.

use pc_adapter_cursor_local::{
    cursor_skills_home, normalize_mode, resolve_cursor_biller, resolve_cursor_billing_type,
    resolve_provider_from_model, CursorBillingType, CursorMode,
};
use std::collections::BTreeMap;

fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

// ---------------------------------------------------------------------------
// resolve_cursor_billing_type
// ---------------------------------------------------------------------------

#[test]
fn billing_默认subscription() {
    assert_eq!(
        resolve_cursor_billing_type(&env_from(&[])),
        CursorBillingType::Subscription
    );
}

#[test]
fn billing_cursor_key_Api() {
    assert_eq!(
        resolve_cursor_billing_type(&env_from(&[("CURSOR_API_KEY", "key")])),
        CursorBillingType::Api
    );
}

#[test]
fn billing_openai_key_Api() {
    assert_eq!(
        resolve_cursor_billing_type(&env_from(&[("OPENAI_API_KEY", "key")])),
        CursorBillingType::Api
    );
}

#[test]
fn billing_api_key_空白_Subscription() {
    let env = env_from(&[("CURSOR_API_KEY", "  ")]);
    assert_eq!(
        resolve_cursor_billing_type(&env),
        CursorBillingType::Subscription
    );
}

#[test]
fn billing_as_str_映射() {
    assert_eq!(CursorBillingType::Api.as_str(), "api");
    assert_eq!(CursorBillingType::Subscription.as_str(), "subscription");
}

// ---------------------------------------------------------------------------
// resolve_cursor_biller
// ---------------------------------------------------------------------------

#[test]
fn biller_openrouter_优先() {
    let env = env_from(&[("CURSOR_API_KEY", "key"), ("OPENROUTER_API_KEY", "or-key")]);
    assert_eq!(
        resolve_cursor_biller(&env, CursorBillingType::Api, Some("anthropic")),
        "openrouter"
    );
}

#[test]
fn biller_subscription_默认cursor() {
    let env = env_from(&[]);
    assert_eq!(
        resolve_cursor_biller(&env, CursorBillingType::Subscription, None),
        "cursor"
    );
}

#[test]
fn biller_api_用provider() {
    let env = env_from(&[("CURSOR_API_KEY", "key")]);
    assert_eq!(
        resolve_cursor_biller(&env, CursorBillingType::Api, Some("anthropic")),
        "anthropic"
    );
}

#[test]
fn biller_api_无provider_fallback_cursor() {
    let env = env_from(&[("CURSOR_API_KEY", "key")]);
    assert_eq!(
        resolve_cursor_biller(&env, CursorBillingType::Api, None),
        "cursor"
    );
}

// ---------------------------------------------------------------------------
// normalize_mode
// ---------------------------------------------------------------------------

#[test]
fn mode_plan合法() {
    assert_eq!(normalize_mode("plan"), Some(CursorMode::Plan));
    assert_eq!(normalize_mode("PLAN"), Some(CursorMode::Plan));
    assert_eq!(normalize_mode("  plan  "), Some(CursorMode::Plan));
}

#[test]
fn mode_ask合法() {
    assert_eq!(normalize_mode("ask"), Some(CursorMode::Ask));
    assert_eq!(normalize_mode("ASK"), Some(CursorMode::Ask));
}

#[test]
fn mode_非法_None() {
    assert_eq!(normalize_mode("agent"), None);
    assert_eq!(normalize_mode(""), None);
    assert_eq!(normalize_mode("  "), None);
}

#[test]
fn mode_as_str_映射() {
    assert_eq!(CursorMode::Plan.as_str(), "plan");
    assert_eq!(CursorMode::Ask.as_str(), "ask");
}

// ---------------------------------------------------------------------------
// resolve_provider_from_model
// ---------------------------------------------------------------------------

#[test]
fn provider_斜杠拆分() {
    assert_eq!(
        resolve_provider_from_model("anthropic/claude-sonnet-4"),
        Some("anthropic".to_owned())
    );
    assert_eq!(
        resolve_provider_from_model("openai/gpt-4"),
        Some("openai".to_owned())
    );
}

#[test]
fn provider_大小写不敏感() {
    assert_eq!(
        resolve_provider_from_model("ANTHROPIC/CLAUDE"),
        Some("anthropic".to_owned())
    );
}

#[test]
fn provider_含sonnet() {
    assert_eq!(
        resolve_provider_from_model("claude-3-sonnet-20240229"),
        Some("anthropic".to_owned())
    );
}

#[test]
fn provider_含claude() {
    assert_eq!(
        resolve_provider_from_model("claude-opus-4"),
        Some("anthropic".to_owned())
    );
}

#[test]
fn provider_gpt开头() {
    assert_eq!(
        resolve_provider_from_model("gpt-4o"),
        Some("openai".to_owned())
    );
}

#[test]
fn provider_o开头() {
    assert_eq!(
        resolve_provider_from_model("o1-preview"),
        Some("openai".to_owned())
    );
}

#[test]
fn provider_中文模型_无匹配() {
    assert_eq!(resolve_provider_from_model("gemini-pro"), None);
    assert_eq!(resolve_provider_from_model("deepseek-coder"), None);
}

#[test]
fn provider_空输入() {
    assert_eq!(resolve_provider_from_model(""), None);
    assert_eq!(resolve_provider_from_model("   "), None);
}

#[test]
fn provider_斜杠开头_None() {
    // Node 行为：`slash > 0` 才返回 prefix，slash=0 返回 None。
    assert_eq!(resolve_provider_from_model("/model"), None);
}

// ---------------------------------------------------------------------------
// cursor_skills_home
// ---------------------------------------------------------------------------

#[test]
fn skills_home_标准路径() {
    assert_eq!(cursor_skills_home("/home/u"), "/home/u/.cursor/skills");
}

#[test]
fn skills_home_尾斜杠() {
    assert_eq!(cursor_skills_home("/home/u/"), "/home/u/.cursor/skills");
}

#[test]
fn skills_home_根路径() {
    assert_eq!(cursor_skills_home("/"), "/.cursor/skills");
}

#[test]
fn skills_home_空输入() {
    assert_eq!(cursor_skills_home(""), "/.cursor/skills");
}

// ---------------------------------------------------------------------------
// 综合场景
// ---------------------------------------------------------------------------

#[test]
fn 综合_企业_anthropic_via_openrouter() {
    let env = env_from(&[("CURSOR_API_KEY", "key"), ("OPENROUTER_API_KEY", "or-key")]);
    assert_eq!(resolve_cursor_billing_type(&env), CursorBillingType::Api);
    let provider = resolve_provider_from_model("anthropic/claude-sonnet-4");
    assert_eq!(provider.as_deref(), Some("anthropic"));
    assert_eq!(
        resolve_cursor_biller(&env, CursorBillingType::Api, provider.as_deref()),
        "openrouter"
    );
}

#[test]
fn 综合_个人_cursor订阅() {
    let env = env_from(&[]);
    assert_eq!(
        resolve_cursor_billing_type(&env),
        CursorBillingType::Subscription
    );
    assert_eq!(
        resolve_cursor_biller(&env, CursorBillingType::Subscription, None),
        "cursor"
    );
}

#[test]
fn 综合_开发_openai_api() {
    let env = env_from(&[("OPENAI_API_KEY", "sk-test")]);
    assert_eq!(resolve_cursor_billing_type(&env), CursorBillingType::Api);
    let provider = resolve_provider_from_model("openai/gpt-4o");
    assert_eq!(provider.as_deref(), Some("openai"));
    assert_eq!(
        resolve_cursor_biller(&env, CursorBillingType::Api, provider.as_deref()),
        "openai"
    );
}

#[test]
fn 综合_anthropic隐式识别() {
    // 不带 prefix，仅凭 model 名字识别。
    let provider = resolve_provider_from_model("claude-3-5-sonnet-20241022");
    assert_eq!(provider.as_deref(), Some("anthropic"));
}

#[test]
fn 综合_openai隐式识别() {
    let provider = resolve_provider_from_model("gpt-4o");
    assert_eq!(provider.as_deref(), Some("openai"));
}

#[test]
fn 综合_mode合法性() {
    // 模拟从配置读取 mode 并校验。
    assert_eq!(normalize_mode("plan"), Some(CursorMode::Plan));
    assert_eq!(normalize_mode("ASK"), Some(CursorMode::Ask));
    assert_eq!(normalize_mode("agent"), None);
}
