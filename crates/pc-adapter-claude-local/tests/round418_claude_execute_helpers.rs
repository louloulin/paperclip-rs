//! R418 — Integration tests for `pc-adapter-claude-local::execute_helpers`.
//!
//! Mirrors Node `packages/adapters/claude-local/src/server/execute.ts`:
//! - `isBedrockAuth` (L148-154)
//! - `resolveClaudeBillingType` (L156-159)
//! - `claudeSessionCwdMatchesExecutionTarget` (L120-127)
//!
//! Unit tests inside `execute_helpers::tests` cover each function in isolation;
//! this integration suite verifies the complete helper API surface end-to-end.

use pc_adapter_claude_local::{
    claude_session_cwd_matches_execution_target, is_bedrock_auth, resolve_claude_billing_type,
    ClaudeBillingType,
};
use std::collections::BTreeMap;

fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

// ---------------------------------------------------------------------------
// is_bedrock_auth
// ---------------------------------------------------------------------------

#[test]
fn bedrock_use_bedrock_1_触发() {
    let env = env_from(&[("CLAUDE_CODE_USE_BEDROCK", "1")]);
    assert!(is_bedrock_auth(&env));
}

#[test]
fn bedrock_use_bedrock_true_触发() {
    let env = env_from(&[("CLAUDE_CODE_USE_BEDROCK", "true")]);
    assert!(is_bedrock_auth(&env));
}

#[test]
fn bedrock_use_bedrock_false_不触发() {
    let env = env_from(&[("CLAUDE_CODE_USE_BEDROCK", "false")]);
    assert!(!is_bedrock_auth(&env));
}

#[test]
fn bedrock_use_bedrock_0_不触发() {
    let env = env_from(&[("CLAUDE_CODE_USE_BEDROCK", "0")]);
    assert!(!is_bedrock_auth(&env));
}

#[test]
fn bedrock_use_bedrock_空字符串_不触发() {
    let env = env_from(&[("CLAUDE_CODE_USE_BEDROCK", "")]);
    assert!(!is_bedrock_auth(&env));
}

#[test]
fn bedrock_base_url_非空_触发() {
    let env = env_from(&[("ANTHROPIC_BEDROCK_BASE_URL", "https://bedrock.example")]);
    assert!(is_bedrock_auth(&env));
}

#[test]
fn bedrock_base_url_空白_不触发() {
    let env = env_from(&[("ANTHROPIC_BEDROCK_BASE_URL", "   ")]);
    assert!(!is_bedrock_auth(&env));
}

#[test]
fn bedrock_都未设置_不触发() {
    let env = env_from(&[]);
    assert!(!is_bedrock_auth(&env));
}

#[test]
fn bedrock_多个env任意一个触发() {
    let env = env_from(&[
        ("CLAUDE_CODE_USE_BEDROCK", "false"), // 假信号
        ("ANTHROPIC_BEDROCK_BASE_URL", "https://bedrock"), // 真信号
    ]);
    assert!(is_bedrock_auth(&env));
}

// ---------------------------------------------------------------------------
// resolve_claude_billing_type
// ---------------------------------------------------------------------------

#[test]
fn billing_默认subscription() {
    assert_eq!(
        resolve_claude_billing_type(&env_from(&[])),
        ClaudeBillingType::Subscription
    );
}

#[test]
fn billing_api_key_有值_Api() {
    let env = env_from(&[("ANTHROPIC_API_KEY", "sk-test")]);
    assert_eq!(
        resolve_claude_billing_type(&env),
        ClaudeBillingType::Api
    );
}

#[test]
fn billing_api_key_空白_Subscription() {
    // API key 为空白 → 视为未设置。
    let env = env_from(&[("ANTHROPIC_API_KEY", "   ")]);
    assert_eq!(
        resolve_claude_billing_type(&env),
        ClaudeBillingType::Subscription
    );
}

#[test]
fn billing_bedrock_优先() {
    // 即使有 API key，Bedrock 仍优先于 api。
    let env = env_from(&[
        ("ANTHROPIC_API_KEY", "sk-test"),
        ("CLAUDE_CODE_USE_BEDROCK", "1"),
    ]);
    assert_eq!(
        resolve_claude_billing_type(&env),
        ClaudeBillingType::MeteredApi
    );
}

#[test]
fn billing_bedrock_通过_base_url() {
    let env = env_from(&[
        ("ANTHROPIC_API_KEY", "sk-test"),
        ("ANTHROPIC_BEDROCK_BASE_URL", "https://bedrock"),
    ]);
    assert_eq!(
        resolve_claude_billing_type(&env),
        ClaudeBillingType::MeteredApi
    );
}

#[test]
fn billing_as_str_映射() {
    assert_eq!(ClaudeBillingType::Api.as_str(), "api");
    assert_eq!(ClaudeBillingType::Subscription.as_str(), "subscription");
    assert_eq!(ClaudeBillingType::MeteredApi.as_str(), "metered_api");
}

// ---------------------------------------------------------------------------
// claude_session_cwd_matches_execution_target
// ---------------------------------------------------------------------------

#[test]
fn session_cwd_remote_总是true() {
    assert!(claude_session_cwd_matches_execution_target(
        "/any/where",
        "/current/here",
        true,
    ));
}

#[test]
fn session_cwd_空cwd_总是true() {
    assert!(claude_session_cwd_matches_execution_target("", "/current/here", false));
    assert!(claude_session_cwd_matches_execution_target("   ", "/current/here", false));
}

#[test]
fn session_cwd_绝对路径一致() {
    assert!(claude_session_cwd_matches_execution_target(
        "/home/u/proj",
        "/home/u/proj",
        false,
    ));
}

#[test]
fn session_cwd_规范化后一致() {
    assert!(claude_session_cwd_matches_execution_target(
        "/home/u/proj/.",
        "/home/u/proj",
        false,
    ));
    assert!(claude_session_cwd_matches_execution_target(
        "/home/u/proj/sub/..",
        "/home/u/proj",
        false,
    ));
}

#[test]
fn session_cwd_不一致() {
    assert!(!claude_session_cwd_matches_execution_target(
        "/home/u/proj",
        "/home/u/other",
        false,
    ));
    assert!(!claude_session_cwd_matches_execution_target(
        "/home/u/proj",
        "/home/u/proj/sub",
        false,
    ));
}

#[test]
fn session_cwd_大小写敏感() {
    // POSIX 区分大小写（与 Node path.resolve 一致）。
    assert!(!claude_session_cwd_matches_execution_target(
        "/home/u/Proj",
        "/home/u/proj",
        false,
    ));
}

// ---------------------------------------------------------------------------
// 综合场景
// ---------------------------------------------------------------------------

#[test]
fn 综合_企业环境_bedrock优先级() {
    // 模拟企业部署：使用 AWS Bedrock，无 API key。
    let env = env_from(&[
        ("CLAUDE_CODE_USE_BEDROCK", "1"),
        ("ANTHROPIC_BEDROCK_BASE_URL", "https://bedrock.us-east-1.amazonaws.com"),
    ]);
    assert!(is_bedrock_auth(&env));
    assert_eq!(
        resolve_claude_billing_type(&env),
        ClaudeBillingType::MeteredApi
    );
}

#[test]
fn 综合_个人开发_api() {
    // 模拟个人开发：直接 API key。
    let env = env_from(&[("ANTHROPIC_API_KEY", "sk-ant-test-123")]);
    assert!(!is_bedrock_auth(&env));
    assert_eq!(
        resolve_claude_billing_type(&env),
        ClaudeBillingType::Api
    );
}

#[test]
fn 综合_claude_pro_订阅() {
    // 模拟 Claude Pro 订阅：无 API key，无 Bedrock。
    let env = env_from(&[]);
    assert!(!is_bedrock_auth(&env));
    assert_eq!(
        resolve_claude_billing_type(&env),
        ClaudeBillingType::Subscription
    );
}

#[test]
fn 综合_resume_决策() {
    // 模拟 resume 决策：session cwd 与当前 cwd 一致 → 可 resume。
    let saved_session_cwd = "/work/proj";
    let current_cwd = "/work/proj";
    let can_resume = claude_session_cwd_matches_execution_target(
        saved_session_cwd,
        current_cwd,
        false, // 本地执行
    );
    assert!(can_resume);
}

#[test]
fn 综合_resume_cwd_改变_不匹配() {
    let saved_session_cwd = "/work/proj";
    let current_cwd = "/work/other"; // cwd 改了
    let can_resume = claude_session_cwd_matches_execution_target(
        saved_session_cwd,
        current_cwd,
        false,
    );
    assert!(!can_resume);
}
