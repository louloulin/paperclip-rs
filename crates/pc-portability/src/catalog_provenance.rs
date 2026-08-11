#![forbid(unsafe_code)]
//! `pc-catalog-provenance` —— Portable catalog provenance 提取。
//!
//! 对应 Node `server/src/services/catalog-provenance.ts`（65 行）。
//!
//! ## 设计目标
//!
//! - 从任意嵌套 `metadata` 中提取 `metadata.paperclip.catalog.{...}` 字段
//! - 提供可序列化的 `sourceRef` + `metadata` 视图，含 `skillKey` / `sourceKind` / 各审计字段
//! - **零运行时 + 纯函数**：不依赖 DB / 网络
//!
//! ## 公共 API
//!
//! - [`PORTABLE_CATALOG_PROVENANCE_STRING_KEYS`] —— 可识别的 string 字段名元组
//! - [`read_portable_catalog_provenance`] —— 顶层入口（与 Node `readPortableCatalogProvenance` 1:1 对齐）
//! - [`read_catalog_string_list`] —— 校验并过滤 string 数组
//! - [`as_catalog_string`] —— trim + 非空校验
//! - [`Provenance`] / [`ProvenanceMetadata`] —— 输出 DTO
//!
//! ## 设计原则
//!
//! - **高内聚**：catalog 解析逻辑集中在本 crate。
//! - **无 IO 依赖**：仅 `serde_json`，便于内嵌调用。
//! - **可测**：纯函数 + 大量单测（含空值、嵌套、部分字段等用例）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ============================================================================
// Constants
// ============================================================================

/// `metadata.paperclip.catalog` 内可识别的 string 字段名（固定顺序，1:1 对齐 Node）。
///
/// 注意：`sourceRef` 不在此列表中 —— 它是 **synthesized** 字段（`originHash` fallback）。
pub const PORTABLE_CATALOG_PROVENANCE_STRING_KEYS: &[&str] = &[
    "sourceRef",
    "originHash",
    "catalogId",
    "catalogKey",
    "catalogKind",
    "catalogCategory",
    "catalogPath",
    "packageName",
    "packageVersion",
    "originVersion",
    "installedHash",
    "userModifiedAt",
    "updateHoldReason",
    "auditVerdict",
    "auditScannedAt",
    "auditScanVersion",
];

const SOURCE_KIND: &str = "catalog";

// ============================================================================
// Types
// ============================================================================

/// Provenance 元数据视图（与 Node `normalized` 对象 1:1 对齐）。
///
/// `serde_json::Map` 而非 `BTreeMap`：保留 caller 提供的 key 顺序。
pub type ProvenanceMetadata = serde_json::Map<String, Value>;

/// Provenance 结果（与 Node `return { sourceRef, metadata }` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    pub metadata: ProvenanceMetadata,
}

// ============================================================================
// Pure helpers
// ============================================================================

/// `value` 是非空字符串 → trim 后非空返回；否则 `None`。
///
/// 与 Node `asCatalogString(value)` 1:1 对齐。
pub fn as_catalog_string(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 校验 `value` 是 string[]，trim + 非空过滤；若任一元素无效则返回 `None`。
///
/// 与 Node `readCatalogStringList(value)` 1:1 对齐。
pub fn read_catalog_string_list(value: &Value) -> Option<Vec<String>> {
    let arr = value.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let s = as_catalog_string(entry)?;
        out.push(s);
    }
    Some(out)
}

fn is_catalog_record(value: &Value) -> bool {
    value.is_object()
}

/// 顶层入口：解析 `metadata.paperclip.catalog.{...}` 为 Provenance。
///
/// 与 Node `readPortableCatalogProvenance(metadata, canonicalKey)` 1:1 对齐。
///
/// ## 行为
///
/// - `metadata` 为 null / 非对象 → 返回 `None`
/// - `metadata.paperclip` 不是对象 → `paperclip = null`
/// - `paperclip.catalog` 不是对象 → 返回 `None`
/// - 其它情况：返回 `{ sourceRef, metadata }`，其中：
///   - `sourceRef` = `catalog.sourceRef` 非空 OR `catalog.originHash` 非空
///   - `metadata` 包含 `sourceKind = "catalog"` + 可选 `skillKey`（取自 `canonicalKey` 或 `catalog.skillKey`）+ 所有非空 string 字段
///   - 若 `sourceRef` 存在且 `originHash` 不存在，则 `originHash = sourceRef`
///   - `auditCodes`（如果存在）从 string[] 转换为归一化数组
pub fn read_portable_catalog_provenance(
    metadata: &Value,
    canonical_key: Option<&str>,
) -> Option<Provenance> {
    let paperclip = metadata
        .as_object()
        .and_then(|m| m.get("paperclip"))
        .filter(|v| is_catalog_record(v));
    let catalog = paperclip
        .and_then(|p| p.as_object())
        .and_then(|p| p.get("catalog"))
        .filter(|v| is_catalog_record(v));

    let catalog = catalog?;

    let source_ref = as_catalog_string(catalog.get("sourceRef").unwrap_or(&Value::Null))
        .or_else(|| as_catalog_string(catalog.get("originHash").unwrap_or(&Value::Null)));

    let mut normalized = ProvenanceMetadata::new();
    if let Some(ck) = canonical_key {
        normalized.insert("skillKey".to_string(), Value::String(ck.to_string()));
    }
    normalized.insert(
        "sourceKind".to_string(),
        Value::String(SOURCE_KIND.to_string()),
    );

    // 缺 canonicalKey 但 catalog 有 skillKey → 把 catalogSkillKey 提升到顶层
    let catalog_skill_key = as_catalog_string(catalog.get("skillKey").unwrap_or(&Value::Null));
    if canonical_key.is_none() {
        if let Some(ref csk) = catalog_skill_key {
            normalized.insert("skillKey".to_string(), Value::String(csk.clone()));
        }
    }

    for key in PORTABLE_CATALOG_PROVENANCE_STRING_KEYS {
        if *key == "sourceRef" {
            continue;
        }
        if let Some(s) = as_catalog_string(catalog.get(key).unwrap_or(&Value::Null)) {
            normalized.insert((*key).to_string(), Value::String(s));
        }
    }

    if let Some(ref sr) = source_ref {
        if !normalized.contains_key("originHash") {
            normalized.insert(
                "originHash".to_string(),
                Value::String(sr.clone()),
            );
        }
    }

    if let Some(codes_value) = catalog.get("auditCodes") {
        if let Some(audit_codes) = read_catalog_string_list(&codes_value.clone()) {
            normalized.insert(
                "auditCodes".to_string(),
                Value::Array(audit_codes.into_iter().map(Value::String).collect()),
            );
        }
    }

    Some(Provenance {
        source_ref,
        metadata: normalized,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog_obj(pairs: &[(&str, &str)]) -> Value {
        let mut catalog = serde_json::Map::new();
        for (k, v) in pairs {
            catalog.insert((*k).to_string(), Value::String((*v).to_string()));
        }
        json!({ "paperclip": { "catalog": catalog } })
    }

    #[test]
    fn r683_constants_match_node_order() {
        assert_eq!(PORTABLE_CATALOG_PROVENANCE_STRING_KEYS[0], "sourceRef");
        assert_eq!(PORTABLE_CATALOG_PROVENANCE_STRING_KEYS[1], "originHash");
        assert!(PORTABLE_CATALOG_PROVENANCE_STRING_KEYS.contains(&"auditScanVersion"));
    }

    #[test]
    fn r683_as_catalog_string_trims_and_filters_empty() {
        assert_eq!(as_catalog_string(&json!("hello")), Some("hello".into()));
        assert_eq!(as_catalog_string(&json!("  hello  ")), Some("hello".into()));
        assert_eq!(as_catalog_string(&json!("")), None);
        assert_eq!(as_catalog_string(&json!("   ")), None);
        assert_eq!(as_catalog_string(&json!(null)), None);
        assert_eq!(as_catalog_string(&json!(123)), None);
        assert_eq!(as_catalog_string(&json!(["a"])), None);
    }

    #[test]
    fn r683_read_catalog_string_list_validates_every_entry() {
        let v = json!(["a", "  b  ", ""]);
        // 有一个空字符串 → None
        assert_eq!(read_catalog_string_list(&v), None);

        let v = json!(["a", "  b  "]);
        assert_eq!(read_catalog_string_list(&v), Some(vec!["a".into(), "b".into()]));

        assert_eq!(read_catalog_string_list(&json!("not array")), None);
    }

    #[test]
    fn r683_provenance_returns_none_for_null_metadata() {
        assert!(read_portable_catalog_provenance(&json!(null), None).is_none());
    }

    #[test]
    fn r683_provenance_returns_none_for_non_object_metadata() {
        assert!(read_portable_catalog_provenance(&json!("string"), None).is_none());
        assert!(read_portable_catalog_provenance(&json!(123), None).is_none());
    }

    #[test]
    fn r683_provenance_returns_none_when_paperclip_missing() {
        let meta = json!({ "other": "value" });
        assert!(read_portable_catalog_provenance(&meta, None).is_none());
    }

    #[test]
    fn r683_provenance_returns_none_when_catalog_missing() {
        let meta = json!({ "paperclip": { "other": "x" } });
        assert!(read_portable_catalog_provenance(&meta, None).is_none());
    }

    #[test]
    fn r683_provenance_extracts_canonical_skill_key() {
        let meta = catalog_obj(&[("sourceRef", "abc123"), ("catalogId", "cat-1")]);
        let p = read_portable_catalog_provenance(&meta, Some("my.skill")).expect("provenance");
        assert_eq!(p.source_ref, Some("abc123".into()));
        assert_eq!(p.metadata.get("skillKey"), Some(&json!("my.skill")));
        assert_eq!(p.metadata.get("sourceKind"), Some(&json!("catalog")));
        assert_eq!(p.metadata.get("catalogId"), Some(&json!("cat-1")));
        // 无 originHash（因为 sourceRef 提供了但 metadata 没 originHash 字段）→ 应回填
        assert_eq!(p.metadata.get("originHash"), Some(&json!("abc123")));
    }

    #[test]
    fn r683_provenance_uses_catalog_skill_key_when_no_canonical() {
        let meta = catalog_obj(&[("skillKey", "from.catalog"), ("sourceRef", "abc")]);
        let p = read_portable_catalog_provenance(&meta, None).expect("provenance");
        assert_eq!(p.metadata.get("skillKey"), Some(&json!("from.catalog")));
    }

    #[test]
    fn r683_provenance_canonical_key_wins_over_catalog_skill_key() {
        let meta = catalog_obj(&[("skillKey", "from.catalog"), ("sourceRef", "abc")]);
        let p = read_portable_catalog_provenance(&meta, Some("canonical")).expect("provenance");
        assert_eq!(p.metadata.get("skillKey"), Some(&json!("canonical")));
    }

    #[test]
    fn r683_provenance_origin_hash_fallback() {
        let meta = catalog_obj(&[("sourceRef", "src-1")]);
        let p = read_portable_catalog_provenance(&meta, None).expect("provenance");
        assert_eq!(p.source_ref, Some("src-1".into()));
        // No explicit originHash → fallback to sourceRef
        assert_eq!(p.metadata.get("originHash"), Some(&json!("src-1")));
    }

    #[test]
    fn r683_provenance_explicit_origin_hash_kept() {
        let meta = catalog_obj(&[("sourceRef", "src-1"), ("originHash", "orig-hash")]);
        let p = read_portable_catalog_provenance(&meta, None).expect("provenance");
        // originHash 已存在；即使 sourceRef 也有，也不会覆盖
        assert_eq!(p.metadata.get("originHash"), Some(&json!("orig-hash")));
    }

    #[test]
    fn r683_provenance_source_ref_from_origin_hash_when_source_ref_blank() {
        let meta = catalog_obj(&[("originHash", "orig-1")]);
        let p = read_portable_catalog_provenance(&meta, None).expect("provenance");
        // sourceRef 不存在但 originHash 存在 → sourceRef = originHash
        assert_eq!(p.source_ref, Some("orig-1".into()));
        assert_eq!(p.metadata.get("originHash"), Some(&json!("orig-1")));
    }

    #[test]
    fn r683_provenance_ignores_blank_string_fields() {
        let mut catalog = serde_json::Map::new();
        catalog.insert("sourceRef".to_string(), Value::String("   ".to_string()));
        catalog.insert("catalogId".to_string(), Value::String("cat-1".to_string()));
        catalog.insert("packageName".to_string(), Value::String("".to_string()));
        let meta = json!({ "paperclip": { "catalog": catalog } });
        let p = read_portable_catalog_provenance(&meta, None).expect("provenance");
        // sourceRef 全空 → 应取 null（fallback）
        assert_eq!(p.source_ref, None);
        // catalogId 保留
        assert_eq!(p.metadata.get("catalogId"), Some(&json!("cat-1")));
        // 空 packageName 不写入
        assert!(!p.metadata.contains_key("packageName"));
    }

    #[test]
    fn r683_provenance_audit_codes_passthrough() {
        let mut catalog = serde_json::Map::new();
        catalog.insert("sourceRef".to_string(), Value::String("abc".to_string()));
        catalog.insert("auditCodes".to_string(), json!(["a", "b", "c"]));
        let meta = json!({ "paperclip": { "catalog": catalog } });
        let p = read_portable_catalog_provenance(&meta, None).expect("provenance");
        assert_eq!(
            p.metadata.get("auditCodes"),
            Some(&json!(["a", "b", "c"]))
        );
    }

    #[test]
    fn r683_provenance_audit_codes_dropped_if_not_string_array() {
        let mut catalog = serde_json::Map::new();
        catalog.insert("sourceRef".to_string(), Value::String("abc".to_string()));
        catalog.insert("auditCodes".to_string(), json!([1, 2, 3]));
        let meta = json!({ "paperclip": { "catalog": catalog } });
        let p = read_portable_catalog_provenance(&meta, None).expect("provenance");
        // Non-string array → dropped
        assert!(!p.metadata.contains_key("auditCodes"));
    }

    #[test]
    fn r683_provenance_audit_codes_dropped_if_any_invalid() {
        let mut catalog = serde_json::Map::new();
        catalog.insert("sourceRef".to_string(), Value::String("abc".to_string()));
        catalog.insert("auditCodes".to_string(), json!(["a", "", "c"]));
        let meta = json!({ "paperclip": { "catalog": catalog } });
        let p = read_portable_catalog_provenance(&meta, None).expect("provenance");
        // 有一个空字符串 → drop 整个 auditCodes
        assert!(!p.metadata.contains_key("auditCodes"));
    }

    #[test]
    fn r683_provenance_skips_source_ref_in_string_keys_iteration() {
        // 关键是: sourceRef 不应在 normalized 中重复（仅当 originHash 缺失时回填）
        let meta = catalog_obj(&[("sourceRef", "src-1")]);
        let p = read_portable_catalog_provenance(&meta, None).expect("provenance");
        // normalized.metadata 中不应有 sourceRef 字段
        assert!(!p.metadata.contains_key("sourceRef"));
        // 但 originHash 应有（来自 sourceRef 回填）
        assert!(p.metadata.contains_key("originHash"));
    }

    #[test]
    fn r683_provenance_metadata_object_required() {
        // array 必须是 object
        let meta = json!({ "paperclip": ["not", "an", "object"] });
        assert!(read_portable_catalog_provenance(&meta, None).is_none());
    }

    #[test]
    fn r683_provenance_json_roundtrip() {
        let meta = catalog_obj(&[
            ("sourceRef", "abc"),
            ("originHash", "orig"),
            ("catalogId", "cat-1"),
            ("catalogKind", "official"),
        ]);
        let p = read_portable_catalog_provenance(&meta, Some("my.skill")).expect("provenance");
        let s = serde_json::to_string(&p).expect("serialize");
        let v: Value = serde_json::from_str(&s).expect("parse");
        assert_eq!(v["sourceRef"], json!("abc"));
        assert_eq!(v["metadata"]["skillKey"], json!("my.skill"));
        assert_eq!(v["metadata"]["sourceKind"], json!("catalog"));
        assert_eq!(v["metadata"]["catalogId"], json!("cat-1"));
    }

    #[test]
    fn r683_full_field_set_extraction() {
        let meta = catalog_obj(&[
            ("sourceRef", "abc"),
            ("originHash", "orig"),
            ("catalogId", "cat-1"),
            ("catalogKey", "key-1"),
            ("catalogKind", "kind-1"),
            ("catalogCategory", "cat-a"),
            ("catalogPath", "/path"),
            ("packageName", "pkg"),
            ("packageVersion", "1.0.0"),
            ("originVersion", "1.0.0"),
            ("installedHash", "inst"),
            ("userModifiedAt", "2026-01-01"),
            ("updateHoldReason", "none"),
            ("auditVerdict", "ok"),
            ("auditScannedAt", "2026-01-02"),
            ("auditScanVersion", "v1"),
        ]);
        let p = read_portable_catalog_provenance(&meta, Some("my.skill")).expect("provenance");
        // sourceRef 写入 p.source_ref
        assert_eq!(p.source_ref, Some("abc".into()));
        // 其余字段应该都在 normalized.metadata 中
        for key in [
            "catalogId",
            "catalogKey",
            "catalogKind",
            "catalogCategory",
            "catalogPath",
            "packageName",
            "packageVersion",
            "originVersion",
            "installedHash",
            "userModifiedAt",
            "updateHoldReason",
            "auditVerdict",
            "auditScannedAt",
            "auditScanVersion",
        ] {
            assert!(
                p.metadata.contains_key(key),
                "missing {key} in metadata: {:#?}",
                p.metadata
            );
        }
        // originHash 已有 → sourceRef 回填不会改写
        assert_eq!(p.metadata.get("originHash"), Some(&json!("orig")));
    }
}
