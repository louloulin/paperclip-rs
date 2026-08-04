//! 可移植技能目录来源元数据归一化。
//!
//! 对齐 Node `services/catalog-provenance.ts`，仅保留允许跨实例导入导出的目录字段。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogProvenance {
    pub source_ref: Option<String>,
    pub metadata: Map<String, Value>,
}

pub fn read_catalog_string_list(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|entry| as_catalog_string(Some(entry)))
        .collect()
}

pub fn read_portable_catalog_provenance(
    metadata: Option<&Map<String, Value>>,
    canonical_key: Option<&str>,
) -> Option<CatalogProvenance> {
    let paperclip_value = metadata?.get("paperclip");
    if !is_catalog_record(paperclip_value) {
        return None;
    }
    let paperclip = paperclip_value?.as_object()?;

    let catalog_value = paperclip.get("catalog");
    if !is_catalog_record(catalog_value) {
        return None;
    }
    let catalog = catalog_value?.as_object()?;

    let source_ref = as_catalog_string(catalog.get("sourceRef"))
        .or_else(|| as_catalog_string(catalog.get("originHash")));
    let canonical_key = canonical_key.filter(|key| !key.is_empty());
    let mut normalized = Map::new();

    if let Some(canonical_key) = canonical_key {
        normalized.insert(
            "skillKey".to_string(),
            Value::String(canonical_key.to_string()),
        );
    }
    normalized.insert(
        "sourceKind".to_string(),
        Value::String("catalog".to_string()),
    );

    if canonical_key.is_none() {
        if let Some(skill_key) = as_catalog_string(catalog.get("skillKey")) {
            normalized.insert("skillKey".to_string(), Value::String(skill_key));
        }
    }

    for key in PORTABLE_CATALOG_PROVENANCE_STRING_KEYS {
        if *key == "sourceRef" {
            continue;
        }
        if let Some(value) = as_catalog_string(catalog.get(*key)) {
            normalized.insert((*key).to_string(), Value::String(value));
        }
    }

    if !normalized.contains_key("originHash") {
        if let Some(source_ref) = &source_ref {
            normalized.insert("originHash".to_string(), Value::String(source_ref.clone()));
        }
    }

    if let Some(audit_codes) = catalog.get("auditCodes").and_then(read_catalog_string_list) {
        normalized.insert(
            "auditCodes".to_string(),
            Value::Array(audit_codes.into_iter().map(Value::String).collect()),
        );
    }

    Some(CatalogProvenance {
        source_ref,
        metadata: normalized,
    })
}

fn as_catalog_string(value: Option<&Value>) -> Option<String> {
    let trimmed = value?.as_str()?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn is_catalog_record(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Object(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata(value: Value) -> Map<String, Value> {
        value
            .as_object()
            .cloned()
            .expect("test fixture is an object")
    }

    #[test]
    fn portable_string_keys_match_node_order() {
        assert_eq!(
            PORTABLE_CATALOG_PROVENANCE_STRING_KEYS,
            [
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
            ]
        );
    }

    #[test]
    fn catalog_string_trims_valid_strings() {
        assert_eq!(
            as_catalog_string(Some(&json!("  sha256:abc  "))),
            Some("sha256:abc".to_string())
        );
    }

    #[test]
    fn catalog_string_rejects_empty_and_non_strings() {
        assert_eq!(as_catalog_string(Some(&json!("   "))), None);
        assert_eq!(as_catalog_string(Some(&json!(42))), None);
        assert_eq!(as_catalog_string(Some(&Value::Null)), None);
        assert_eq!(as_catalog_string(None), None);
    }

    #[test]
    fn catalog_string_list_trims_every_entry() {
        assert_eq!(
            read_catalog_string_list(&json!([" audit_a ", "audit_b"])),
            Some(vec!["audit_a".to_string(), "audit_b".to_string()])
        );
    }

    #[test]
    fn catalog_string_list_accepts_empty_arrays() {
        assert_eq!(read_catalog_string_list(&json!([])), Some(Vec::new()));
    }

    #[test]
    fn catalog_string_list_rejects_non_arrays_and_partial_lists() {
        assert_eq!(read_catalog_string_list(&json!("audit_a")), None);
        assert_eq!(read_catalog_string_list(&json!(["audit_a", 42])), None);
        assert_eq!(read_catalog_string_list(&json!(["audit_a", " "])), None);
    }

    #[test]
    fn catalog_record_only_accepts_json_objects() {
        assert!(is_catalog_record(Some(&json!({}))));
        assert!(!is_catalog_record(Some(&json!([]))));
        assert!(!is_catalog_record(Some(&Value::Null)));
        assert!(!is_catalog_record(Some(&json!("catalog"))));
        assert!(!is_catalog_record(None));
    }

    #[test]
    fn missing_or_invalid_nested_catalog_returns_none() {
        assert_eq!(read_portable_catalog_provenance(None, None), None);
        assert_eq!(
            read_portable_catalog_provenance(Some(&metadata(json!({}))), None),
            None
        );
        assert_eq!(
            read_portable_catalog_provenance(Some(&metadata(json!({ "paperclip": [] }))), None),
            None
        );
        assert_eq!(
            read_portable_catalog_provenance(
                Some(&metadata(json!({ "paperclip": { "catalog": null } }))),
                None
            ),
            None
        );
    }

    #[test]
    fn source_ref_takes_precedence_over_origin_hash() {
        let input = metadata(json!({
            "paperclip": {
                "catalog": {
                    "sourceRef": " sha256:source ",
                    "originHash": " sha256:origin "
                }
            }
        }));

        let provenance = read_portable_catalog_provenance(Some(&input), None).unwrap();

        assert_eq!(provenance.source_ref.as_deref(), Some("sha256:source"));
        assert_eq!(
            provenance.metadata.get("originHash"),
            Some(&json!("sha256:origin"))
        );
    }

    #[test]
    fn origin_hash_is_source_ref_fallback() {
        let input = metadata(json!({
            "paperclip": { "catalog": { "originHash": " sha256:origin " } }
        }));

        let provenance = read_portable_catalog_provenance(Some(&input), None).unwrap();

        assert_eq!(provenance.source_ref.as_deref(), Some("sha256:origin"));
        assert_eq!(
            provenance.metadata.get("originHash"),
            Some(&json!("sha256:origin"))
        );
    }

    #[test]
    fn source_ref_populates_missing_origin_hash() {
        let input = metadata(json!({
            "paperclip": { "catalog": { "sourceRef": "sha256:source" } }
        }));

        let provenance = read_portable_catalog_provenance(Some(&input), None).unwrap();

        assert_eq!(
            provenance.metadata.get("originHash"),
            Some(&json!("sha256:source"))
        );
        assert!(!provenance.metadata.contains_key("sourceRef"));
    }

    #[test]
    fn canonical_key_overrides_catalog_skill_key_without_trimming() {
        let input = metadata(json!({
            "paperclip": { "catalog": { "skillKey": " catalog/key " } }
        }));

        let provenance =
            read_portable_catalog_provenance(Some(&input), Some(" canonical/key ")).unwrap();

        assert_eq!(
            provenance.metadata.get("skillKey"),
            Some(&json!(" canonical/key "))
        );
    }

    #[test]
    fn empty_canonical_key_falls_back_to_trimmed_catalog_key() {
        let input = metadata(json!({
            "paperclip": { "catalog": { "skillKey": " catalog/key " } }
        }));

        let provenance = read_portable_catalog_provenance(Some(&input), Some("")).unwrap();

        assert_eq!(
            provenance.metadata.get("skillKey"),
            Some(&json!("catalog/key"))
        );
    }

    #[test]
    fn normalizes_all_portable_fields_and_drops_unknown_fields() {
        let input = metadata(json!({
            "paperclip": {
                "catalog": {
                    "sourceRef": " sha256:source ",
                    "originHash": " sha256:origin ",
                    "catalogId": " catalog-id ",
                    "catalogKey": " catalog-key ",
                    "catalogKind": " bundled ",
                    "catalogCategory": " development ",
                    "catalogPath": " catalog/bundled/review ",
                    "packageName": " @paperclipai/skills-catalog ",
                    "packageVersion": " 0.3.1 ",
                    "originVersion": " 0.3.0 ",
                    "installedHash": " sha256:installed ",
                    "userModifiedAt": " 2026-05-01T00:00:00.000Z ",
                    "updateHoldReason": " local_modifications ",
                    "auditVerdict": " warning ",
                    "auditScannedAt": " 2026-05-02T00:00:00.000Z ",
                    "auditScanVersion": " skills-audit-v1 ",
                    "auditCodes": [" local_modifications ", " script_trust "],
                    "originSnapshotLocator": "/tmp/local-only-origin"
                }
            }
        }));

        let provenance = read_portable_catalog_provenance(Some(&input), None).unwrap();

        assert_eq!(provenance.source_ref.as_deref(), Some("sha256:source"));
        assert_eq!(provenance.metadata.len(), 17);
        assert_eq!(
            provenance.metadata.get("sourceKind"),
            Some(&json!("catalog"))
        );
        assert_eq!(
            provenance.metadata.get("catalogId"),
            Some(&json!("catalog-id"))
        );
        assert_eq!(
            provenance.metadata.get("auditCodes"),
            Some(&json!(["local_modifications", "script_trust"]))
        );
        assert!(!provenance.metadata.contains_key("sourceRef"));
        assert!(!provenance.metadata.contains_key("originSnapshotLocator"));
    }

    #[test]
    fn invalid_audit_codes_are_omitted_atomically() {
        let input = metadata(json!({
            "paperclip": {
                "catalog": {
                    "auditCodes": ["valid", 42]
                }
            }
        }));

        let provenance = read_portable_catalog_provenance(Some(&input), None).unwrap();

        assert_eq!(
            provenance.metadata,
            metadata(json!({ "sourceKind": "catalog" }))
        );
    }

    #[test]
    fn empty_catalog_still_returns_catalog_source_kind() {
        let input = metadata(json!({ "paperclip": { "catalog": {} } }));

        let provenance = read_portable_catalog_provenance(Some(&input), None).unwrap();

        assert_eq!(provenance.source_ref, None);
        assert_eq!(
            provenance.metadata,
            metadata(json!({ "sourceKind": "catalog" }))
        );
    }

    #[test]
    fn catalog_provenance_serializes_with_node_field_names() {
        let provenance = CatalogProvenance {
            source_ref: Some("sha256:source".to_string()),
            metadata: metadata(json!({ "sourceKind": "catalog" })),
        };

        let value = serde_json::to_value(&provenance).unwrap();

        assert_eq!(
            value,
            json!({
                "sourceRef": "sha256:source",
                "metadata": { "sourceKind": "catalog" }
            })
        );
        assert_eq!(
            serde_json::from_value::<CatalogProvenance>(value).unwrap(),
            provenance
        );
    }
}
