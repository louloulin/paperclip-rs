//! Secret-like config key detection（与 Node `managed-config.ts` 的
//! `SECRET_LIKE_CONFIG_KEY_PATTERN` / `findSecretLikeConfigKey` 1:1 对齐）。
//!
//! Managed-config 文档绝不携带任何 secret；credentials 必须通过 env vars
//! 到达 managed instance。如果 config 中出现 secret-like key（"apiKey",
//! "api-key", "api_key", "token", "secret", "password", "credential" 等），
//! 解析器必须 fail-closed。

// ============================================================================
// Pattern
// ============================================================================

/// Secret-like config key pattern（与 Node
/// `/(api[-_]?key|token|secret|password|credential)/i` 1:1 对齐）。
pub const SECRET_LIKE_CONFIG_KEY_PATTERN_STR: &str =
    "(?i)(api[-_]?key|token|secret|password|credential)";

/// Lazy-compiled regex（运行期编译一次；no_std-friendly with `regex` lite）。
pub static SECRET_LIKE_CONFIG_KEY_PATTERN: once_cell_regex_lite::CompiledPattern =
    once_cell_regex_lite::CompiledPattern::new(SECRET_LIKE_CONFIG_KEY_PATTERN_STR);

// ============================================================================
// find_secret_like_config_key
// ============================================================================

/// 递归扫描任意 JSON-like value，返回第一个匹配 SECRET_LIKE_CONFIG_KEY_PATTERN
/// 的 key 路径（点号分隔 + 数组下标）；找不到则返回 `None`。
///
/// 与 Node `findSecretLikeConfigKey(value, path)` 1:1 对齐：
/// - 顶层对象：path 为 `""`，返回 `"key"` / `"parent.child"` 等
/// - 数组元素：path 为 `"items[0]"`，递归
pub fn find_secret_like_config_key(value: &serde_json::Value, path: &str) -> Option<String> {
    let obj = value.as_object()?;
    for (key, child) in obj {
        let child_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{}.{}", path, key)
        };

        if SECRET_LIKE_CONFIG_KEY_PATTERN.is_match(key) {
            return Some(child_path);
        }

        if child.is_object() {
            if let Some(nested) = find_secret_like_config_key(child, &child_path) {
                return Some(nested);
            }
        } else if child.is_array() {
            if let Some(arr) = child.as_array() {
                for (index, element) in arr.iter().enumerate() {
                    if !element.is_object() {
                        continue;
                    }
                    let indexed_path = format!("{}[{}]", child_path, index);
                    if let Some(nested) = find_secret_like_config_key(element, &indexed_path) {
                        return Some(nested);
                    }
                }
            }
        }
    }
    None
}

// ============================================================================
// Tiny inline regex helper
// ============================================================================

/// 简易 inline regex 容器（避免引入 `once_cell` 或 `lazy_static` 依赖）。
///
/// 使用 Rust 标准库的 pattern matching：对于 `(api[-_]?key|token|secret|password|credential)`
/// 我们手工实现一个 Aho-Corasick-like 替代品即可（pattern 短、case 简单）。
mod once_cell_regex_lite {
    use std::sync::OnceLock;

    /// A pre-compiled, lazily-initialized, thread-safe case-insensitive substring matcher.
    pub struct CompiledPattern {
        pattern: &'static str,
        regex: OnceLock<regex::Regex>,
    }

    impl CompiledPattern {
        pub const fn new(pattern: &'static str) -> Self {
            Self {
                pattern,
                regex: OnceLock::new(),
            }
        }

        pub fn is_match(&self, input: &str) -> bool {
            let re = self.regex.get_or_init(|| {
                regex::Regex::new(self.pattern)
                    .expect("SECRET_LIKE_CONFIG_KEY_PATTERN must compile")
            });
            re.is_match(input)
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pattern_matches_api_key() {
        assert!(SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("apiKey"));
        assert!(SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("api_key"));
        assert!(SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("api-key"));
        assert!(SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("API_KEY"));
    }

    #[test]
    fn pattern_matches_other_secret_words() {
        assert!(SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("token"));
        assert!(SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("secret"));
        assert!(SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("password"));
        assert!(SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("credential"));
        assert!(SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("myTOKEN"));
    }

    #[test]
    fn pattern_does_not_match_unrelated_keys() {
        assert!(!SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("target"));
        assert!(!SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("region"));
        assert!(!SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("apiVersion"));
        assert!(!SECRET_LIKE_CONFIG_KEY_PATTERN.is_match("description"));
    }

    #[test]
    fn find_top_level_secret() {
        let v = json!({"apiKey": "x"});
        assert_eq!(
            find_secret_like_config_key(&v, ""),
            Some("apiKey".to_string())
        );
    }

    #[test]
    fn find_nested_secret() {
        let v = json!({"outer": {"inner": {"token": "abc"}}});
        assert_eq!(
            find_secret_like_config_key(&v, ""),
            Some("outer.inner.token".to_string())
        );
    }

    #[test]
    fn find_array_element_secret() {
        let v = json!({"items": [{"name": "a"}, {"password": "b"}]});
        assert_eq!(
            find_secret_like_config_key(&v, ""),
            Some("items[1].password".to_string())
        );
    }

    #[test]
    fn no_secret_returns_none() {
        let v = json!({"target": "us-east-1", "region": "us"});
        assert_eq!(find_secret_like_config_key(&v, ""), None);
    }

    #[test]
    fn empty_object_returns_none() {
        let v = json!({});
        assert_eq!(find_secret_like_config_key(&v, ""), None);
    }

    #[test]
    fn non_object_returns_none() {
        assert_eq!(find_secret_like_config_key(&json!("string"), ""), None);
        assert_eq!(find_secret_like_config_key(&json!(42), ""), None);
        assert_eq!(find_secret_like_config_key(&json!(null), ""), None);
        assert_eq!(find_secret_like_config_key(&json!([1, 2, 3]), ""), None);
    }

    #[test]
    fn array_elements_that_are_not_objects_are_skipped() {
        let v = json!({"items": ["a", "b", "c"]});
        assert_eq!(find_secret_like_config_key(&v, ""), None);
    }

    #[test]
    fn deeply_nested_path_with_arrays() {
        let v = json!({"a": {"b": [{"c": {"apiKey": "x"}}]}});
        assert_eq!(
            find_secret_like_config_key(&v, ""),
            Some("a.b[0].c.apiKey".to_string())
        );
    }
}
