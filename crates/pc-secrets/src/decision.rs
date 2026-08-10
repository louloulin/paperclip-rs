//! Secret provider 决策流。
//!
//! 给定一个 company + secret 上下文，决定使用哪个 provider。
//! 决策依据：
//! 1. company 配置中显式声明的 `provider_id`（优先级最高）。
//! 2. 回退链：按配置顺序尝试每个 provider。
//! 3. 若全部失败，返回 `Decision::Rejected` + 原因。
//!
//! 与 Node `secret-decision-flow.ts` 等价。

use serde::{Deserialize, Serialize};

use crate::registry::SecretProviderRegistry;

/// 决策上下文。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretDecisionContext {
    /// 公司显式指定的 provider id；None 时走回退链。
    pub preferred_provider_id: Option<String>,
    /// 回退链：按顺序尝试。
    pub fallback_chain: Vec<String>,
    /// 排除的 provider id（如已知不可用）。
    pub exclude: Vec<String>,
    /// 当前 secret key（用于审计/日志）。
    pub secret_key: Option<String>,
}

/// 决策结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretDecision {
    /// 选中的 provider。
    Selected {
        provider_id: String,
        /// 是否回退链中的非首选项。
        from_fallback: bool,
    },
    /// 全部候选都不可用。
    Rejected { reason: String, tried: Vec<String> },
}

impl SecretDecision {
    #[must_use]
    pub fn is_selected(&self) -> bool {
        matches!(self, Self::Selected { .. })
    }

    #[must_use]
    pub fn provider_id(&self) -> Option<&str> {
        match self {
            Self::Selected { provider_id, .. } => Some(provider_id),
            Self::Rejected { .. } => None,
        }
    }
}

/// 决策评估：选出一个可用的 provider。
#[must_use]
pub fn decide_provider(
    registry: &SecretProviderRegistry,
    context: &SecretDecisionContext,
) -> SecretDecision {
    let mut tried = Vec::new();

    // 1. 优先首选
    if let Some(preferred) = &context.preferred_provider_id {
        tried.push(preferred.clone());
        if !context.exclude.contains(preferred) && registry.get(preferred).is_some() {
            return SecretDecision::Selected {
                provider_id: preferred.clone(),
                from_fallback: false,
            };
        }
    }

    // 2. 走回退链
    for candidate in &context.fallback_chain {
        if tried.contains(candidate) {
            continue;
        }
        if context.exclude.contains(candidate) {
            continue;
        }
        tried.push(candidate.clone());
        if registry.get(candidate).is_some() {
            return SecretDecision::Selected {
                provider_id: candidate.clone(),
                from_fallback: true,
            };
        }
    }

    // 3. 全部不可用
    SecretDecision::Rejected {
        reason: format!(
            "no usable provider; preferred={:?}, fallback={}, excluded={}",
            context.preferred_provider_id, context.fallback_chain.len(), context.exclude.len()
        ),
        tried,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_encrypted::LocalEncryptedProvider;
    use std::sync::Arc;

    fn registry_with_local() -> SecretProviderRegistry {
        let mut reg = SecretProviderRegistry::new();
        reg.register(Arc::new(LocalEncryptedProvider::from_bytes([0u8; 32])));
        reg
    }

    #[test]
    fn r567_decide_preferred_provider_when_registered() {
        let reg = registry_with_local();
        let ctx = SecretDecisionContext {
            preferred_provider_id: Some("local_encrypted".into()),
            ..Default::default()
        };
        let d = decide_provider(&reg, &ctx);
        match d {
            SecretDecision::Selected { provider_id, from_fallback } => {
                assert_eq!(provider_id, "local_encrypted");
                assert!(!from_fallback);
            }
            _ => panic!("expected Selected"),
        }
    }

    #[test]
    fn r567_decide_falls_back_when_preferred_unknown() {
        let reg = registry_with_local();
        let ctx = SecretDecisionContext {
            preferred_provider_id: Some("nonexistent".into()),
            fallback_chain: vec!["local_encrypted".into()],
            ..Default::default()
        };
        let d = decide_provider(&reg, &ctx);
        match d {
            SecretDecision::Selected { provider_id, from_fallback } => {
                assert_eq!(provider_id, "local_encrypted");
                assert!(from_fallback, "should mark as from_fallback");
            }
            _ => panic!("expected Selected"),
        }
    }

    #[test]
    fn r567_decide_rejects_when_nothing_available() {
        let reg = registry_with_local();
        let ctx = SecretDecisionContext {
            preferred_provider_id: Some("nonexistent".into()),
            fallback_chain: vec!["also_missing".into()],
            ..Default::default()
        };
        let d = decide_provider(&reg, &ctx);
        assert!(!d.is_selected());
        assert_eq!(d.provider_id(), None);
    }

    #[test]
    fn r567_decide_respects_exclude() {
        let reg = registry_with_local();
        let ctx = SecretDecisionContext {
            preferred_provider_id: Some("local_encrypted".into()),
            exclude: vec!["local_encrypted".into()],
            ..Default::default()
        };
        let d = decide_provider(&reg, &ctx);
        assert!(!d.is_selected());
    }

    #[test]
    fn r567_decide_skips_duplicates_in_fallback() {
        let reg = registry_with_local();
        let ctx = SecretDecisionContext {
            preferred_provider_id: None,
            fallback_chain: vec![
                "missing1".into(),
                "local_encrypted".into(),
                "local_encrypted".into(), // 重复
            ],
            ..Default::default()
        };
        let d = decide_provider(&reg, &ctx);
        match d {
            SecretDecision::Selected { provider_id, .. } => {
                assert_eq!(provider_id, "local_encrypted");
            }
            _ => panic!("expected Selected"),
        }
    }
}
