// SPDX-License-Identifier: MIT
//
// R681 parity: `environment-custom-images.ts` pure helpers + types.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const ACTIVE_SETUP_STATUSES: &[&str] = &["starting", "waiting_for_user", "capturing"];
pub const DEFAULT_SETUP_TTL_SECONDS: u64 = 60 * 60;
pub const DEFAULT_CONNECTION_EXPIRES_IN_MINUTES: u64 = 15;
pub const SETUP_RPC_COMPANY_ID_METADATA_KEY: &str = "setupRpcCompanyId";
pub const SOURCE_ENVIRONMENT_CONFIG_FINGERPRINT_METADATA_KEY: &str =
    "sourceEnvironmentConfigFingerprint";

// ---------------------------------------------------------------------------
// Domain enums
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentCustomImageSetupSessionStatus {
    #[default]
    Starting,
    WaitingForUser,
    Capturing,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

pub const ENVIRONMENT_CUSTOM_IMAGE_SETUP_SESSION_STATUSES: &[&str] = &[
    "starting",
    "waiting_for_user",
    "capturing",
    "succeeded",
    "failed",
    "cancelled",
    "timed_out",
];

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentCustomImageSetupConnectionType {
    Ssh,
    Web,
    Exec,
    Database,
    Custom,
    #[default]
    Unknown,
}

pub const ENVIRONMENT_CUSTOM_IMAGE_SETUP_CONNECTION_TYPES: &[&str] = &[
    "ssh", "web", "exec", "database", "custom",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentCustomImageTemplateKind {
    Snapshot,
    Live,
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EnvironmentCustomImageSetupConnectionSummary {
    #[serde(rename = "type")]
    pub ty: EnvironmentCustomImageSetupConnectionType,
    pub username: Option<String>,
    pub host_redacted: bool,
    pub port_redacted: bool,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EnvironmentCustomImageSetupSession {
    pub id: String,
    pub environment_id: String,
    pub template_id: Option<String>,
    pub promoted_template_id: Option<String>,
    pub provider: String,
    pub provider_lease_id: Option<String>,
    pub environment_lease_id: Option<String>,
    pub status: EnvironmentCustomImageSetupSessionStatus,
    pub started_by_user_id: Option<String>,
    pub started_by_agent_id: Option<String>,
    pub base_template_ref: Option<String>,
    pub expires_at: Option<String>,
    pub finished_at: Option<String>,
    pub failure_reason: Option<String>,
    pub connection_summary: Option<EnvironmentCustomImageSetupConnectionSummary>,
    pub connection_secret_ref: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SetupSessionRow {
    pub id: String,
    pub environment_id: String,
    pub template_id: Option<String>,
    pub promoted_template_id: Option<String>,
    pub provider: String,
    pub provider_lease_id: Option<String>,
    pub environment_lease_id: Option<String>,
    pub status: String,
    pub started_by_user_id: Option<String>,
    pub started_by_agent_id: Option<String>,
    pub base_template_ref: Option<String>,
    pub expires_at: Option<String>,
    pub finished_at: Option<String>,
    pub failure_reason: Option<String>,
    pub connection_summary: Option<EnvironmentCustomImageSetupConnectionSummary>,
    pub connection_secret_ref: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// Export interfaces
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EnvironmentCustomImageOverview {
    pub active_template: Option<serde_json::Value>,
    pub active_template_matches_config: Option<bool>,
    pub active_session: Option<EnvironmentCustomImageSetupSession>,
    pub latest_session: Option<EnvironmentCustomImageSetupSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum EnvironmentCustomImageReconciliation {
    None,
    Relinked { template: serde_json::Value },
    Detached { template: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EnvironmentCustomImageSetupSessionResult {
    pub session: EnvironmentCustomImageSetupSession,
    pub connection_payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EnvironmentCustomImageSetupCleanupResult {
    pub scanned: u64,
    pub timed_out: u64,
    pub failed: u64,
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

pub fn to_session(row: &SetupSessionRow) -> EnvironmentCustomImageSetupSession {
    EnvironmentCustomImageSetupSession {
        id: row.id.clone(),
        environment_id: row.environment_id.clone(),
        template_id: row.template_id.clone(),
        promoted_template_id: row.promoted_template_id.clone(),
        provider: row.provider.clone(),
        provider_lease_id: row.provider_lease_id.clone(),
        environment_lease_id: row.environment_lease_id.clone(),
        status: normalize_persisted_status(&row.status, EnvironmentCustomImageSetupSessionStatus::Failed),
        started_by_user_id: row.started_by_user_id.clone(),
        started_by_agent_id: row.started_by_agent_id.clone(),
        base_template_ref: row.base_template_ref.clone(),
        expires_at: row.expires_at.clone(),
        finished_at: row.finished_at.clone(),
        failure_reason: row.failure_reason.clone(),
        connection_summary: row.connection_summary.clone(),
        connection_secret_ref: row.connection_secret_ref.clone(),
        metadata: row.metadata.clone(),
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}

pub fn read_connection_type(value: Option<&str>) -> EnvironmentCustomImageSetupConnectionType {
    match value {
        Some("ssh") => EnvironmentCustomImageSetupConnectionType::Ssh,
        Some("web") => EnvironmentCustomImageSetupConnectionType::Web,
        Some("exec") => EnvironmentCustomImageSetupConnectionType::Exec,
        Some("database") => EnvironmentCustomImageSetupConnectionType::Database,
        Some("custom") => EnvironmentCustomImageSetupConnectionType::Custom,
        _ => EnvironmentCustomImageSetupConnectionType::Unknown,
    }
}

pub fn read_string(value: &serde_json::Value) -> Option<String> {
    if let serde_json::Value::String(s) = value {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

pub fn to_date(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.is_empty()).map(|v| v.to_string())
}

pub fn normalize_connection_summary(
    summary: Option<&serde_json::Value>,
) -> Option<EnvironmentCustomImageSetupConnectionSummary> {
    let summary = summary?;
    let obj = summary.as_object()?;
    let ty_str = obj.get("type").and_then(|v| v.as_str());
    let ty = read_connection_type(ty_str);
    let label = obj.get("label").and_then(read_string);
    let mut s = EnvironmentCustomImageSetupConnectionSummary {
        ty,
        username: None,
        host_redacted: true,
        port_redacted: true,
        label: None,
    };
    if let Some(l) = label {
        s.label = Some(l);
    }
    Some(s)
}

pub fn normalize_provider_metadata(metadata: Option<serde_json::Value>) -> Option<serde_json::Value> {
    metadata
}

pub fn metadata_record(metadata: Option<&serde_json::Value>) -> serde_json::Value {
    match metadata {
        Some(serde_json::Value::Object(_)) => metadata.cloned().unwrap(),
        _ => serde_json::Value::Object(serde_json::Map::new()),
    }
}

pub fn normalize_setup_rpc_company_id(value: &serde_json::Value) -> Option<String> {
    read_string(value)
}

pub fn read_setup_rpc_company_id(metadata: Option<&serde_json::Value>) -> Option<String> {
    let rec = metadata_record(metadata);
    rec.get(SETUP_RPC_COMPANY_ID_METADATA_KEY)
        .and_then(normalize_setup_rpc_company_id)
}

pub fn persisted_setup_metadata(metadata: Option<&serde_json::Value>) -> serde_json::Value {
    let record = metadata_record(metadata);
    let mut out = serde_json::Map::new();
    if let Some(v) = record.get(SETUP_RPC_COMPANY_ID_METADATA_KEY) {
        if let Some(s) = normalize_setup_rpc_company_id(v) {
            out.insert(SETUP_RPC_COMPANY_ID_METADATA_KEY.to_string(), serde_json::Value::String(s));
        }
    }
    if let Some(v) = record.get(SOURCE_ENVIRONMENT_CONFIG_FINGERPRINT_METADATA_KEY) {
        if let Some(s) = read_string(v) {
            out.insert(
                SOURCE_ENVIRONMENT_CONFIG_FINGERPRINT_METADATA_KEY.to_string(),
                serde_json::Value::String(s),
            );
        }
    }
    serde_json::Value::Object(out)
}

pub fn merge_setup_session_metadata(
    existing: Option<&serde_json::Value>,
    provider_metadata: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    let provider = normalize_provider_metadata(provider_metadata.cloned())
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    let persisted = persisted_setup_metadata(existing);
    let mut merged = serde_json::Map::new();
    if let serde_json::Value::Object(map) = provider {
        for (k, v) in map {
            merged.insert(k, v);
        }
    }
    if let serde_json::Value::Object(map) = persisted {
        for (k, v) in map {
            merged.insert(k, v);
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(merged))
    }
}

pub fn normalize_persisted_status(
    status: &str,
    fallback: EnvironmentCustomImageSetupSessionStatus,
) -> EnvironmentCustomImageSetupSessionStatus {
    if !ENVIRONMENT_CUSTOM_IMAGE_SETUP_SESSION_STATUSES.contains(&status) {
        return fallback;
    }
    match status {
        "starting" => EnvironmentCustomImageSetupSessionStatus::Starting,
        "waiting_for_user" => EnvironmentCustomImageSetupSessionStatus::WaitingForUser,
        "capturing" => EnvironmentCustomImageSetupSessionStatus::Capturing,
        "succeeded" => EnvironmentCustomImageSetupSessionStatus::Succeeded,
        "failed" => EnvironmentCustomImageSetupSessionStatus::Failed,
        "cancelled" => EnvironmentCustomImageSetupSessionStatus::Cancelled,
        "timed_out" => EnvironmentCustomImageSetupSessionStatus::TimedOut,
        _ => fallback,
    }
}

pub fn add_seconds(date_iso: &str, seconds: u64) -> String {
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(date_iso) {
        let shifted = d + chrono::Duration::seconds(seconds as i64);
        return shifted.to_rfc3339();
    }
    format!("{}+{}s", date_iso, seconds)
}

pub fn is_active_setup_status(status: EnvironmentCustomImageSetupSessionStatus) -> bool {
    let s = match status {
        EnvironmentCustomImageSetupSessionStatus::Starting => "starting",
        EnvironmentCustomImageSetupSessionStatus::WaitingForUser => "waiting_for_user",
        EnvironmentCustomImageSetupSessionStatus::Capturing => "capturing",
        _ => "",
    };
    ACTIVE_SETUP_STATUSES.contains(&s)
}

pub fn template_config_binding_from_driver(
    template_ref_kind: Option<&str>,
    template_config_binding: Option<&serde_json::Value>,
) -> serde_json::Value {
    if let Some(v) = template_config_binding {
        if !v.is_null() {
            return v.clone();
        }
    }
    let kind = template_ref_kind.unwrap_or("snapshot");
    serde_json::json!({
        "field": kind,
        "snapshot": "snapshot",
    })
}

pub fn source_template_from_config(
    config: &serde_json::Value,
    binding: &serde_json::Value,
    template_kind: EnvironmentCustomImageTemplateKind,
) -> (Option<String>, Option<EnvironmentCustomImageTemplateKind>) {
    let field = binding
        .get("field")
        .and_then(|v| v.as_str())
        .unwrap_or("snapshot");
    if let Some(v) = config.get(field).and_then(read_string) {
        return (Some(v), Some(template_kind));
    }
    if let Some(v) = config.get("snapshot").and_then(read_string) {
        return (Some(v), Some(EnvironmentCustomImageTemplateKind::Snapshot));
    }
    (None, None)
}

// ---------------------------------------------------------------------------
// Factory signature
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DbHandle {
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct PluginWorkerManagerHandle {
    pub label: String,
}

#[derive(Debug, Clone, Default)]
pub struct EnvironmentCustomImageServiceOptions {
    pub plugin_worker_manager: Option<PluginWorkerManagerHandle>,
}

#[derive(Debug, Clone)]
pub struct EnvironmentCustomImageServiceHandle {
    pub db: DbHandle,
    pub plugin_worker_manager: Option<PluginWorkerManagerHandle>,
}

pub fn environment_custom_image_service(
    db: DbHandle,
    options: EnvironmentCustomImageServiceOptions,
) -> EnvironmentCustomImageServiceHandle {
    EnvironmentCustomImageServiceHandle {
        db,
        plugin_worker_manager: options.plugin_worker_manager,
    }
}
