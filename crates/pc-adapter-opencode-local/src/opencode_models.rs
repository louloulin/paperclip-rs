//! Opencode models 验证与发现（对齐 Node
//! `packages/adapters/opencode-local/src/server/models.ts`）。
//!
//! 核心函数：
//! - `is_valid_opencode_model_id` — 验证 `provider/model` 格式
//! - `require_opencode_model_id` — 必填校验，失败抛错
//! - `parse_opencode_models_output` — 解析 `opencode --list-models` 输出

use serde::{Deserialize, Serialize};

/// Adapter model 表示（对齐 Node `AdapterModel`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterModel {
    pub id: String,
    pub label: String,
}

/// 验证模型 ID 是否为合法 `provider/model` 格式。
///
/// 规则（与 Node `isValidOpenCodeModelId` 等价）：
/// - 必须包含 `/`
/// - 前后两段非空
/// - 每段仅含字母数字 + `-` `_` `.` `:` `/`
/// - 长度 1-256
pub fn is_valid_opencode_model_id(model: &str) -> bool {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed.len() > 256 {
        return false;
    }
    let Some(slash_idx) = trimmed.find('/') else {
        return false;
    };
    let (provider, model_id) = trimmed.split_at(slash_idx);
    let model_id = &model_id[1..]; // skip '/'
    if provider.is_empty() || model_id.is_empty() {
        return false;
    }
    is_valid_provider(provider) && is_valid_model_id(model_id)
}

fn is_valid_provider(provider: &str) -> bool {
    !provider.is_empty()
        && provider
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn is_valid_model_id(model_id: &str) -> bool {
    !model_id.is_empty()
        && model_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
}

/// 必填校验（与 Node `requireOpenCodeModelId` 一致）。
///
/// `""` 或非法格式返回 `Err(message)`。
pub fn require_opencode_model_id(input: Option<&str>) -> Result<String, String> {
    let model = input.unwrap_or("").trim();
    if !is_valid_opencode_model_id(model) {
        return Err(
            "OpenCode requires `adapterConfig.model` in provider/model format.".to_string(),
        );
    }
    Ok(model.to_string())
}

/// 解析 `opencode --list-models` 输出。
///
/// 每行第一个 token 形如 `provider/model`，构造为 `AdapterModel`。
/// 去重（按 id）+ 按 id 升序排序（与 Node 行为一致）。
pub fn parse_opencode_models_output(stdout: &str) -> Vec<AdapterModel> {
    let mut parsed: Vec<AdapterModel> = Vec::new();
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let first_token = line.split_whitespace().next().unwrap_or("").trim();
        if !first_token.contains('/') {
            continue;
        }
        let slash_idx = first_token.find('/').unwrap();
        let provider = first_token[..slash_idx].trim();
        let model_id = first_token[slash_idx + 1..].trim();
        if provider.is_empty() || model_id.is_empty() {
            continue;
        }
        parsed.push(AdapterModel {
            id: format!("{provider}/{model_id}"),
            label: format!("{provider}/{model_id}"),
        });
    }
    dedupe_models(parsed)
}

/// 按 id 去重（保留首次出现顺序）。
pub fn dedupe_models(models: Vec<AdapterModel>) -> Vec<AdapterModel> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped: Vec<AdapterModel> = Vec::new();
    for model in models {
        let id = model.id.trim();
        if id.is_empty() || seen.contains(id) {
            continue;
        }
        seen.insert(id.to_string());
        deduped.push(AdapterModel {
            id: id.to_string(),
            label: if model.label.trim().is_empty() {
                id.to_string()
            } else {
                model.label.trim().to_string()
            },
        });
    }
    deduped
}

/// 按 id 升序排序（locale-aware numeric）。
pub fn sort_models(models: Vec<AdapterModel>) -> Vec<AdapterModel> {
    let mut sorted = models;
    sorted.sort_by(|a, b| {
        a.id.to_lowercase()
            .chars()
            .zip(b.id.to_lowercase().chars())
            .map(|(x, y)| x.cmp(&y))
            .fold(std::cmp::Ordering::Equal, |acc, ord| acc.then(ord))
    });
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_opencode_model_id_accepts_typical_ids() {
        assert!(is_valid_opencode_model_id("anthropic/claude-3.5-sonnet"));
        assert!(is_valid_opencode_model_id("openai/gpt-4o"));
        assert!(is_valid_opencode_model_id(
            "openrouter/anthropic/claude-3-haiku"
        ));
        assert!(is_valid_opencode_model_id("zai/glm-4.5"));
    }

    #[test]
    fn is_valid_opencode_model_id_rejects_invalid() {
        assert!(!is_valid_opencode_model_id(""));
        assert!(!is_valid_opencode_model_id("no-slash"));
        assert!(!is_valid_opencode_model_id("/missing-provider"));
        assert!(!is_valid_opencode_model_id("missing-model/"));
        assert!(!is_valid_opencode_model_id("provider/with space"));
        assert!(!is_valid_opencode_model_id("provider/with!special"));
        assert!(!is_valid_opencode_model_id(&"x".repeat(300)));
    }

    #[test]
    fn require_validates_and_trims() {
        assert!(require_opencode_model_id(Some("anthropic/claude-3")).is_ok());
        assert!(require_opencode_model_id(Some("  anthropic/claude-3  ")).is_ok());
        assert!(require_opencode_model_id(Some("")).is_err());
        assert!(require_opencode_model_id(None).is_err());
    }

    #[test]
    fn parse_opencode_models_output_extracts_first_token() {
        let stdout = "\
anthropic/claude-3.5-sonnet     200K context
openai/gpt-4o                   128K context
anthropic/claude-3.5-sonnet     dup entry
";
        let models = parse_opencode_models_output(stdout);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "anthropic/claude-3.5-sonnet");
        assert_eq!(models[1].id, "openai/gpt-4o");
    }

    #[test]
    fn parse_handles_invalid_lines() {
        let stdout = "\
no slash here
anthropic/claude-3
/random
also-no-slash
";
        let models = parse_opencode_models_output(stdout);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "anthropic/claude-3");
    }

    #[test]
    fn parse_returns_empty_for_blank_input() {
        assert!(parse_opencode_models_output("").is_empty());
        assert!(parse_opencode_models_output("   \n  \n").is_empty());
    }

    #[test]
    fn dedupe_preserves_first_occurrence() {
        let models = vec![
            AdapterModel {
                id: "a/1".into(),
                label: "first".into(),
            },
            AdapterModel {
                id: "b/2".into(),
                label: "second".into(),
            },
            AdapterModel {
                id: "a/1".into(),
                label: "duplicate".into(),
            },
        ];
        let deduped = dedupe_models(models);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].label, "first");
    }

    #[test]
    fn dedupe_falls_back_label_to_id() {
        let models = vec![AdapterModel {
            id: "p/m".into(),
            label: "   ".into(),
        }];
        let deduped = dedupe_models(models);
        assert_eq!(deduped[0].label, "p/m");
    }

    #[test]
    fn sort_orders_alphabetically() {
        let models = vec![
            AdapterModel {
                id: "z/1".into(),
                label: "z".into(),
            },
            AdapterModel {
                id: "a/1".into(),
                label: "a".into(),
            },
            AdapterModel {
                id: "m/1".into(),
                label: "m".into(),
            },
        ];
        let sorted = sort_models(models);
        assert_eq!(sorted[0].id, "a/1");
        assert_eq!(sorted[1].id, "m/1");
        assert_eq!(sorted[2].id, "z/1");
    }
}
