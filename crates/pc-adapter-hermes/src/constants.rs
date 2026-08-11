//! Hermes adapter 常量（对齐 Node `packages/adapters/hermes/src/shared/constants.ts`）。

#![allow(dead_code)]

/// Adapter 类型标识。
pub const ADAPTER_TYPE: &str = "hermes";
/// Adapter UI 标签。
pub const ADAPTER_LABEL: &str = "Hermes Agent";

/// 默认 CLI 二进制名。
pub const HERMES_CLI: &str = "hermes";

/// 默认 timeout (秒)。
pub const DEFAULT_TIMEOUT_SEC: u64 = 1800;

/// SIGTERM → SIGKILL 之间宽限 (秒)。
pub const DEFAULT_GRACE_SEC: u64 = 10;

/// 默认模型：让 Hermes 从 `~/.hermes/config.yaml` 解析，避免 Paperclip
/// onboarding 期间覆盖用户配置的默认模型。
pub const DEFAULT_MODEL: &str = "auto";

/// Hermes `--provider` 合法值（必须与 `hermes chat --help` 保持同步）。
pub const VALID_PROVIDERS: &[&str] = &[
    "auto",
    "openrouter",
    "nous",
    "openai-codex",
    "copilot",
    "copilot-acp",
    "anthropic",
    "huggingface",
    "zai",
    "kimi-coding",
    "minimax",
    "minimax-cn",
    "kilocode",
];

/// 模型名前缀 → provider 推断（长前缀优先匹配）。
pub const MODEL_PREFIX_PROVIDER_HINTS: &[(&str, &str)] = &[
    ("gpt-4", "openai-codex"),
    ("gpt-5", "copilot"),
    ("o1-", "openai-codex"),
    ("o3-", "openai-codex"),
    ("o4-", "openai-codex"),
    ("claude", "anthropic"),
    ("gemini", "auto"),
    ("hermes-", "nous"),
    ("glm-", "zai"),
    ("moonshot", "kimi-coding"),
    ("kimi", "kimi-coding"),
    ("minimax", "minimax"),
    ("deepseek", "auto"),
    ("llama", "auto"),
    ("qwen", "auto"),
    ("mistral", "auto"),
    ("huggingface/", "huggingface"),
];

/// Session id 现代格式（quiet mode 行首 `session_id: <id>`）。
pub const SESSION_ID_REGEX_QUIET: &str = r"^session_id:\s*(\S+)";
/// Session id legacy 格式（行中 `Session id:` / `session_saved:`）。
pub const SESSION_ID_REGEX_LEGACY: &str = r"session[_ ](?:id|saved)[:\s]+([a-zA-Z0-9_-]+)";

/// Token usage 正则（input/output tokens）。
pub const TOKEN_USAGE_REGEX: &str =
    r"tokens?[:\s]+(\d+)\s*(?:input|in)\b.*?(\d+)\s*(?:output|out)\b";

/// Cost 正则。
pub const COST_REGEX: &str = r"(?:cost|spent)[:\s]*\$?([\d.]+)";

/// Hermes 工具输出前缀。
pub const TOOL_OUTPUT_PREFIX: &str = "┊";
/// Hermes thinking block 前缀。
pub const THINKING_PREFIX: &str = "💭";

/// 决定一个 provider 是否合法。
pub fn is_valid_provider(provider: &str) -> bool {
    VALID_PROVIDERS.iter().any(|p| *p == provider)
}

/// 根据模型名推断 provider（长前缀优先）。
pub fn infer_provider_from_model(model: &str) -> Option<&'static str> {
    let lower = model.to_ascii_lowercase();
    MODEL_PREFIX_PROVIDER_HINTS
        .iter()
        .find(|(prefix, _)| lower.starts_with(prefix))
        .map(|(_, provider)| *provider)
}

/// 提取 session id：先按 modern quiet 格式，再按 legacy 格式。返回 None 表示没找到。
pub fn extract_session_id(combined: &str) -> Option<String> {
    if let Some(id) = regex_lite::Regex::new(SESSION_ID_REGEX_QUIET)
        .ok()
        .and_then(|re| {
            re.captures(combined)
                .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        })
    {
        return Some(id);
    }
    let legacy_pattern = format!("(?i){SESSION_ID_REGEX_LEGACY}");
    regex_lite::Regex::new(&legacy_pattern).ok().and_then(|re| {
        re.captures(combined)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_providers_contains_known_entries() {
        for required in ["auto", "anthropic", "minimax", "minimax-cn"] {
            assert!(
                is_valid_provider(required),
                "missing valid provider: {required}"
            );
        }
    }

    #[test]
    fn infer_provider_handles_longest_prefix_first() {
        // gpt-5 比 gpt-4 更具体，必须优先
        assert_eq!(infer_provider_from_model("gpt-5.6-sol"), Some("copilot"));
        assert_eq!(
            infer_provider_from_model("gpt-4o-mini"),
            Some("openai-codex")
        );
        assert_eq!(
            infer_provider_from_model("claude-3.7-sonnet"),
            Some("anthropic")
        );
        assert_eq!(infer_provider_from_model("MiniMax-2.0"), Some("minimax"));
        assert_eq!(infer_provider_from_model("GLM-5-air"), Some("zai"));
    }

    #[test]
    fn infer_provider_returns_auto_or_unknown() {
        // 没有匹配项的模型返回 None（调用方会落到 auto fallback）
        assert_eq!(infer_provider_from_model("phi-3"), None);
    }
}
