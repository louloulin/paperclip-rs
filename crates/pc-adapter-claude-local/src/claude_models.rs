//! Claude 模型目录与认证模式识别（对齐 Node models.ts）。
//!
//! 提供：
//! - `is_bedrock_model_id` — 判断 model ID 是否为 Bedrock-native 标识
//! - `is_bedrock_env` — 判断当前 env 是否走 Bedrock 通道
//! - `BEDROCK_MODELS` — Bedrock 模式下的内置模型目录
//! - `ClaudeModel` / `ClaudeModelSource` — 列表条目

use serde::{Deserialize, Serialize};

/// 单条模型记录（id + 人类可读 label）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeModel {
    pub id: &'static str,
    pub label: &'static str,
}

/// Bedrock 模型目录（与 Node `BEDROCK_MODELS` 对齐）。
/// 包含 region-qualified IDs：`us.anthropic.*` / `eu.anthropic.*` / `arn:aws:bedrock:*`。
pub const BEDROCK_MODELS: &[ClaudeModel] = &[
    ClaudeModel {
        id: "us.anthropic.claude-opus-4-8-v1",
        label: "Bedrock Opus 4.8",
    },
    ClaudeModel {
        id: "us.anthropic.claude-fable-5-v1",
        label: "Bedrock Fable 5",
    },
    ClaudeModel {
        id: "us.anthropic.claude-opus-4-6-v1",
        label: "Bedrock Opus 4.6",
    },
    ClaudeModel {
        id: "us.anthropic.claude-sonnet-4-5-20250929-v2:0",
        label: "Bedrock Sonnet 4.5",
    },
    ClaudeModel {
        id: "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        label: "Bedrock Haiku 4.5",
    },
];

/// 直连 Anthropic API 时的默认模型目录（与 Node `DIRECT_MODELS` 对齐）。
pub const DIRECT_MODELS: &[ClaudeModel] = &[
    ClaudeModel {
        id: "claude-opus-4-8",
        label: "Claude Opus 4.8",
    },
    ClaudeModel {
        id: "claude-sonnet-4-5",
        label: "Claude Sonnet 4.5",
    },
    ClaudeModel {
        id: "claude-haiku-4-5",
        label: "Claude Haiku 4.5",
    },
];

/// 判断当前 env 是否应走 Bedrock 通道（对齐 Node `isBedrockEnv`）。
///
/// 命中任一：
/// - `CLAUDE_CODE_USE_BEDROCK=1` / `true`
/// - `ANTHROPIC_BEDROCK_BASE_URL` 非空字符串
#[must_use]
pub fn is_bedrock_env(env: &std::collections::BTreeMap<String, String>) -> bool {
    if let Some(value) = env.get("CLAUDE_CODE_USE_BEDROCK") {
        let v = value.trim().to_ascii_lowercase();
        if v == "1" || v == "true" || v == "yes" || v == "on" {
            return true;
        }
    }
    if let Some(value) = env.get("ANTHROPIC_BEDROCK_BASE_URL") {
        if !value.trim().is_empty() {
            return true;
        }
    }
    false
}

/// 判断 model ID 是否为 Bedrock-native 标识符（对齐 Node `isBedrockModelId`）。
///
/// - `us.anthropic.*` / `eu.anthropic.*` / `ap.anthropic.*` 等 region-qualified
/// - `arn:aws:bedrock:*` ARN 形式
#[must_use]
pub fn is_bedrock_model_id(model: &str) -> bool {
    // region-qualified: 字母数字 + `.anthropic.`
    let bytes = model.as_bytes();
    if bytes.len() >= 12 {
        // 寻找 ".anthropic." 子串，且前面必须是合法 region 字符（[A-Za-z0-9_-]+）
        if let Some(pos) = model.find(".anthropic.") {
            let prefix = &model[..pos];
            if !prefix.is_empty()
                && prefix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return true;
            }
        }
    }
    model.starts_with("arn:aws:bedrock:")
}

/// 当前认证模式下的内置模型目录（不发起远程拉取）。对齐 Node
/// `loadClaudeModels` 的 fallback 分支。
#[must_use]
pub fn builtin_models_for_env(env: &std::collections::BTreeMap<String, String>) -> &'static [ClaudeModel] {
    if is_bedrock_env(env) {
        BEDROCK_MODELS
    } else {
        DIRECT_MODELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn is_bedrock_env_accepts_truthy_use_bedrock() {
        for v in ["1", "true", "TRUE", "True", "yes", "on"] {
            let env = env_with(&[("CLAUDE_CODE_USE_BEDROCK", v)]);
            assert!(is_bedrock_env(&env), "expected true for {v}");
        }
    }

    #[test]
    fn is_bedrock_env_rejects_empty_and_false() {
        for v in ["", "0", "false", "no", "off"] {
            let env = env_with(&[("CLAUDE_CODE_USE_BEDROCK", v)]);
            assert!(!is_bedrock_env(&env), "expected false for {v}");
        }
    }

    #[test]
    fn is_bedrock_env_accepts_non_empty_base_url() {
        let env = env_with(&[("ANTHROPIC_BEDROCK_BASE_URL", "https://bedrock.us-east-1.amazonaws.com")]);
        assert!(is_bedrock_env(&env));
    }

    #[test]
    fn is_bedrock_env_empty_base_url_falls_through() {
        let env = env_with(&[("ANTHROPIC_BEDROCK_BASE_URL", "   ")]);
        assert!(!is_bedrock_env(&env));
    }

    #[test]
    fn is_bedrock_env_default_is_false() {
        let env = env_with(&[]);
        assert!(!is_bedrock_env(&env));
    }

    #[test]
    fn is_bedrock_model_id_recognizes_region_qualified_ids() {
        assert!(is_bedrock_model_id("us.anthropic.claude-opus-4-8-v1"));
        assert!(is_bedrock_model_id("eu.anthropic.claude-haiku-4-5-20251001-v1:0"));
        assert!(is_bedrock_model_id("ap.anthropic.claude-sonnet-4-5-20250929-v2:0"));
    }

    #[test]
    fn is_bedrock_model_id_recognizes_arn() {
        assert!(is_bedrock_model_id(
            "arn:aws:bedrock:us-east-1:123456789012:custom-model/abc"
        ));
    }

    #[test]
    fn is_bedrock_model_id_rejects_anthropic_api_short_names() {
        assert!(!is_bedrock_model_id("claude-opus-4-8"));
        assert!(!is_bedrock_model_id("claude-sonnet-4-5"));
        assert!(!is_bedrock_model_id("claude-haiku-4-5"));
    }

    #[test]
    fn is_bedrock_model_id_rejects_invalid_prefix() {
        // prefix 必须为合法 region 字符；空格 + 特殊字符应拒绝
        assert!(!is_bedrock_model_id("foo bar.anthropic.claude-x"));
        assert!(!is_bedrock_model_id(".anthropic.claude-x"));
        assert!(!is_bedrock_model_id("a/b.anthropic.claude-x"));
    }

    #[test]
    fn is_bedrock_model_id_rejects_empty() {
        assert!(!is_bedrock_model_id(""));
    }

    #[test]
    fn builtin_models_for_bedrock_env() {
        let env = env_with(&[("CLAUDE_CODE_USE_BEDROCK", "1")]);
        let models = builtin_models_for_env(&env);
        assert!(models.iter().any(|m| m.id == "us.anthropic.claude-opus-4-8-v1"));
        // 不应包含直连模型 ID
        assert!(!models.iter().any(|m| m.id == "claude-opus-4-8"));
    }

    #[test]
    fn builtin_models_for_anthropic_env() {
        let env = env_with(&[]);
        let models = builtin_models_for_env(&env);
        assert!(models.iter().any(|m| m.id == "claude-opus-4-8"));
        // 不应包含 Bedrock-only ID
        assert!(!models.iter().any(|m| m.id == "us.anthropic.claude-opus-4-8-v1"));
    }
}
