//! Unit tests for `pc_environment::custom_image_runtime`.
//!
//! Verifies Node `environment-custom-image-runtime.ts` 1:1 parity for the
//! pure subset (9 functions + 4 constants + 4 types).

use pc_environment::{
    apply_custom_image_template_to_sandbox_config, classify_environment_custom_image_config_change,
    default_environment_custom_image_runtime_config_binding,
    environment_custom_image_template_from_row, environment_custom_image_template_matches_base_config,
    fingerprint_environment_sandbox_provider_config,
    normalize_environment_custom_image_runtime_config_binding,
    read_environment_custom_image_template_kind,
    resolve_environment_custom_image_runtime_config_binding, stable_stringify,
    ClassifyConfigChangeInput, EnvironmentCustomImageConfigChangeKind,
    EnvironmentCustomImageRuntimeConfigBinding, EnvironmentCustomImageTemplate,
    EnvironmentCustomImageTemplateKind, EnvironmentCustomImageTemplateRow, MatchBaseConfigInput,
    ResolveBindingInput, TemplateBindingInput,
    ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS,
    ENVIRONMENT_CUSTOM_IMAGE_RUNTIME_CONFIG_BINDING_METADATA_KEY,
    ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_KINDS, ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_SOURCE_FIELDS,
};
use serde_json::{json, Value};

// =======================================================================
// Constants
// =======================================================================

#[test]
fn r677_constants_metadata_key() {
    assert_eq!(ENVIRONMENT_CUSTOM_IMAGE_RUNTIME_CONFIG_BINDING_METADATA_KEY, "runtimeConfigBinding");
}

#[test]
fn r677_constants_excluded_paths_count_and_contents() {
    assert_eq!(ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS.len(), 11);
    let expected: Vec<&str> = vec![
        "timeoutMs", "reuseLease", "streamRunLogs", "archiveOnRelease",
        "cpu", "memory", "disk", "gpu",
        "autoStopInterval", "autoArchiveInterval", "autoDeleteInterval",
    ];
    for e in expected {
        assert!(
            ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS.contains(&e),
            "missing {e}"
        );
    }
}

#[test]
fn r677_constants_source_fields() {
    assert_eq!(ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_SOURCE_FIELDS, &["snapshot", "image", "template"]);
}

#[test]
fn r677_constants_template_kinds() {
    assert_eq!(
        ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_KINDS,
        &["snapshot", "image", "provider_template", "unknown"]
    );
}

// =======================================================================
// read_kind / default / normalize / resolve
// =======================================================================

#[test]
fn r677_read_kind_known() {
    assert_eq!(read_environment_custom_image_template_kind(Some("snapshot")), EnvironmentCustomImageTemplateKind::Snapshot);
    assert_eq!(read_environment_custom_image_template_kind(Some("image")), EnvironmentCustomImageTemplateKind::Image);
    assert_eq!(read_environment_custom_image_template_kind(Some("provider_template")), EnvironmentCustomImageTemplateKind::ProviderTemplate);
}

#[test]
fn r677_read_kind_unknown_or_null() {
    assert_eq!(read_environment_custom_image_template_kind(Some("unknown")), EnvironmentCustomImageTemplateKind::Unknown);
    assert_eq!(read_environment_custom_image_template_kind(Some("garbage")), EnvironmentCustomImageTemplateKind::Unknown);
    assert_eq!(read_environment_custom_image_template_kind(None), EnvironmentCustomImageTemplateKind::Unknown);
}

#[test]
fn r677_default_binding_snapshot() {
    let b = default_environment_custom_image_runtime_config_binding(Some("snapshot"));
    assert_eq!(b.field, "snapshot");
    assert_eq!(b.unset_fields, vec!["image".to_string()]);
}

#[test]
fn r677_default_binding_image() {
    let b = default_environment_custom_image_runtime_config_binding(Some("image"));
    assert_eq!(b.field, "image");
    assert_eq!(b.unset_fields, vec!["snapshot".to_string()]);
}

#[test]
fn r677_default_binding_provider_template() {
    let b = default_environment_custom_image_runtime_config_binding(Some("provider_template"));
    assert_eq!(b.field, "template");
    assert!(b.unset_fields.is_empty());
}

#[test]
fn r677_default_binding_unknown_kind() {
    let b = default_environment_custom_image_runtime_config_binding(Some("bogus"));
    assert_eq!(b.field, "templateRef");
    assert!(b.unset_fields.is_empty());
}

#[test]
fn r677_default_binding_null_kind() {
    let b = default_environment_custom_image_runtime_config_binding(None);
    assert_eq!(b.field, "templateRef");
}

#[test]
fn r677_normalize_binding_minimal() {
    let v = json!({"field": "snapshot"});
    let b = normalize_environment_custom_image_runtime_config_binding(&v).expect("ok");
    assert_eq!(b.field, "snapshot");
    assert!(b.unset_fields.is_empty());
}

#[test]
fn r677_normalize_binding_with_unset() {
    let v = json!({"field": "snapshot", "unsetFields": ["image", "extra", "snapshot"]});
    let b = normalize_environment_custom_image_runtime_config_binding(&v).expect("ok");
    assert_eq!(b.field, "snapshot");
    // "snapshot" excluded (== field); "image" + "extra" kept in insertion order; dedup.
    assert_eq!(b.unset_fields, vec!["image".to_string(), "extra".to_string()]);
}

#[test]
fn r677_normalize_binding_rejects_invalid_field() {
    let v = json!({"field": "1bad"});
    assert!(normalize_environment_custom_image_runtime_config_binding(&v).is_none());
    let v = json!({"field": "provider"});
    assert!(normalize_environment_custom_image_runtime_config_binding(&v).is_none());
    let v = json!({"field": ""});
    assert!(normalize_environment_custom_image_runtime_config_binding(&v).is_none());
}

#[test]
fn r677_normalize_binding_rejects_non_object() {
    let v = json!(null);
    assert!(normalize_environment_custom_image_runtime_config_binding(&v).is_none());
    let v = json!("string");
    assert!(normalize_environment_custom_image_runtime_config_binding(&v).is_none());
    let v = json!([1, 2, 3]);
    assert!(normalize_environment_custom_image_runtime_config_binding(&v).is_none());
}

#[test]
fn r677_resolve_prefers_metadata_over_default() {
    let metadata = json!({
        ENVIRONMENT_CUSTOM_IMAGE_RUNTIME_CONFIG_BINDING_METADATA_KEY: {"field": "image", "unsetFields": ["snapshot"]}
    });
    let b = resolve_environment_custom_image_runtime_config_binding(ResolveBindingInput {
        template_kind: Some("snapshot".into()),
        metadata: Some(metadata),
    });
    assert_eq!(b.field, "image");
    assert_eq!(b.unset_fields, vec!["snapshot".to_string()]);
}

#[test]
fn r677_resolve_falls_back_to_default() {
    let b = resolve_environment_custom_image_runtime_config_binding(ResolveBindingInput {
        template_kind: Some("snapshot".into()),
        metadata: Some(json!({"other": "stuff"})),
    });
    assert_eq!(b.field, "snapshot");
    assert_eq!(b.unset_fields, vec!["image".to_string()]);
}

#[test]
fn r677_resolve_handles_null_metadata() {
    let b = resolve_environment_custom_image_runtime_config_binding(ResolveBindingInput {
        template_kind: Some("provider_template".into()),
        metadata: None,
    });
    assert_eq!(b.field, "template");
}

// =======================================================================
// stable_stringify
// =======================================================================

#[test]
fn r677_stable_stringify_keys_sorted() {
    let a = json!({"b": 1, "a": 2});
    let b = json!({"a": 2, "b": 1});
    assert_eq!(stable_stringify(&a), stable_stringify(&b));
}

#[test]
fn r677_stable_stringify_nested() {
    let a = json!({"z": {"y": 1, "x": 2}, "a": [3, 2, 1]});
    let b = json!({"a": [3, 2, 1], "z": {"x": 2, "y": 1}});
    assert_eq!(stable_stringify(&a), stable_stringify(&b));
}

// =======================================================================
// fingerprint
// =======================================================================

#[test]
fn r677_fingerprint_stable_across_key_order() {
    let cfg = json!({"provider":"docker","image":"alpine","timeoutMs":1000,"reuseLease":true});
    let cfg2 = json!({"reuseLease":true,"timeoutMs":1000,"image":"alpine","provider":"docker"});
    let exclude: Vec<&str> = ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS.to_vec();
    let fp1 = fingerprint_environment_sandbox_provider_config(&cfg, Some(&exclude));
    let fp2 = fingerprint_environment_sandbox_provider_config(&cfg2, Some(&exclude));
    assert_eq!(fp1, fp2);
}

#[test]
fn r677_fingerprint_differs_when_excluded_field_differs() {
    let cfg = json!({"provider":"docker","image":"alpine","timeoutMs":1000});
    let cfg2 = json!({"provider":"docker","image":"alpine","timeoutMs":99999});
    // timeoutMs is in excluded paths so fingerprints must match.
    let exclude: Vec<&str> = ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS.to_vec();
    let fp1 = fingerprint_environment_sandbox_provider_config(&cfg, Some(&exclude));
    let fp2 = fingerprint_environment_sandbox_provider_config(&cfg2, Some(&exclude));
    assert_eq!(fp1, fp2);
}

#[test]
fn r677_fingerprint_differs_when_non_excluded_field_differs() {
    let cfg = json!({"provider":"docker","image":"alpine"});
    let cfg2 = json!({"provider":"docker","image":"busybox"});
    let exclude: Vec<&str> = ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS.to_vec();
    let fp1 = fingerprint_environment_sandbox_provider_config(&cfg, Some(&exclude));
    let fp2 = fingerprint_environment_sandbox_provider_config(&cfg2, Some(&exclude));
    assert_ne!(fp1, fp2);
}

#[test]
fn r677_fingerprint_no_excludes() {
    let cfg = json!({"image":"alpine"});
    let fp = fingerprint_environment_sandbox_provider_config(&cfg, None);
    assert_eq!(fp.len(), 64); // SHA-256 hex
}

// =======================================================================
// apply_template
// =======================================================================

#[test]
fn r677_apply_snapshot_template_writes_field_and_unsets_image() {
    let cfg = json!({"provider":"docker","image":"alpine"});
    let template = TemplateBindingInput {
        template_kind: EnvironmentCustomImageTemplateKind::Snapshot,
        template_ref: Some("snap-abc".into()),
        metadata: None,
    };
    let next = apply_custom_image_template_to_sandbox_config(&cfg, &template);
    assert_eq!(next["snapshot"], "snap-abc");
    assert!(next.get("image").is_none(), "image should be removed");
    assert_eq!(next["provider"], "docker");
}

#[test]
fn r677_apply_image_template_writes_image_unsets_snapshot() {
    let cfg = json!({"provider":"docker","snapshot":"snap-x"});
    let template = TemplateBindingInput {
        template_kind: EnvironmentCustomImageTemplateKind::Image,
        template_ref: Some("img-123".into()),
        metadata: None,
    };
    let next = apply_custom_image_template_to_sandbox_config(&cfg, &template);
    assert_eq!(next["image"], "img-123");
    assert!(next.get("snapshot").is_none());
}

#[test]
fn r677_apply_provider_template_writes_template_no_unset() {
    let cfg = json!({"provider":"docker"});
    let template = TemplateBindingInput {
        template_kind: EnvironmentCustomImageTemplateKind::ProviderTemplate,
        template_ref: Some("tpl-99".into()),
        metadata: None,
    };
    let next = apply_custom_image_template_to_sandbox_config(&cfg, &template);
    assert_eq!(next["template"], "tpl-99");
}

#[test]
fn r677_apply_no_template_ref_returns_config_unchanged() {
    let cfg = json!({"provider":"docker","image":"alpine"});
    let template = TemplateBindingInput {
        template_kind: EnvironmentCustomImageTemplateKind::Snapshot,
        template_ref: None,
        metadata: None,
    };
    let next = apply_custom_image_template_to_sandbox_config(&cfg, &template);
    assert_eq!(next, cfg);
}

#[test]
fn r677_apply_respects_metadata_binding_override() {
    let cfg = json!({"provider":"docker","image":"alpine"});
    let metadata = json!({
        ENVIRONMENT_CUSTOM_IMAGE_RUNTIME_CONFIG_BINDING_METADATA_KEY: {"field":"image","unsetFields":["snapshot"]}
    });
    let template = TemplateBindingInput {
        template_kind: EnvironmentCustomImageTemplateKind::Snapshot, // would default to field=snapshot
        template_ref: Some("override-1".into()),
        metadata: Some(metadata),
    };
    let next = apply_custom_image_template_to_sandbox_config(&cfg, &template);
    assert_eq!(next["image"], "override-1");
}

// =======================================================================
// matches_base_config
// =======================================================================

fn template_with_fingerprint(fp: &str) -> EnvironmentCustomImageTemplate {
    EnvironmentCustomImageTemplate {
        id: "tpl-1".into(),
        environment_id: "env-1".into(),
        provider: "docker".into(),
        template_kind: EnvironmentCustomImageTemplateKind::Snapshot,
        template_ref: Some("snap".into()),
        source_template_ref: None,
        source_environment_config_fingerprint: Some(fp.into()),
        status: "active".into(),
        created_by_user_id: None,
        created_by_agent_id: None,
        captured_at: None,
        last_used_at: None,
        superseded_by_template_id: None,
        metadata: None,
        created_at: "2026-08-16T00:00:00Z".into(),
        updated_at: "2026-08-16T00:00:00Z".into(),
    }
}

#[test]
fn r677_matches_missing_fingerprint_returns_true() {
    let mut t = template_with_fingerprint("ignored");
    t.source_environment_config_fingerprint = None;
    let input = MatchBaseConfigInput {
        template: t,
        base_config: json!({"provider":"docker"}),
        secret_ref_exclude_paths: vec![],
    };
    assert!(environment_custom_image_template_matches_base_config(&input));
}

#[test]
fn r677_matches_excluded_paths_dont_break_match() {
    let cfg = json!({"provider":"docker","image":"alpine","timeoutMs":1});
    let exclude: Vec<&str> = ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS.to_vec();
    let fp = fingerprint_environment_sandbox_provider_config(&cfg, Some(&exclude));
    let t = template_with_fingerprint(&fp);
    // Re-fingerprint with a different timeoutMs value must still match.
    let cfg_diff = json!({"provider":"docker","image":"alpine","timeoutMs":999});
    let input = MatchBaseConfigInput {
        template: t,
        base_config: cfg_diff,
        secret_ref_exclude_paths: vec![],
    };
    assert!(environment_custom_image_template_matches_base_config(&input));
}

#[test]
fn r677_matches_secret_ref_excludes() {
    let cfg = json!({"provider":"docker","image":"alpine"});
    let fp = fingerprint_environment_sandbox_provider_config(&cfg, None);
    let t = template_with_fingerprint(&fp);
    // Add a secret-ref path that is excluded — fingerprint must match.
    let cfg_with_secret = json!({"provider":"docker","image":"alpine","auth":"uuid-secret"});
    let input = MatchBaseConfigInput {
        template: t,
        base_config: cfg_with_secret,
        secret_ref_exclude_paths: vec!["auth".to_string()],
    };
    assert!(environment_custom_image_template_matches_base_config(&input));
}

#[test]
fn r677_matches_real_field_change_breaks() {
    let cfg = json!({"provider":"docker","image":"alpine"});
    let fp = fingerprint_environment_sandbox_provider_config(&cfg, None);
    let t = template_with_fingerprint(&fp);
    let cfg_diff = json!({"provider":"docker","image":"busybox"});
    let input = MatchBaseConfigInput {
        template: t,
        base_config: cfg_diff,
        secret_ref_exclude_paths: vec![],
    };
    assert!(!environment_custom_image_template_matches_base_config(&input));
}

// =======================================================================
// classify_config_change
// =======================================================================

#[test]
fn r677_classify_none_when_template_already_detached() {
    let cfg = json!({"provider":"docker","image":"alpine"});
    let fp = fingerprint_environment_sandbox_provider_config(&cfg, None);
    let t = template_with_fingerprint(&fp);
    // previous doesn't match → "none"
    let prev = json!({"provider":"docker","image":"different"});
    let next = json!({"provider":"docker","image":"alpine"});
    let result = classify_environment_custom_image_config_change(&ClassifyConfigChangeInput {
        template: t,
        previous_config: prev,
        next_config: next,
        secret_ref_exclude_paths: vec![],
        template_identity_paths: vec![],
    });
    assert_eq!(result, EnvironmentCustomImageConfigChangeKind::None);
}

#[test]
fn r677_classify_none_when_next_also_matches() {
    let cfg = json!({"provider":"docker","image":"alpine"});
    let fp = fingerprint_environment_sandbox_provider_config(&cfg, None);
    let t = template_with_fingerprint(&fp);
    let result = classify_environment_custom_image_config_change(&ClassifyConfigChangeInput {
        template: t,
        previous_config: cfg.clone(),
        next_config: cfg,
        secret_ref_exclude_paths: vec![],
        template_identity_paths: vec![],
    });
    assert_eq!(result, EnvironmentCustomImageConfigChangeKind::None);
}

#[test]
fn r677_classify_breaking_when_provider_changes() {
    let cfg = json!({"provider":"docker","image":"alpine"});
    let fp = fingerprint_environment_sandbox_provider_config(&cfg, None);
    let t = template_with_fingerprint(&fp);
    let prev = cfg.clone();
    let mut next = cfg.clone();
    next["provider"] = json!("fly");
    let result = classify_environment_custom_image_config_change(&ClassifyConfigChangeInput {
        template: t,
        previous_config: prev,
        next_config: next,
        secret_ref_exclude_paths: vec![],
        template_identity_paths: vec![],
    });
    assert_eq!(result, EnvironmentCustomImageConfigChangeKind::Breaking);
}

#[test]
fn r677_classify_breaking_when_binding_field_changes() {
    let cfg = json!({"provider":"docker","snapshot":"snap-1"});
    let fp = fingerprint_environment_sandbox_provider_config(&cfg, None);
    let mut t = template_with_fingerprint(&fp);
    t.template_kind = EnvironmentCustomImageTemplateKind::Snapshot;
    let prev = cfg.clone();
    let mut next = cfg;
    next["snapshot"] = json!("snap-2");
    let result = classify_environment_custom_image_config_change(&ClassifyConfigChangeInput {
        template: t,
        previous_config: prev,
        next_config: next,
        secret_ref_exclude_paths: vec![],
        template_identity_paths: vec![],
    });
    assert_eq!(result, EnvironmentCustomImageConfigChangeKind::Breaking);
}

#[test]
fn r677_classify_relinkable_for_non_breaking_field() {
    let cfg = json!({"provider":"docker","image":"alpine","region":"us-east"});
    let fp = fingerprint_environment_sandbox_provider_config(&cfg, None);
    let t = template_with_fingerprint(&fp);
    let prev = cfg.clone();
    let mut next = cfg;
    next["region"] = json!("eu-west");
    let result = classify_environment_custom_image_config_change(&ClassifyConfigChangeInput {
        template: t,
        previous_config: prev,
        next_config: next,
        secret_ref_exclude_paths: vec![],
        template_identity_paths: vec![],
    });
    assert_eq!(result, EnvironmentCustomImageConfigChangeKind::Relinkable);
}

#[test]
fn r677_classify_template_identity_paths_force_breaking() {
    let cfg = json!({"provider":"docker","image":"alpine","identityX":"v1"});
    let fp = fingerprint_environment_sandbox_provider_config(&cfg, None);
    let t = template_with_fingerprint(&fp);
    let prev = cfg.clone();
    let mut next = cfg;
    next["identityX"] = json!("v2");
    let result = classify_environment_custom_image_config_change(&ClassifyConfigChangeInput {
        template: t,
        previous_config: prev,
        next_config: next,
        secret_ref_exclude_paths: vec![],
        template_identity_paths: vec!["identityX".to_string()],
    });
    assert_eq!(result, EnvironmentCustomImageConfigChangeKind::Breaking);
}

// =======================================================================
// row mapper
// =======================================================================

#[test]
fn r677_row_mapper_basic() {
    let row = EnvironmentCustomImageTemplateRow {
        id: "tpl-1".into(),
        environment_id: "env-1".into(),
        provider: "docker".into(),
        template_kind: "image".into(),
        template_ref: Some("img-1".into()),
        source_template_ref: Some("src-1".into()),
        source_environment_config_fingerprint: Some("fp-1".into()),
        status: "active".into(),
        created_by_user_id: Some("user-1".into()),
        created_by_agent_id: None,
        captured_at: Some("2026-08-16T00:00:00Z".into()),
        last_used_at: None,
        superseded_by_template_id: None,
        metadata: Some(json!({"foo":"bar"})),
        created_at: "2026-08-16T00:00:00Z".into(),
        updated_at: "2026-08-16T00:00:00Z".into(),
    };
    let mapped = environment_custom_image_template_from_row(&row);
    assert_eq!(mapped.id, "tpl-1");
    assert_eq!(mapped.template_kind, EnvironmentCustomImageTemplateKind::Image);
    assert_eq!(mapped.template_ref.as_deref(), Some("img-1"));
    assert_eq!(mapped.created_by_user_id.as_deref(), Some("user-1"));
    assert_eq!(mapped.metadata.as_ref().unwrap()["foo"], "bar");
}

#[test]
fn r677_row_mapper_normalizes_unknown_kind() {
    let row = EnvironmentCustomImageTemplateRow {
        template_kind: "totally_unknown".into(),
        ..EnvironmentCustomImageTemplateRow::default()
    };
    let mapped = environment_custom_image_template_from_row(&row);
    assert_eq!(mapped.template_kind, EnvironmentCustomImageTemplateKind::Unknown);
}
