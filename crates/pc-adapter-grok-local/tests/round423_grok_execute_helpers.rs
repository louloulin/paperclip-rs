//! R423 — Integration tests for `pc-adapter-grok-local::execute_helpers`.
//!
//! Mirrors Node `packages/adapters/grok-local/src/server/execute.ts`:
//! - `resolveBillingType` (L188-190)
//!
//! Unit tests inside `execute_helpers::tests` cover each function in isolation;
//! this integration suite verifies the complete helper API surface end-to-end.

use pc_adapter_grok_local::{resolve_grok_billing_type, GrokBillingType};
use std::collections::BTreeMap;

fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect()
}

// ---------------------------------------------------------------------------
// resolve_grok_billing_type
// ---------------------------------------------------------------------------

#[test]
fn billing_默认subscription() {
    assert_eq!(
        resolve_grok_billing_type(&env_from(&[])),
        GrokBillingType::Subscription
    );
}

#[test]
fn billing_xai_key_Api() {
    let env = env_from(&[("XAI_API_KEY", "xai-test-123")]);
    assert_eq!(
        resolve_grok_billing_type(&env),
        GrokBillingType::Api
    );
}

#[test]
fn billing_xai_key_空白_Subscription() {
    let env = env_from(&[("XAI_API_KEY", "   ")]);
    assert_eq!(
        resolve_grok_billing_type(&env),
        GrokBillingType::Subscription
    );
}

#[test]
fn billing_as_str_映射() {
    assert_eq!(GrokBillingType::Api.as_str(), "api");
    assert_eq!(GrokBillingType::Subscription.as_str(), "subscription");
}

// ---------------------------------------------------------------------------
// 综合场景
// ---------------------------------------------------------------------------

#[test]
fn 综合_企业_xai_api() {
    let env = env_from(&[("XAI_API_KEY", "xai-prod-key")]);
    assert_eq!(
        resolve_grok_billing_type(&env),
        GrokBillingType::Api
    );
}

#[test]
fn 综合_个人_grok订阅() {
    let env = env_from(&[]);
    assert_eq!(
        resolve_grok_billing_type(&env),
        GrokBillingType::Subscription
    );
}

#[test]
fn 综合_开发_有key默认返回Api() {
    // 模拟开发环境，XAI_API_KEY 已设置。
    let env = env_from(&[("XAI_API_KEY", "xai-test")]);
    assert_eq!(
        resolve_grok_billing_type(&env),
        GrokBillingType::Api
    );
}
