// SPDX-License-Identifier: MIT
//
// R681 parity tests for `environment-custom-images.ts` pure helpers.

use pc_environment::environment_custom_images_pure::*;
use serde_json::json;

#[test]
fn r681_constants_match_node() {
    assert_eq!(ACTIVE_SETUP_STATUSES, &["starting", "waiting_for_user", "capturing"]);
    assert_eq!(DEFAULT_SETUP_TTL_SECONDS, 60 * 60);
    assert_eq!(DEFAULT_CONNECTION_EXPIRES_IN_MINUTES, 15);
    assert_eq!(SETUP_RPC_COMPANY_ID_METADATA_KEY, "setupRpcCompanyId");
    assert_eq!(
        SOURCE_ENVIRONMENT_CONFIG_FINGERPRINT_METADATA_KEY,
        "sourceEnvironmentConfigFingerprint"
    );
}

#[test]
fn r681_read_string_basic() {
    assert_eq!(read_string(&json!("hello")), Some("hello".to_string()));
    assert_eq!(read_string(&json!("  spaced  ")), Some("spaced".to_string()));
    assert_eq!(read_string(&json!("")), None);
    assert_eq!(read_string(&json!("   ")), None);
    assert_eq!(read_string(&json!(123)), None);
    assert_eq!(read_string(&json!(null)), None);
}

#[test]
fn r681_read_connection_type_known() {
    assert_eq!(read_connection_type(Some("ssh")), EnvironmentCustomImageSetupConnectionType::Ssh);
    assert_eq!(read_connection_type(Some("web")), EnvironmentCustomImageSetupConnectionType::Web);
    assert_eq!(read_connection_type(Some("exec")), EnvironmentCustomImageSetupConnectionType::Exec);
    assert_eq!(read_connection_type(Some("database")), EnvironmentCustomImageSetupConnectionType::Database);
    assert_eq!(read_connection_type(Some("custom")), EnvironmentCustomImageSetupConnectionType::Custom);
}

#[test]
fn r681_read_connection_type_unknown_falls_back() {
    assert_eq!(read_connection_type(Some("mystery")), EnvironmentCustomImageSetupConnectionType::Unknown);
    assert_eq!(read_connection_type(None), EnvironmentCustomImageSetupConnectionType::Unknown);
    assert_eq!(read_connection_type(Some("")), EnvironmentCustomImageSetupConnectionType::Unknown);
}

#[test]
fn r681_to_date_passthrough() {
    assert_eq!(to_date(None), None);
    assert_eq!(to_date(Some("")), None);
    assert_eq!(to_date(Some("2026-08-16T10:00:00Z")), Some("2026-08-16T10:00:00Z".to_string()));
}

#[test]
fn r681_to_session_field_mapping() {
    let row = SetupSessionRow {
        id: "s1".into(),
        environment_id: "e1".into(),
        template_id: Some("t1".into()),
        promoted_template_id: None,
        provider: "aws".into(),
        provider_lease_id: None,
        environment_lease_id: None,
        status: "starting".into(),
        started_by_user_id: Some("u1".into()),
        started_by_agent_id: None,
        base_template_ref: None,
        expires_at: Some("2026-08-16T11:00:00Z".into()),
        finished_at: None,
        failure_reason: None,
        connection_summary: None,
        connection_secret_ref: None,
        metadata: Some(json!({"k": "v"})),
        created_at: "2026-08-16T09:00:00Z".into(),
        updated_at: "2026-08-16T09:30:00Z".into(),
    };
    let s = to_session(&row);
    assert_eq!(s.id, "s1");
    assert_eq!(s.environment_id, "e1");
    assert_eq!(s.template_id, Some("t1".into()));
    assert_eq!(s.provider, "aws");
    assert_eq!(s.status, EnvironmentCustomImageSetupSessionStatus::Starting);
    assert_eq!(s.started_by_user_id, Some("u1".into()));
    assert_eq!(s.metadata, Some(json!({"k": "v"})));
}

#[test]
fn r681_to_session_invalid_status_falls_back_to_failed() {
    let mut row = SetupSessionRow::default();
    row.id = "s1".into();
    row.environment_id = "e1".into();
    row.provider = "aws".into();
    row.status = "garbage".into();
    row.created_at = "2026-08-16T09:00:00Z".into();
    row.updated_at = "2026-08-16T09:30:00Z".into();
    let s = to_session(&row);
    assert_eq!(s.status, EnvironmentCustomImageSetupSessionStatus::Failed);
}

#[test]
fn r681_normalize_connection_summary_basic() {
    let raw = json!({
        "type": "ssh",
        "label": "  primary  ",
        "host": "1.2.3.4",
        "port": 22,
        "username": "ubuntu",
    });
    let s = normalize_connection_summary(Some(&raw)).unwrap();
    assert_eq!(s.ty, EnvironmentCustomImageSetupConnectionType::Ssh);
    assert_eq!(s.label, Some("primary".to_string()));
    assert!(s.host_redacted);
    assert!(s.port_redacted);
    assert!(s.username.is_none()); // explicit None
}

#[test]
fn r681_normalize_connection_summary_null_label_omitted() {
    let raw = json!({"type": "web", "label": ""});
    let s = normalize_connection_summary(Some(&raw)).unwrap();
    assert_eq!(s.ty, EnvironmentCustomImageSetupConnectionType::Web);
    assert!(s.label.is_none());
}

#[test]
fn r681_normalize_connection_summary_none_input() {
    assert!(normalize_connection_summary(None).is_none());
}

#[test]
fn r681_metadata_record_falsy_yields_empty_object() {
    let v = metadata_record(None);
    assert_eq!(v, json!({}));
    let v = metadata_record(Some(&json!(null)));
    assert_eq!(v, json!({}));
    let v = metadata_record(Some(&json!([])));
    assert_eq!(v, json!({}));
}

#[test]
fn r681_metadata_record_object_passthrough() {
    let v = metadata_record(Some(&json!({"a": 1, "b": "x"})));
    assert_eq!(v, json!({"a": 1, "b": "x"}));
}

#[test]
fn r681_normalize_setup_rpc_company_id() {
    assert_eq!(normalize_setup_rpc_company_id(&json!("c-123")), Some("c-123".to_string()));
    assert_eq!(normalize_setup_rpc_company_id(&json!("  c-456  ")), Some("c-456".to_string()));
    assert_eq!(normalize_setup_rpc_company_id(&json!("")), None);
    assert_eq!(normalize_setup_rpc_company_id(&json!(42)), None);
}

#[test]
fn r681_read_setup_rpc_company_id_present() {
    let m = json!({"setupRpcCompanyId": "c-99", "other": "ignore"});
    assert_eq!(read_setup_rpc_company_id(Some(&m)), Some("c-99".to_string()));
}

#[test]
fn r681_read_setup_rpc_company_id_missing() {
    let m = json!({"other": "x"});
    assert_eq!(read_setup_rpc_company_id(Some(&m)), None);
    assert_eq!(read_setup_rpc_company_id(None), None);
}

#[test]
fn r681_persisted_setup_metadata_keep_allowlisted() {
    let m = json!({
        "setupRpcCompanyId": "c-1",
        "sourceEnvironmentConfigFingerprint": "fp-1",
        "noise": "drop-me",
    });
    let out = persisted_setup_metadata(Some(&m));
    let obj = out.as_object().unwrap();
    assert_eq!(obj.len(), 2);
    assert_eq!(obj.get("setupRpcCompanyId"), Some(&json!("c-1")));
    assert_eq!(obj.get("sourceEnvironmentConfigFingerprint"), Some(&json!("fp-1")));
    assert!(obj.get("noise").is_none());
}

#[test]
fn r681_persisted_setup_metadata_empty_when_only_noise() {
    let m = json!({"noise": "x"});
    let out = persisted_setup_metadata(Some(&m));
    assert_eq!(out, json!({}));
}

#[test]
fn r681_persisted_setup_metadata_invalid_company_id_dropped() {
    let m = json!({"setupRpcCompanyId": "", "sourceEnvironmentConfigFingerprint": "fp-1"});
    let out = persisted_setup_metadata(Some(&m));
    let obj = out.as_object().unwrap();
    assert_eq!(obj.len(), 1);
    assert!(obj.get("setupRpcCompanyId").is_none());
    assert_eq!(obj.get("sourceEnvironmentConfigFingerprint"), Some(&json!("fp-1")));
}

#[test]
fn r681_merge_setup_session_metadata_provider_overrides_then_persisted_overrides() {
    let existing = json!({"setupRpcCompanyId": "old", "noise": "drop"});
    let provider = json!({"setupRpcCompanyId": "from-provider", "provider_field": "p1"});
    let out = merge_setup_session_metadata(Some(&existing), Some(&provider)).unwrap();
    let obj = out.as_object().unwrap();
    // merge_setup_session_metadata semantics: provider first, persisted overrides provider.
    assert_eq!(obj.get("setupRpcCompanyId"), Some(&json!("old")));
    assert!(obj.get("noise").is_none()); // dropped by persisted filter
    assert_eq!(obj.get("provider_field"), Some(&json!("p1"))); // provider fields pass through
}

#[test]
fn r681_merge_setup_session_metadata_empty_returns_none() {
    let out = merge_setup_session_metadata(None, None);
    assert!(out.is_none());
    let out = merge_setup_session_metadata(Some(&json!({})), Some(&json!({})));
    assert!(out.is_none());
}

#[test]
fn r681_normalize_persisted_status_known() {
    assert_eq!(
        normalize_persisted_status("starting", EnvironmentCustomImageSetupSessionStatus::Failed),
        EnvironmentCustomImageSetupSessionStatus::Starting
    );
    assert_eq!(
        normalize_persisted_status("waiting_for_user", EnvironmentCustomImageSetupSessionStatus::Failed),
        EnvironmentCustomImageSetupSessionStatus::WaitingForUser
    );
    assert_eq!(
        normalize_persisted_status("timed_out", EnvironmentCustomImageSetupSessionStatus::Failed),
        EnvironmentCustomImageSetupSessionStatus::TimedOut
    );
}

#[test]
fn r681_normalize_persisted_status_unknown_uses_fallback() {
    assert_eq!(
        normalize_persisted_status("nonsense", EnvironmentCustomImageSetupSessionStatus::Failed),
        EnvironmentCustomImageSetupSessionStatus::Failed
    );
    assert_eq!(
        normalize_persisted_status("nonsense", EnvironmentCustomImageSetupSessionStatus::Cancelled),
        EnvironmentCustomImageSetupSessionStatus::Cancelled
    );
}

#[test]
fn r681_add_seconds_iso() {
    let out = add_seconds("2026-08-16T10:00:00+00:00", 60);
    assert_eq!(out, "2026-08-16T10:01:00+00:00");
    let out = add_seconds("2026-08-16T10:00:00+00:00", 3600);
    assert_eq!(out, "2026-08-16T11:00:00+00:00");
}

#[test]
fn r681_is_active_setup_status() {
    assert!(is_active_setup_status(EnvironmentCustomImageSetupSessionStatus::Starting));
    assert!(is_active_setup_status(EnvironmentCustomImageSetupSessionStatus::WaitingForUser));
    assert!(is_active_setup_status(EnvironmentCustomImageSetupSessionStatus::Capturing));
    assert!(!is_active_setup_status(EnvironmentCustomImageSetupSessionStatus::Succeeded));
    assert!(!is_active_setup_status(EnvironmentCustomImageSetupSessionStatus::Failed));
    assert!(!is_active_setup_status(EnvironmentCustomImageSetupSessionStatus::Cancelled));
    assert!(!is_active_setup_status(EnvironmentCustomImageSetupSessionStatus::TimedOut));
}

#[test]
fn r681_template_config_binding_from_driver_explicit() {
    let explicit = json!({"field": "snapshot", "snapshot": "snap-1"});
    let out = template_config_binding_from_driver(Some("live"), Some(&explicit));
    assert_eq!(out, explicit);
}

#[test]
fn r681_template_config_binding_from_driver_null_binding_uses_default() {
    let out = template_config_binding_from_driver(Some("snapshot"), None);
    assert_eq!(out.get("field"), Some(&json!("snapshot")));
}

#[test]
fn r681_template_config_binding_from_driver_default_kind() {
    let out = template_config_binding_from_driver(None, Some(&json!(null)));
    assert_eq!(out.get("field"), Some(&json!("snapshot")));
}

#[test]
fn r681_source_template_from_config_match_field() {
    let cfg = json!({"snapshot": "snap-1"});
    let binding = json!({"field": "snapshot"});
    let (src, kind) = source_template_from_config(&cfg, &binding, EnvironmentCustomImageTemplateKind::Live);
    assert_eq!(src, Some("snap-1".to_string()));
    assert_eq!(kind, Some(EnvironmentCustomImageTemplateKind::Live));
}

#[test]
fn r681_source_template_from_config_no_field_falls_back_to_snapshot() {
    let cfg = json!({"snapshot": "snap-1"});
    let binding = json!({"field": "missing"});
    let (src, kind) = source_template_from_config(&cfg, &binding, EnvironmentCustomImageTemplateKind::Live);
    assert_eq!(src, Some("snap-1".to_string()));
    assert_eq!(kind, Some(EnvironmentCustomImageTemplateKind::Snapshot));
}

#[test]
fn r681_source_template_from_config_empty_returns_none() {
    let cfg = json!({});
    let binding = json!({"field": "snapshot"});
    let (src, kind) = source_template_from_config(&cfg, &binding, EnvironmentCustomImageTemplateKind::Live);
    assert_eq!(src, None);
    assert_eq!(kind, None);
}

#[test]
fn r681_overview_default() {
    let o = EnvironmentCustomImageOverview::default();
    assert!(o.active_template.is_none());
    assert!(o.active_template_matches_config.is_none());
    assert!(o.active_session.is_none());
    assert!(o.latest_session.is_none());
}

#[test]
fn r681_overview_serde_roundtrip() {
    let o = EnvironmentCustomImageOverview {
        active_template: Some(json!({"id": "t1"})),
        active_template_matches_config: Some(true),
        active_session: None,
        latest_session: None,
    };
    let s = serde_json::to_string(&o).unwrap();
    let back: EnvironmentCustomImageOverview = serde_json::from_str(&s).unwrap();
    assert_eq!(back, o);
}

#[test]
fn r681_reconciliation_union_tagged_roundtrip() {
    let r = EnvironmentCustomImageReconciliation::None;
    let s = serde_json::to_string(&r).unwrap();
    assert!(s.contains("\"action\":\"none\""));
    let r = EnvironmentCustomImageReconciliation::Relinked {
        template: json!({"id": "t1"}),
    };
    let s = serde_json::to_string(&r).unwrap();
    assert!(s.contains("\"action\":\"relinked\""));
    let r = EnvironmentCustomImageReconciliation::Detached {
        template: json!({"id": "t2"}),
    };
    let s = serde_json::to_string(&r).unwrap();
    assert!(s.contains("\"action\":\"detached\""));
}

#[test]
fn r681_setup_session_result_default() {
    let r = EnvironmentCustomImageSetupSessionResult::default();
    assert_eq!(r.session.id, "");
    assert!(r.connection_payload.is_none());
}

#[test]
fn r681_cleanup_result_default() {
    let r = EnvironmentCustomImageSetupCleanupResult::default();
    assert_eq!(r.scanned, 0);
    assert_eq!(r.timed_out, 0);
    assert_eq!(r.failed, 0);
}

#[test]
fn r681_factory_signature_creates_handle() {
    let db = DbHandle { label: "db".into() };
    let wm = PluginWorkerManagerHandle { label: "wm".into() };
    let opts = EnvironmentCustomImageServiceOptions {
        plugin_worker_manager: Some(wm),
    };
    let h = environment_custom_image_service(db.clone(), opts);
    assert_eq!(h.db.label, "db");
    assert!(h.plugin_worker_manager.is_some());
}

#[test]
fn r681_factory_signature_no_worker_manager() {
    let db = DbHandle { label: "db".into() };
    let h = environment_custom_image_service(db, EnvironmentCustomImageServiceOptions::default());
    assert!(h.plugin_worker_manager.is_none());
}
