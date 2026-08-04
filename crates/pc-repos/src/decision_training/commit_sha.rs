//! `decision_training::commit_sha` —— commit SHA 提取工具（纯逻辑，无 IO）。
//!
//! 两个公开函数：
//! - [`json_copy`] —— 深拷贝（与 Node `JSON.parse(JSON.stringify(value))` 1:1 对齐）
//! - [`find_commit_sha`] —— 递归搜索嵌套 JSON 中的 commit SHA 字段
//!   - 支持 key 候选：`commitSha` / `commitSHA` / `gitCommitSha` / `headSha` / `commit`
//!   - SHA 格式：`/^[0-9a-f]{7,64}$/i`（7-64 位十六进制）
//!
//! 不持有状态；不依赖 IO。

use serde_json::Value;

/// 深拷贝（与 Node `JSON.parse(JSON.stringify(value))` 1:1 对齐）。
///
/// 返回 plain JSON object；调用方传非 object 时降级为 `Value::Object({})`。
#[must_use]
pub fn json_copy(value: &Value) -> Value {
    serde_json::from_str(&serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()))
        .unwrap_or(Value::Object(serde_json::Map::new()))
}

/// 候选 commit SHA key 列表（按 Node 端查找顺序）。
pub const COMMIT_SHA_KEYS: &[&str] = &[
    "commitSha",
    "commitSHA",
    "gitCommitSha",
    "headSha",
    "commit",
];

/// 在嵌套 JSON 中递归查找 commit SHA。
///
/// 行为（与 Node `findCommitSha` 1:1 对齐）：
/// 1. 非对象（null / 字符串 / 数字）→ `None`
/// 2. 数组 → 递归每个元素
/// 3. 对象 → 先查 `COMMIT_SHA_KEYS` 5 个候选 key；命中且匹配 `/^[0-9a-f]{7,64}$/i` → `Some(sha)`
/// 4. 否则递归所有 value
#[must_use]
pub fn find_commit_sha(value: &Value) -> Option<String> {
    if value.is_null() || !value.is_object() && !value.is_array() {
        return None;
    }
    if let Some(arr) = value.as_array() {
        for item in arr {
            if let Some(found) = find_commit_sha(item) {
                return Some(found);
            }
        }
        return None;
    }
    let record = value.as_object()?;
    for key in COMMIT_SHA_KEYS {
        if let Some(candidate) = record.get(*key).and_then(Value::as_str) {
            if is_commit_sha(candidate) {
                return Some(candidate.to_string());
            }
        }
    }
    for nested in record.values() {
        if let Some(found) = find_commit_sha(nested) {
            return Some(found);
        }
    }
    None
}

/// 判断字符串是否是合法 commit SHA（7-64 位十六进制）。
#[must_use]
pub fn is_commit_sha(s: &str) -> bool {
    if s.len() < 7 || s.len() > 64 {
        return false;
    }
    s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- json_copy ----

    #[test]
    fn json_copy_object_is_deep_copy() {
        let original = json!({"a": 1, "b": [1, 2, {"c": 3}]});
        let copy = json_copy(&original);
        assert_eq!(original, copy);
        // 不同的 String 表示
        assert_eq!(original.to_string(), copy.to_string());
    }

    #[test]
    fn json_copy_handles_null() {
        // JSON round-trip of `null` is `null`
        let copy = json_copy(&Value::Null);
        assert_eq!(copy, Value::Null);
    }

    #[test]
    fn json_copy_handles_array() {
        let original = json!([1, 2, 3]);
        let copy = json_copy(&original);
        assert_eq!(copy, original);
    }

    // ---- is_commit_sha ----

    #[test]
    fn is_commit_sha_accepts_short_hex() {
        assert!(is_commit_sha("abc1234"));
        assert!(is_commit_sha("ABC1234"));
        assert!(is_commit_sha("1234567"));
    }

    #[test]
    fn is_commit_sha_accepts_long_hex() {
        assert!(is_commit_sha(&"a".repeat(40))); // git SHA-1
        assert!(is_commit_sha(&"a".repeat(64))); // git SHA-256
    }

    #[test]
    fn is_commit_sha_rejects_too_short() {
        assert!(!is_commit_sha("abc123")); // 6 chars
        assert!(!is_commit_sha(""));
    }

    #[test]
    fn is_commit_sha_rejects_too_long() {
        assert!(!is_commit_sha(&"a".repeat(65)));
    }

    #[test]
    fn is_commit_sha_rejects_non_hex() {
        assert!(!is_commit_sha("xyz1234"));
        assert!(!is_commit_sha("abc123z"));
        assert!(!is_commit_sha("abc-123"));
    }

    // ---- find_commit_sha ----

    #[test]
    fn find_commit_sha_top_level_commit_sha_key() {
        let v = json!({"commitSha": "abc1234"});
        assert_eq!(find_commit_sha(&v), Some("abc1234".to_string()));
    }

    #[test]
    fn find_commit_sha_accepts_commit_sha_uppercase_key() {
        let v = json!({"commitSHA": "abc1234"});
        assert_eq!(find_commit_sha(&v), Some("abc1234".to_string()));
    }

    #[test]
    fn find_commit_sha_accepts_git_commit_sha() {
        let v = json!({"gitCommitSha": "abc1234"});
        assert_eq!(find_commit_sha(&v), Some("abc1234".to_string()));
    }

    #[test]
    fn find_commit_sha_accepts_head_sha() {
        let v = json!({"headSha": "abc1234"});
        assert_eq!(find_commit_sha(&v), Some("abc1234".to_string()));
    }

    #[test]
    fn find_commit_sha_accepts_commit_key() {
        let v = json!({"commit": "abc1234"});
        assert_eq!(find_commit_sha(&v), Some("abc1234".to_string()));
    }

    #[test]
    fn find_commit_sha_searches_nested_object() {
        let v = json!({
            "metadata": {"repo": {"commitSha": "abc1234"}}
        });
        assert_eq!(find_commit_sha(&v), Some("abc1234".to_string()));
    }

    #[test]
    fn find_commit_sha_searches_arrays() {
        let v = json!({
            "commits": [
                {"sha": "wrong"},
                {"commitSha": "abc1234"}
            ]
        });
        assert_eq!(find_commit_sha(&v), Some("abc1234".to_string()));
    }

    #[test]
    fn find_commit_sha_invalid_value_returns_none() {
        let v = json!({"commitSha": "xyz1234"}); // not hex
        assert_eq!(find_commit_sha(&v), None);
    }

    #[test]
    fn find_commit_sha_short_value_returns_none() {
        let v = json!({"commitSha": "abc123"}); // 6 chars
        assert_eq!(find_commit_sha(&v), None);
    }

    #[test]
    fn find_commit_sha_returns_none_for_non_object() {
        assert_eq!(find_commit_sha(&Value::Null), None);
        assert_eq!(find_commit_sha(&json!("string")), None);
        assert_eq!(find_commit_sha(&json!(42)), None);
        assert_eq!(find_commit_sha(&json!(true)), None);
    }

    #[test]
    fn find_commit_sha_returns_none_when_not_found() {
        let v = json!({"unrelated": "value", "nested": {"x": 1}});
        assert_eq!(find_commit_sha(&v), None);
    }

    #[test]
    fn find_commit_sha_returns_none_for_empty_object() {
        let v = json!({});
        assert_eq!(find_commit_sha(&v), None);
    }

    #[test]
    fn find_commit_sha_returns_none_for_empty_array() {
        let v = json!([]);
        assert_eq!(find_commit_sha(&v), None);
    }

    #[test]
    fn find_commit_sha_prioritizes_top_level_match() {
        // 第一个匹配的 key 优先（数组顺序）
        let v = json!({
            "headSha": "abc1234",
            "nested": {"commitSha": "def5678"}
        });
        // headSha 在 COMMIT_SHA_KEYS 中排在 commitSha 之前
        assert_eq!(find_commit_sha(&v), Some("abc1234".to_string()));
    }
}
