//! Hermes 模型/提供商/凭据探测（对齐 Node
//! `packages/adapters/hermes/src/server/detect-model.ts`）。
//!
//! Hermes 用户的全局配置位于 `~/.hermes/config.yaml`。该模块用纯 Rust 正则
//! 解析（不引入 YAML 依赖，对齐 Node 的"避免 YAML 依赖"决策）提取：
//! - `model:` 块的 `default` / `provider` / `base_url` / `api_key` / `api_mode`
//!
//! `api_key` 仅记录"是否非空"（与 Node 行为一致），从不暴露任何凭据字节。

use std::path::{Path, PathBuf};

/// 从 `~/.hermes/config.yaml` 检测到的模型快照。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DetectedModel {
    pub model: String,
    pub provider: String,
    pub base_url: String,
    pub has_api_key: bool,
    pub api_mode: String,
}

/// 默认 Hermes config 路径（`~/.hermes/config.yaml`）。
pub fn default_hermes_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".hermes").join("config.yaml"))
}

/// 解析 YAML 文本 → `DetectedModel`。失败（缺 `model:` 块 / 空内容）
/// 返回 `None`。绝不抛错。
pub fn parse_model_from_config(content: &str) -> Option<DetectedModel> {
    let mut detected = DetectedModel::default();
    let mut in_model_section = false;
    let mut model_section_indent: usize = 0;

    for line in content.lines() {
        let trimmed = line.trim_end();
        let indent = line.len() - line.trim_start().len();

        // 进入 `model:` 顶层节（仅匹配 indent == 0）
        if indent == 0 && trimmed == "model:" {
            in_model_section = true;
            model_section_indent = 0;
            continue;
        }

        // 离开 model 节：缩进回到顶层或更浅、且非空、非注释
        if in_model_section
            && indent <= model_section_indent
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
        {
            in_model_section = false;
        }

        if !in_model_section {
            continue;
        }

        // 匹配 `key: value` — 仅取顶层 model.* 字段
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "default" => detected.model = strip_yaml_scalar(value),
                "provider" => detected.provider = strip_yaml_scalar(value),
                "base_url" => detected.base_url = strip_yaml_scalar(value),
                "api_mode" => detected.api_mode = strip_yaml_scalar(value),
                "api_key" => {
                    detected.has_api_key = !value.is_empty() && value != "''" && value != "\"\""
                }
                _ => {}
            }
        }
    }

    if detected.model.is_empty() {
        None
    } else {
        Some(detected)
    }
}

/// 去除 YAML 单引号 / 双引号包裹（与 Node `parseModelFromConfig` 行为一致）。
fn strip_yaml_scalar(raw: &str) -> String {
    let raw = raw.trim();
    if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
    {
        raw[1..raw.len() - 1].to_string()
    } else {
        raw.to_string()
    }
}

/// 从 `path` 读取文件并解析。失败（文件不存在 / IO 错误 / 解析失败）
/// 返回 `None`，永不抛错。
pub async fn detect_model_from_path(path: &Path) -> Option<DetectedModel> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    parse_model_from_config(&content)
}

/// 自动探测（用 `HERMES_CONFIG_PATH` 环境变量覆盖，否则 `~/.hermes/config.yaml`）。
pub async fn detect_model() -> Option<DetectedModel> {
    let path = std::env::var_os("HERMES_CONFIG_PATH")
        .map(PathBuf::from)
        .or_else(default_hermes_config_path)?;
    detect_model_from_path(&path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_fixture(content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hermes-detect-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("config.yaml");
        let mut handle = std::fs::File::create(&path).expect("create fixture");
        handle.write_all(content.as_bytes()).expect("write");
        path
    }

    #[test]
    fn parses_full_model_block() {
        let yaml = "\
model:
  default: anthropic/claude-sonnet-4
  provider: anthropic
  base_url: https://api.anthropic.com
  api_mode: chat_completions
  api_key: sk-ant-redacted
";
        let detected = parse_model_from_config(yaml).expect("parse");
        assert_eq!(detected.model, "anthropic/claude-sonnet-4");
        assert_eq!(detected.provider, "anthropic");
        assert_eq!(detected.base_url, "https://api.anthropic.com");
        assert_eq!(detected.api_mode, "chat_completions");
        assert!(detected.has_api_key);
    }

    #[test]
    fn parses_quoted_scalars() {
        let yaml = "\
model:
  default: \"gpt-5\"
  provider: 'openai-codex'
";
        let detected = parse_model_from_config(yaml).expect("parse");
        assert_eq!(detected.model, "gpt-5");
        assert_eq!(detected.provider, "openai-codex");
    }

    #[test]
    fn missing_model_returns_none() {
        let yaml = "other:\n  key: value\n";
        assert!(parse_model_from_config(yaml).is_none());
    }

    #[test]
    fn empty_model_field_returns_none() {
        let yaml = "model:\n  provider: anthropic\n";
        assert!(parse_model_from_config(yaml).is_none());
    }

    #[test]
    fn api_key_blank_treated_as_no_key() {
        let yaml = "model:\n  default: foo\n  api_key: \"\"\n";
        let detected = parse_model_from_config(yaml).expect("parse");
        assert!(!detected.has_api_key);
    }

    #[test]
    fn leaves_model_section_when_indent_returns_to_zero() {
        let yaml = "\
model:
  default: foo
tools:
  enabled:
    - terminal
";
        let detected = parse_model_from_config(yaml).expect("parse");
        assert_eq!(detected.model, "foo");
        assert_eq!(detected.provider, "");
        assert_eq!(detected.base_url, "");
    }

    #[tokio::test]
    async fn detect_model_from_path_reads_real_file() {
        let path = write_fixture("model:\n  default: detected-model\n  provider: anthropic\n");
        let detected = detect_model_from_path(&path).await.expect("detect");
        assert_eq!(detected.model, "detected-model");
        assert_eq!(detected.provider, "anthropic");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn detect_model_from_path_returns_none_when_missing() {
        let path = std::env::temp_dir().join(format!("hermes-absent-{}", uuid::Uuid::new_v4()));
        assert!(detect_model_from_path(&path).await.is_none());
    }
}
