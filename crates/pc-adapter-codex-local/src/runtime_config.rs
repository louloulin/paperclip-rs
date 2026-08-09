//! Codex config.toml 合并与 Paperclip 托管块剥离（对齐 Node runtime-config.ts）。
//!
//! 提供：
//! - `MANAGED_*` 标记常量 — codex config.toml 中由 Paperclip 注入的可识别区域
//! - `strip_managed_block` — 通用标记块剥离
//! - `strip_managed_codex_provider_blocks` — 同时剥离 root + tables 块
//! - `expand_env_placeholders` — `{env:VAR}` 占位符展开（用于烘焙密钥）
//! - `parse_table_header_path` — TOML table 头解析（点号路径）

use std::collections::BTreeMap;

/// Paperclip 注入的 root 级标记块起始。
pub const MANAGED_ROOT_BEGIN: &str =
    "# >>> paperclip codex providers (root) -- managed, do not edit >>>";
pub const MANAGED_ROOT_END: &str = "# <<< paperclip codex providers (root) <<<";

/// Paperclip 注入的 table 级标记块起始。
pub const MANAGED_TABLES_BEGIN: &str =
    "# >>> paperclip codex providers (tables) -- managed, do not edit >>>";
pub const MANAGED_TABLES_END: &str = "# <<< paperclip codex providers (tables) <<<";

/// 把 `content` 中 `[begin ... end]` 标记对内部的行全部剔除。
///
/// 匹配规则：
/// - begin 行：精确等于 `begin`（trim 后）
/// - end 行：精确等于 `end`（trim 后）
/// - begin/end 之外的行：保留
/// - 若只有 begin 没有 end：剥到结尾
pub fn strip_managed_block(content: &str, begin: &str, end: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in content.split('\n') {
        let trimmed = line.trim();
        if in_block {
            if trimmed == end {
                in_block = false;
            }
            continue;
        }
        if trimmed == begin {
            in_block = true;
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

/// 同时剥离 Paperclip 在 codex `config.toml` 中注入的 root 块与 tables 块。
/// 对齐 Node `stripManagedCodexProviderBlocks`。
#[must_use]
pub fn strip_managed_codex_provider_blocks(content: &str) -> String {
    let stripped_root = strip_managed_block(content, MANAGED_ROOT_BEGIN, MANAGED_ROOT_END);
    strip_managed_block(&stripped_root, MANAGED_TABLES_BEGIN, MANAGED_TABLES_END)
}

/// 把字符串（递归包含数组/对象）中的 `{env:VAR}` 占位符替换为 `resolve` 返回的值。
/// 无法解析的占位符保留原样。对齐 Node `expandEnvPlaceholders`。
pub fn expand_env_placeholders<F>(value: &serde_json::Value, resolve: F) -> serde_json::Value
where
    F: Fn(&str) -> Option<String> + 'static,
{
    use serde_json::Value;
    fn walk(value: &Value, resolve: &dyn Fn(&str) -> Option<String>) -> Value {
        match value {
            Value::String(s) => {
                let mut out = String::with_capacity(s.len());
                let bytes = s.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    if let Some(rel) = s[i..].find("{env:") {
                        let abs = i + rel;
                        out.push_str(&s[i..abs]);
                        if let Some(close_rel) = s[abs..].find('}') {
                            let close = abs + close_rel;
                            let name = &s[abs + 5..close];
                            if !name.is_empty()
                                && name
                                    .chars()
                                    .next()
                                    .map(|c| c.is_ascii_alphabetic() || c == '_')
                                    .unwrap_or(false)
                                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                            {
                                if let Some(resolved) = resolve(name) {
                                    if !resolved.is_empty() {
                                        out.push_str(&resolved);
                                        i = close + 1;
                                        continue;
                                    }
                                }
                            }
                            out.push_str(&s[abs..close + 1]);
                            i = close + 1;
                        } else {
                            out.push_str(&s[abs..]);
                            i = s.len();
                        }
                    } else {
                        out.push_str(&s[i..]);
                        i = s.len();
                    }
                }
                Value::String(out)
            }
            Value::Array(arr) => Value::Array(arr.iter().map(|v| walk(v, resolve)).collect()),
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (k, v) in map {
                    out.insert(k.clone(), walk(v, resolve));
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }
    walk(value, &resolve)
}

/// 解析 `[a.b.c]` 形式的 TOML table 头为路径段数组。
/// 段名带引号会被剥离；空段名返回 `null`。
#[must_use]
pub fn parse_table_header_path(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    // 仅当整行以 [ 开头、] 结尾（允许中间含 # 注释）才认为合法
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    // 去除尾部注释（注释必须在 segments 之外；本解析不处理 quoted 段内 #）
    let without_comment = match inner.find('#') {
        Some(pos) => &inner[..pos],
        None => inner,
    };
    let without_comment = without_comment.trim();
    let segments: Vec<String> = without_comment
        .split('.')
        .map(|s| s.trim())
        .map(|s| {
            s.strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .map(str::to_string)
                .unwrap_or_else(|| s.to_string())
        })
        .collect();
    if segments.iter().any(String::is_empty) {
        None
    } else {
        Some(segments)
    }
}

/// 简易 TOML key 合法性检查（Node tomlKey 逻辑简化版）。
/// 真实情况下 codex provider id 应当是 `[A-Za-z0-9_-]+`。
#[must_use]
pub fn is_valid_toml_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 简易 env 变量名合法性检查（用于 `{env:NAME}` 中的 NAME）。
#[must_use]
pub fn is_valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_managed_block_keeps_unrelated_lines() {
        let content = "\
# top comment
key1 = 1
# >>> block begin >>>
# inside the block
key2 = 2
# <<< block end <<<
key3 = 3
";
        let stripped = strip_managed_block(content, "# >>> block begin >>>", "# <<< block end <<<");
        assert!(stripped.contains("key1 = 1"));
        assert!(stripped.contains("key3 = 3"));
        assert!(!stripped.contains("key2 = 2"));
        assert!(!stripped.contains("# inside the block"));
    }

    #[test]
    fn strip_managed_block_handles_missing_end() {
        let content = "\
key1 = 1
# >>> block begin >>>
key2 = 2
key3 = 3
";
        let stripped = strip_managed_block(content, "# >>> block begin >>>", "# <<< block end <<<");
        assert!(stripped.contains("key1 = 1"));
        assert!(!stripped.contains("key2 = 2"));
        assert!(!stripped.contains("key3 = 3"));
    }

    #[test]
    fn strip_managed_block_handles_missing_begin() {
        let content = "key1 = 1\nkey2 = 2\n";
        let stripped = strip_managed_block(content, "# >>> x >>>", "# <<< x <<<");
        assert_eq!(stripped, content);
    }

    #[test]
    fn strip_managed_codex_provider_blocks_removes_both_blocks() {
        let content = format!(
            "[model_providers.openai]\nname = \"OpenAI\"\n\
             {root_begin}\nmodel_provider = \"openai\"\n{root_end}\n\
             key = 1\n\
             {tables_begin}\n[model_providers.openai]\nname = \"X\"\n{tables_end}\n\
             key = 2\n",
            root_begin = MANAGED_ROOT_BEGIN,
            root_end = MANAGED_ROOT_END,
            tables_begin = MANAGED_TABLES_BEGIN,
            tables_end = MANAGED_TABLES_END,
        );
        let stripped = strip_managed_codex_provider_blocks(&content);
        assert!(stripped.contains("key = 1"));
        assert!(stripped.contains("key = 2"));
        // 没有 managed 块的内容应被剥离
        assert!(!stripped.contains("model_provider = \"openai\""));
        assert!(!stripped.contains("[model_providers.openai]\nname = \"X\""));
    }

    #[test]
    fn parse_table_header_path_simple() {
        assert_eq!(
            parse_table_header_path("[a.b.c]"),
            Some(vec!["a".into(), "b".into(), "c".into()])
        );
    }

    #[test]
    fn parse_table_header_path_with_spaces() {
        assert_eq!(
            parse_table_header_path("  [ a . b ]  "),
            Some(vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn parse_table_header_path_with_inline_comment() {
        // 行内注释位于 segments 之间，解析器应识别为 # 注释终止符
        assert_eq!(
            parse_table_header_path("[ a . b ]"),
            Some(vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn parse_table_header_path_quoted_segments() {
        assert_eq!(
            parse_table_header_path("[\"foo\".\"bar\"]"),
            Some(vec!["foo".into(), "bar".into()])
        );
    }

    #[test]
    fn parse_table_header_path_invalid_returns_none() {
        assert!(parse_table_header_path("not a header").is_none());
        assert!(parse_table_header_path("[a..b]").is_none());
    }

    #[test]
    fn is_valid_toml_key_accepts_basic() {
        assert!(is_valid_toml_key("model_providers"));
        assert!(is_valid_toml_key("openai-1"));
        assert!(is_valid_toml_key("_underscore"));
    }

    #[test]
    fn is_valid_toml_key_rejects_invalid() {
        assert!(!is_valid_toml_key(""));
        assert!(!is_valid_toml_key("with space"));
        assert!(!is_valid_toml_key("with.dot"));
    }

    #[test]
    fn is_valid_env_name_follows_shell_rules() {
        assert!(is_valid_env_name("OPENAI_API_KEY"));
        assert!(is_valid_env_name("_PRIVATE"));
        assert!(!is_valid_env_name("1INVALID"));
        assert!(!is_valid_env_name(""));
        assert!(!is_valid_env_name("WITH-DASH"));
    }

    #[test]
    fn expand_env_placeholders_replaces_in_string() {
        let resolver = |name: &str| match name {
            "FOO" => Some("foo_value".to_string()),
            _ => None,
        };
        let v = serde_json::json!("prefix-{env:FOO}-suffix");
        let result = expand_env_placeholders(&v, resolver);
        assert_eq!(result, serde_json::json!("prefix-foo_value-suffix"));
    }

    #[test]
    fn expand_env_placeholders_keeps_unresolved() {
        let v = serde_json::json!("value is {env:MISSING} here");
        let result = expand_env_placeholders(&v, |_| None);
        assert_eq!(result, serde_json::json!("value is {env:MISSING} here"));
    }

    #[test]
    fn expand_env_placeholders_recurses_into_objects() {
        let v = serde_json::json!({
            "name": "x",
            "url": "https://{env:HOST}:8080",
            "nested": { "key": "{env:TOKEN}" }
        });
        let resolver = |name: &str| match name {
            "HOST" => Some("example.com".to_string()),
            "TOKEN" => Some("secret".to_string()),
            _ => None,
        };
        let result = expand_env_placeholders(&v, resolver);
        assert_eq!(result["name"], "x");
        assert_eq!(result["url"], "https://example.com:8080");
        assert_eq!(result["nested"]["key"], "secret");
    }

    #[test]
    fn expand_env_placeholders_handles_arrays() {
        let v = serde_json::json!(["a-{env:X}", "b-{env:Y}", "c"]);
        let resolver = |name: &str| match name {
            "X" => Some("X-val".to_string()),
            "Y" => None,
            _ => None,
        };
        let result = expand_env_placeholders(&v, resolver);
        assert_eq!(result, serde_json::json!(["a-X-val", "b-{env:Y}", "c"]));
    }

    #[test]
    fn managed_constants_are_distinct() {
        assert_ne!(MANAGED_ROOT_BEGIN, MANAGED_TABLES_BEGIN);
        assert_ne!(MANAGED_ROOT_END, MANAGED_TABLES_END);
    }
}
