//! Grok-local execute 助手函数。
//!
//! 完整复刻 Node `packages/adapters/grok-local/src/server/execute.ts`
//! 中与 billing 解析相关的纯函数。
//!
//! 通用 `render_paperclip_env_note` / `render_api_access_note` 复用
//! `pc_acpx::session_config_options`（R408+），不重复实现。

use std::collections::BTreeMap;

use pc_acpx::env_helpers::has_non_empty_env_value;

/// Grok 的 billing 模式。
///
/// Node 等价：`resolveBillingType` 的返回类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokBillingType {
    /// API key 认证（`XAI_API_KEY`）。
    Api,
    /// Grok 订阅（无 API key）。
    Subscription,
}

impl GrokBillingType {
    pub fn as_str(&self) -> &'static str {
        match self {
            GrokBillingType::Api => "api",
            GrokBillingType::Subscription => "subscription",
        }
    }
}

/// 解析 Grok 的 billing 类型。
///
/// Node 等价：`resolveBillingType`。`XAI_API_KEY` 非空 → `Api`。
pub fn resolve_grok_billing_type(env: &BTreeMap<String, String>) -> GrokBillingType {
    if has_non_empty_env_value(env, "XAI_API_KEY") {
        GrokBillingType::Api
    } else {
        GrokBillingType::Subscription
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
    fn billing_默认subscription() {
        assert_eq!(
            resolve_grok_billing_type(&env_from(&[])),
            GrokBillingType::Subscription
        );
    }

    #[test]
    fn billing_xai_key_Api() {
        let env = env_from(&[("XAI_API_KEY", "xai-test")]);
        assert_eq!(resolve_grok_billing_type(&env), GrokBillingType::Api);
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
}
