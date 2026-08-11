//! Hermes provider 解析（对齐 Node `resolveProvider`）。
//!
//! 优先级链（与 Node 完全一致）：
//! 1. **explicit** — adapterConfig 中显式配置（用户覆盖）
//! 2. **detected** — `~/.hermes/config.yaml` 中检测到的 provider
//! 3. **inferred** — 模型名前缀推断
//! 4. **auto** — 让 Hermes 自己决定

use crate::constants::infer_provider_from_model;
use crate::detect_model::DetectedModel;

/// provider 解析来源（用于日志/审计）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSource {
    Explicit,
    Detected,
    Inferred,
    Default,
}

impl ProviderSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Detected => "detected",
            Self::Inferred => "inferred",
            Self::Default => "default",
        }
    }
}

/// Provider 解析输入。
#[derive(Debug, Clone, Default)]
pub struct ResolveProviderInput<'a> {
    pub explicit_provider: Option<&'a str>,
    pub detected: Option<&'a DetectedModel>,
    pub model: &'a str,
}

/// 解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProvider {
    pub provider: String,
    pub source: ProviderSource,
}

/// Provider 解析：按 explicit → detected → inferred → auto 顺序回退。
///
/// 与 Node 不同：Node 还会基于 `detectedHasApiKey` 和 `detectedApiMode` 调整
/// 推断（按 `provider` -> `baseUrl` -> `apiMode` 的同源验证）。这里保留更
/// 保守的语义——如果 detected.provider 非空就用它，否则用 inferred，否则
/// auto。
pub fn resolve_provider(input: ResolveProviderInput<'_>) -> ResolvedProvider {
    let trimmed_explicit = input
        .explicit_provider
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(provider) = trimmed_explicit {
        return ResolvedProvider {
            provider: provider.to_string(),
            source: ProviderSource::Explicit,
        };
    }

    if let Some(detected) = input.detected {
        let detected_provider = detected.provider.trim();
        if !detected_provider.is_empty() {
            return ResolvedProvider {
                provider: detected_provider.to_string(),
                source: ProviderSource::Detected,
            };
        }
    }

    if let Some(provider) = infer_provider_from_model(input.model) {
        return ResolvedProvider {
            provider: provider.to_string(),
            source: ProviderSource::Inferred,
        };
    }

    ResolvedProvider {
        provider: "auto".to_string(),
        source: ProviderSource::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detected(provider: &str) -> DetectedModel {
        DetectedModel {
            provider: provider.to_string(),
            ..DetectedModel::default()
        }
    }

    #[test]
    fn explicit_provider_wins() {
        let resolved = resolve_provider(ResolveProviderInput {
            explicit_provider: Some("anthropic"),
            detected: Some(&detected("openrouter")),
            model: "gpt-5",
        });
        assert_eq!(resolved.provider, "anthropic");
        assert_eq!(resolved.source, ProviderSource::Explicit);
    }

    #[test]
    fn detected_provider_used_when_no_explicit() {
        let resolved = resolve_provider(ResolveProviderInput {
            explicit_provider: None,
            detected: Some(&detected("zai")),
            model: "gpt-5",
        });
        assert_eq!(resolved.provider, "zai");
        assert_eq!(resolved.source, ProviderSource::Detected);
    }

    #[test]
    fn inferred_provider_used_when_no_explicit_or_detected() {
        let resolved = resolve_provider(ResolveProviderInput {
            explicit_provider: None,
            detected: None,
            model: "claude-3.7-sonnet",
        });
        assert_eq!(resolved.provider, "anthropic");
        assert_eq!(resolved.source, ProviderSource::Inferred);
    }

    #[test]
    fn falls_back_to_auto() {
        let resolved = resolve_provider(ResolveProviderInput {
            explicit_provider: None,
            detected: Some(&detected("")),
            model: "phi-3-mini",
        });
        assert_eq!(resolved.provider, "auto");
        assert_eq!(resolved.source, ProviderSource::Default);
    }

    #[test]
    fn empty_explicit_treated_as_absent() {
        let resolved = resolve_provider(ResolveProviderInput {
            explicit_provider: Some("   "),
            detected: None,
            model: "claude-3",
        });
        assert_eq!(resolved.source, ProviderSource::Inferred);
        assert_eq!(resolved.provider, "anthropic");
    }
}
