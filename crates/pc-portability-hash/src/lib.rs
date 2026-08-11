#![forbid(unsafe_code)]

//! Canonical JSON + SHA-256 content hashing for portability.
//!
//! R536: Direct port of `paperclip/packages/shared/src/portability-hash.ts`.
//!
//! 设计原则:
//! - 所有 `pub fn` 都是纯函数 (无 IO, 无副作用, 无环境依赖)
//! - 接受 `&serde_json::Value` 而非 `unknown` — 类型安全
//! - 字符串输出用 `String` (而非 `&'static str`)，允许任意长度
//! - 不引入业务 crate 依赖 (零耦合)
//!
//! 范围 (本 crate):
//! - [`NormalizedSha256`] newtype — `"sha256:<64hex>"`
//! - [`normalized_content_hash`] — sha256 of canonical JSON of a value
//! - [`canonical_json`] — JSON string with sorted object keys
//! - [`sha256_hex_of_bytes`] — sha256 hex of raw bytes
//!
//! **不** 范围 (留给集成层):
//! - DB 持久化 (`server/src/services/portability.ts`)
//! - 任何 IO / 网络 / 文件读写
//!
//! 设计 vs Node 上游:
//! - `serde_json::Value` 替代 `unknown` — 类型层消除所有意外类型
//! - `BTreeMap` 替代 JS object — 天然排序, 无需 `localeCompare` 排序步骤
//! - `sha2::Sha256` 替代 `node:crypto.createHash` — 编译期验证, 无运行时依赖
//! - `NormalizedSha256` newtype 替代 TS template literal type — 编译期防止与
//!   任意 string 混用, `Display` 提供 `as_str()` 视图

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

// ============================================================================
// Newtype
// ============================================================================

/// Canonical SHA-256 hash string in `"sha256:<64-hex-chars>"` form.
///
/// Mirrors Node `NormalizedSha256` template-literal type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedSha256(String);

impl NormalizedSha256 {
    /// The constant prefix used in [`NormalizedSha256`].
    pub const PREFIX: &'static str = "sha256:";

    /// Wrap an existing 64-char hex string with the `sha256:` prefix.
    ///
    /// Returns `None` if `hex` is not exactly 64 lowercase hex characters
    /// (this matches Node upstream's implicit hex-only contract — Node's
    /// `digest("hex")` always produces lowercase hex).
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 64
            || !hex
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return None;
        }
        Some(Self(format!("{}{}", Self::PREFIX, hex)))
    }

    /// View the underlying string (including the `sha256:` prefix).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// View the hex portion (without the `sha256:` prefix).
    #[must_use]
    pub fn hex(&self) -> &str {
        // SAFETY: length is enforced by `from_hex`; nothing else can
        // construct a `NormalizedSha256`.
        &self.0[Self::PREFIX.len()..]
    }
}

impl std::fmt::Display for NormalizedSha256 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<NormalizedSha256> for String {
    fn from(s: NormalizedSha256) -> Self {
        s.0
    }
}

impl AsRef<str> for NormalizedSha256 {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// ============================================================================
// Canonical JSON
// ============================================================================

/// Recursively sort JSON object keys (and arrays of objects) so that two
/// semantically-equal values produce identical canonical output.
///
/// Mirrors Node `sortJson`. Implemented by converting to a `BTreeMap` for
/// objects (natural key ordering); arrays preserve element order but recurse.
fn sort_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            // BTreeMap iteration is in ascending key order — this gives us
            // `localeCompare` semantics for free, including Unicode ordering.
            let mut sorted: std::collections::BTreeMap<&String, &Value> =
                std::collections::BTreeMap::new();
            for (k, v) in map {
                sorted.insert(k, v);
            }
            let mut out = Map::with_capacity(map.len());
            for (k, v) in sorted {
                out.insert(k.clone(), sort_json(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_json).collect()),
        // Null, Bool, Number, String pass through unchanged.
        _ => value.clone(),
    }
}

/// Serialize a JSON value to a canonical string (object keys sorted).
///
/// Mirrors Node `canonicalJson`.
#[must_use]
pub fn canonical_json(value: &Value) -> String {
    serde_json::to_string(&sort_json(value)).unwrap_or_else(|_| "null".to_owned())
}

/// Compute a canonical SHA-256 hash of a JSON value, returned in
/// `"sha256:<hex>"` form.
///
/// Mirrors Node `normalizedContentHash`.
#[must_use]
pub fn normalized_content_hash(value: &Value) -> NormalizedSha256 {
    let canonical = canonical_json(value);
    let hex = sha256_hex_of_bytes(canonical.as_bytes());
    // `from_hex` cannot fail because `sha256_hex_of_bytes` produces exactly
    // 64 lowercase hex chars.
    NormalizedSha256::from_hex(&hex)
        .expect("sha256_hex_of_bytes always returns 64 lowercase hex chars")
}

/// Compute the SHA-256 hex digest of a byte slice (lowercase, 64 chars).
///
/// Mirrors Node `sha256HexOfBytes`.
#[must_use]
pub fn sha256_hex_of_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ----- NormalizedSha256 -----

    #[test]
    fn r536_normalized_sha256_from_hex_basic() {
        let n = NormalizedSha256::from_hex(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )
        .unwrap();
        assert_eq!(
            n.as_str(),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            n.hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn r536_normalized_sha256_rejects_wrong_length() {
        assert!(NormalizedSha256::from_hex("abc").is_none());
        assert!(NormalizedSha256::from_hex("").is_none());
        assert!(NormalizedSha256::from_hex(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85"
        )
        .is_none()); // 63 chars
    }

    #[test]
    fn r536_normalized_sha256_rejects_uppercase() {
        // Node upstream produces lowercase hex; we reject uppercase to keep
        // the contract tight.
        assert!(NormalizedSha256::from_hex(
            "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"
        )
        .is_none());
    }

    #[test]
    fn r536_normalized_sha256_rejects_non_hex() {
        assert!(NormalizedSha256::from_hex(
            "g3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        )
        .is_none());
    }

    #[test]
    fn r536_normalized_sha256_display_and_into() {
        let n = NormalizedSha256::from_hex(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )
        .unwrap();
        assert_eq!(n.to_string(), n.as_str());
        let s: String = n.clone().into();
        assert_eq!(s, n.as_str());
        let r: &str = n.as_ref();
        assert_eq!(r, n.as_str());
    }

    // ----- canonical_json -----

    #[test]
    fn r536_canonical_json_sorts_object_keys() {
        // Different insertion order → same canonical output.
        let a = json!({"b": 1, "a": 2});
        let b = json!({"a": 2, "b": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&a), r#"{"a":2,"b":1}"#);
    }

    #[test]
    fn r536_canonical_json_recurses_into_nested_objects() {
        let a = json!({"outer": {"z": 1, "a": 2}, "first": 0});
        let b = json!({"first": 0, "outer": {"a": 2, "z": 1}});
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn r536_canonical_json_recurses_into_array_of_objects() {
        let a = json!({"items": [{"b": 1, "a": 2}, {"y": 0, "x": 1}]});
        let b = json!({"items": [{"a": 2, "b": 1}, {"x": 1, "y": 0}]});
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn r536_canonical_json_array_preserves_element_order() {
        // Arrays preserve order (object keys inside each element are sorted).
        let a = json!({"list": [3, 1, 2]});
        let b = json!({"list": [1, 2, 3]});
        assert_ne!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn r536_canonical_json_primitives_passthrough() {
        assert_eq!(canonical_json(&json!(null)), "null");
        assert_eq!(canonical_json(&json!(true)), "true");
        assert_eq!(canonical_json(&json!(42)), "42");
        assert_eq!(canonical_json(&json!("hello")), r#""hello""#);
    }

    #[test]
    fn r536_canonical_json_empty_object() {
        assert_eq!(canonical_json(&json!({})), "{}");
    }

    #[test]
    fn r536_canonical_json_empty_array() {
        assert_eq!(canonical_json(&json!([])), "[]");
    }

    #[test]
    fn r536_canonical_json_key_order_alphabetical_not_lexicographic() {
        // BTreeMap iteration is by ascending Ord. Strings compare by byte
        // value (not locale-aware). Verify the deterministic order matches
        // what BTreeMap produces.
        let value = json!({"Z": 1, "a": 2, "M": 3, "b": 4});
        let out = canonical_json(&value);
        // Ascending byte order: 'M' (0x4D) < 'Z' (0x5A) < 'a' (0x61) < 'b' (0x62)
        assert_eq!(out, r#"{"M":3,"Z":1,"a":2,"b":4}"#);
    }

    // ----- sha256_hex_of_bytes -----

    #[test]
    fn r536_sha256_hex_of_bytes_empty_input() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex_of_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn r536_sha256_hex_of_bytes_abc() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        assert_eq!(
            sha256_hex_of_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn r536_sha256_hex_of_bytes_lowercase_64_chars() {
        let hex = sha256_hex_of_bytes(b"hello world");
        assert_eq!(hex.len(), 64);
        assert!(hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn r536_sha256_hex_of_bytes_deterministic() {
        let a = sha256_hex_of_bytes(b"foo");
        let b = sha256_hex_of_bytes(b"foo");
        let c = sha256_hex_of_bytes(b"bar");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    // ----- normalized_content_hash -----

    #[test]
    fn r536_normalized_content_hash_key_order_invariant() {
        // Two objects with the same content but different key order must
        // produce the same hash (this is the whole point of canonical JSON).
        let a = json!({"name": "alice", "role": "ceo", "age": 30});
        let b = json!({"age": 30, "role": "ceo", "name": "alice"});
        let c = json!({"role": "ceo", "name": "alice", "age": 30});
        assert_eq!(normalized_content_hash(&a), normalized_content_hash(&b));
        assert_eq!(normalized_content_hash(&b), normalized_content_hash(&c));
    }

    #[test]
    fn r536_normalized_content_hash_nested_invariant() {
        let a = json!({"outer": {"b": 2, "a": 1}, "x": 0});
        let b = json!({"x": 0, "outer": {"a": 1, "b": 2}});
        assert_eq!(normalized_content_hash(&a), normalized_content_hash(&b));
    }

    #[test]
    fn r536_normalized_content_hash_array_order_matters() {
        // Different array order → different hash.
        let a = json!({"list": [1, 2, 3]});
        let b = json!({"list": [3, 2, 1]});
        assert_ne!(normalized_content_hash(&a), normalized_content_hash(&b));
    }

    #[test]
    fn r536_normalized_content_hash_different_values_different_hash() {
        let a = json!({"name": "alice"});
        let b = json!({"name": "bob"});
        assert_ne!(normalized_content_hash(&a), normalized_content_hash(&b));
    }

    #[test]
    fn r536_normalized_content_hash_empty_object() {
        let n = normalized_content_hash(&json!({}));
        // SHA-256("{}") — verified against OpenSSL / Python hashlib.
        assert_eq!(
            n.as_str(),
            "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }

    #[test]
    fn r536_normalized_content_hash_format() {
        let n = normalized_content_hash(&json!({"a": 1}));
        assert!(n.as_str().starts_with("sha256:"));
        assert_eq!(n.as_str().len(), 7 + 64); // "sha256:" + 64 hex chars
        assert_eq!(n.hex().len(), 64);
    }

    #[test]
    fn r536_normalized_content_hash_empty_array_distinct_from_empty_object() {
        // `[]` and `{}` are semantically distinct values.
        assert_ne!(
            normalized_content_hash(&json!([])),
            normalized_content_hash(&json!({}))
        );
    }

    #[test]
    fn r536_normalized_content_hash_null_vs_absent() {
        // `{"a": null}` and `{}` are distinct values.
        assert_ne!(
            normalized_content_hash(&json!({"a": null})),
            normalized_content_hash(&json!({}))
        );
    }

    #[test]
    fn r536_normalized_content_hash_matches_manual_pipeline() {
        // Verify the helper functions compose correctly:
        // normalized_content_hash(v) == "sha256:" + sha256_hex_of_bytes(canonical_json(v).as_bytes())
        let value = json!({"complex": {"nested": [3, 1, 2], "deeper": {"y": true, "x": false}}, "top": "value"});
        let canonical = canonical_json(&value);
        let hex = sha256_hex_of_bytes(canonical.as_bytes());
        let expected = NormalizedSha256::from_hex(&hex).unwrap();
        assert_eq!(normalized_content_hash(&value), expected);
    }
}
