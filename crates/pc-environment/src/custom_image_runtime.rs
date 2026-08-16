//! Pure environment custom-image runtime helpers.
//!
//! Mirrors Node `server/src/services/environment-custom-image-runtime.ts` 1:1 for
//! the **pure** (no DB, no Plugin runtime) helper set:
//!
//! - Constants: `ENVIRONMENT_CUSTOM_IMAGE_RUNTIME_CONFIG_BINDING_METADATA_KEY`,
//!   `ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS`,
//!   `ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_SOURCE_FIELDS`,
//!   `ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_KINDS`.
//! - `readEnvironmentCustomImageTemplateKind`
//! - `defaultEnvironmentCustomImageRuntimeConfigBinding`
//! - `normalizeEnvironmentCustomImageRuntimeConfigBinding`
//! - `resolveEnvironmentCustomImageRuntimeConfigBinding`
//! - `fingerprintEnvironmentSandboxProviderConfig`
//! - `applyCustomImageTemplateToSandboxConfig`
//! - `environmentCustomImageTemplateMatchesBaseConfig`
//! - `classifyEnvironmentCustomImageConfigChange`
//! - `environmentCustomImageTemplateFromRow` — pure DB row mapper (DB-touching
//!   function `resolveActiveEnvironmentCustomImageTemplateForRuntime` is out of scope)
//!
//! Uses `pc_secrets::json_schema_secret_refs::{read_config_value_at_path,
//! write_config_value_at_path}` for path-based field access — mirrors Node's
//! imports from `json-schema-secret-refs.ts`.

use std::collections::HashSet;

use pc_secrets::json_schema_secret_refs::{read_config_value_at_path, write_config_value_at_path};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

// ============================================================================
// Constants
// ============================================================================

/// Metadata key under which the runtime-config binding is stored
/// (mirrors Node `ENVIRONMENT_CUSTOM_IMAGE_RUNTIME_CONFIG_BINDING_METADATA_KEY`).
pub const ENVIRONMENT_CUSTOM_IMAGE_RUNTIME_CONFIG_BINDING_METADATA_KEY: &str =
    "runtimeConfigBinding";

/// Config paths stripped before computing the fingerprint
/// (mirrors Node `ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS`).
pub const ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS: &[&str] = &[
    "timeoutMs",
    "reuseLease",
    "streamRunLogs",
    "archiveOnRelease",
    "cpu",
    "memory",
    "disk",
    "gpu",
    "autoStopInterval",
    "autoArchiveInterval",
    "autoDeleteInterval",
];

/// Source-template fields (a change to any of these is "breaking").
/// Mirrors Node `ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_SOURCE_FIELDS`.
pub const ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_SOURCE_FIELDS: &[&str] =
    &["snapshot", "image", "template"];

/// Allowed template kinds (mirrors Node `ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_KINDS`).
pub const ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_KINDS: &[&str] =
    &["snapshot", "image", "provider_template", "unknown"];

// ============================================================================
// Types
// ============================================================================

/// A binding describes which config field the active template ref should be
/// written into, and which fields must be cleared.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentCustomImageRuntimeConfigBinding {
    pub field: String,
    #[serde(rename = "unsetFields")]
    pub unset_fields: Vec<String>,
}

/// Template kind (1:1 with Node `EnvironmentCustomImageTemplateKind`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentCustomImageTemplateKind {
    #[default]
    Unknown,
    Snapshot,
    Image,
    ProviderTemplate,
}

impl EnvironmentCustomImageTemplateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Image => "image",
            Self::ProviderTemplate => "provider_template",
            Self::Unknown => "unknown",
        }
    }
}

/// Outcome of classifying a config change against an active captured template
/// (1:1 with Node `EnvironmentCustomImageConfigChangeKind`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentCustomImageConfigChangeKind {
    None,
    Relinkable,
    Breaking,
}

impl EnvironmentCustomImageConfigChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Relinkable => "relinkable",
            Self::Breaking => "breaking",
        }
    }
}

/// Template record (1:1 with Node `EnvironmentCustomImageTemplate`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EnvironmentCustomImageTemplate {
    pub id: String,
    #[serde(rename = "environmentId")]
    pub environment_id: String,
    pub provider: String,
    #[serde(rename = "templateKind")]
    pub template_kind: EnvironmentCustomImageTemplateKind,
    #[serde(rename = "templateRef")]
    pub template_ref: Option<String>,
    #[serde(rename = "sourceTemplateRef")]
    pub source_template_ref: Option<String>,
    #[serde(rename = "sourceEnvironmentConfigFingerprint")]
    pub source_environment_config_fingerprint: Option<String>,
    pub status: String,
    #[serde(rename = "createdByUserId")]
    pub created_by_user_id: Option<String>,
    #[serde(rename = "createdByAgentId")]
    pub created_by_agent_id: Option<String>,
    #[serde(rename = "capturedAt")]
    pub captured_at: Option<String>,
    #[serde(rename = "lastUsedAt")]
    pub last_used_at: Option<String>,
    #[serde(rename = "supersededByTemplateId")]
    pub superseded_by_template_id: Option<String>,
    pub metadata: Option<Value>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

/// Minimal input for template-apply / match helpers
/// (mirrors Node's `Pick<EnvironmentCustomImageTemplate, "templateKind" | "templateRef" | "metadata">`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateBindingInput {
    #[serde(rename = "templateKind")]
    pub template_kind: EnvironmentCustomImageTemplateKind,
    #[serde(rename = "templateRef")]
    pub template_ref: Option<String>,
    pub metadata: Option<Value>,
}

// ============================================================================
// Internal helpers
// ============================================================================

fn is_record(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Object(_)))
}

/// Stable JSON stringify — sorts object keys recursively so structurally equal
/// but textually different JSON produces the same string.
/// Mirrors Node's private `stableStringify`.
pub fn stable_stringify(value: &Value) -> String {
    match value {
        Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(stable_stringify).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(obj) => {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_else(|_| "null".to_string()),
                        stable_stringify(&obj[*k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        _ => serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()),
    }
}

/// Same regex as Node: `/^[A-Za-z_][A-Za-z0-9_-]*$/` plus exclusion of literal
/// `"provider"`.
fn is_valid_runtime_config_binding_field(value: &str) -> bool {
    if value == "provider" {
        return false;
    }
    if value.is_empty() {
        return false;
    }
    let bytes = value.as_bytes();
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

// ============================================================================
// Read / default / normalize / resolve
// ============================================================================

/// Type guard: returns the input if it's a known kind, else `"unknown"`.
pub fn read_environment_custom_image_template_kind(
    value: Option<&str>,
) -> EnvironmentCustomImageTemplateKind {
    match value {
        Some(v) if ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_KINDS.contains(&v) => match v {
            "snapshot" => EnvironmentCustomImageTemplateKind::Snapshot,
            "image" => EnvironmentCustomImageTemplateKind::Image,
            "provider_template" => EnvironmentCustomImageTemplateKind::ProviderTemplate,
            _ => EnvironmentCustomImageTemplateKind::Unknown,
        },
        _ => EnvironmentCustomImageTemplateKind::Unknown,
    }
}

/// Default binding by template kind (mirrors Node `defaultEnvironmentCustomImageRuntimeConfigBinding`).
pub fn default_environment_custom_image_runtime_config_binding(
    template_kind: Option<&str>,
) -> EnvironmentCustomImageRuntimeConfigBinding {
    let kind = read_environment_custom_image_template_kind(template_kind);
    match kind {
        EnvironmentCustomImageTemplateKind::Snapshot => EnvironmentCustomImageRuntimeConfigBinding {
            field: "snapshot".into(),
            unset_fields: vec!["image".into()],
        },
        EnvironmentCustomImageTemplateKind::Image => EnvironmentCustomImageRuntimeConfigBinding {
            field: "image".into(),
            unset_fields: vec!["snapshot".into()],
        },
        EnvironmentCustomImageTemplateKind::ProviderTemplate => {
            EnvironmentCustomImageRuntimeConfigBinding {
                field: "template".into(),
                unset_fields: vec![],
            }
        }
        EnvironmentCustomImageTemplateKind::Unknown => {
            EnvironmentCustomImageRuntimeConfigBinding {
                field: "templateRef".into(),
                unset_fields: vec![],
            }
        }
    }
}

/// Normalize + validate a binding from external (JSON) value.
/// Returns `None` if the input is malformed.
pub fn normalize_environment_custom_image_runtime_config_binding(
    value: &Value,
) -> Option<EnvironmentCustomImageRuntimeConfigBinding> {
    if !is_record(Some(value)) {
        return None;
    }
    let obj = value.as_object().unwrap();
    let field = obj.get("field").and_then(|v| v.as_str())?;
    if !is_valid_runtime_config_binding_field(field) {
        return None;
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut unset_fields: Vec<String> = Vec::new();
    if let Some(Value::Array(arr)) = obj.get("unsetFields") {
        for entry in arr {
            let s = match entry.as_str() {
                Some(s) => s,
                None => continue,
            };
            if !is_valid_runtime_config_binding_field(s) || s == field {
                continue;
            }
            if seen.insert(s.to_string()) {
                unset_fields.push(s.to_string());
            }
        }
    }
    Some(EnvironmentCustomImageRuntimeConfigBinding {
        field: field.to_string(),
        unset_fields,
    })
}

/// Top-level binding resolver — prefers normalized metadata binding; falls back
/// to default by template kind. Mirrors Node
/// `resolveEnvironmentCustomImageRuntimeConfigBinding`.
pub fn resolve_environment_custom_image_runtime_config_binding(
    input: ResolveBindingInput,
) -> EnvironmentCustomImageRuntimeConfigBinding {
    let metadata = input.metadata.as_ref();
    let key = ENVIRONMENT_CUSTOM_IMAGE_RUNTIME_CONFIG_BINDING_METADATA_KEY;
    let raw = metadata.and_then(|m| m.as_object()).and_then(|o| o.get(key));
    match raw.and_then(|v| normalize_environment_custom_image_runtime_config_binding(v)) {
        Some(b) => b,
        None => {
            let kind = input.template_kind.as_deref();
            default_environment_custom_image_runtime_config_binding(kind)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResolveBindingInput {
    pub template_kind: Option<String>,
    pub metadata: Option<Value>,
}

// ============================================================================
// Fingerprint
// ============================================================================

/// SHA-256 fingerprint over a sandbox config, after stripping runtime-only
/// fields by dot-path. Stable across JSON key ordering.
/// Mirrors Node `fingerprintEnvironmentSandboxProviderConfig`.
pub fn fingerprint_environment_sandbox_provider_config(
    config: &Value,
    exclude_paths: Option<&[&str]>,
) -> String {
    let mut normalized = config.clone();
    if let Some(paths) = exclude_paths {
        let mut obj_value = match &normalized {
            Value::Object(_) => normalized.clone(),
            _ => Value::Object(Map::new()),
        };
        for path in paths {
            obj_value = write_config_value_at_path(&obj_value, path, None);
        }
        normalized = obj_value;
    }
    let serialized = stable_stringify(&normalized);
    let digest = Sha256::digest(serialized.as_bytes());
    let bytes = digest.as_slice();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{:02x}", b);
    }
    hex
}

// ============================================================================
// Apply template
// ============================================================================

/// Pure config transform — replaces/clears fields per the binding.
/// Mirrors Node `applyCustomImageTemplateToSandboxConfig`.
pub fn apply_custom_image_template_to_sandbox_config(
    config: &Value,
    template: &TemplateBindingInput,
) -> Value {
    let Some(template_ref) = template.template_ref.as_ref() else {
        return config.clone();
    };
    let mut next = match config {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    let binding = resolve_environment_custom_image_runtime_config_binding(
        ResolveBindingInput {
            template_kind: Some(template.template_kind.as_str().to_string()),
            metadata: template.metadata.clone(),
        },
    );
    for field in &binding.unset_fields {
        next.remove(field);
    }
    next.insert(binding.field, Value::String(template_ref.clone()));
    Value::Object(next)
}

// ============================================================================
// Match base config (capture fingerprint)
// ============================================================================

#[derive(Debug, Clone)]
pub struct MatchBaseConfigInput {
    pub template: EnvironmentCustomImageTemplate,
    pub base_config: Value,
    pub secret_ref_exclude_paths: Vec<String>,
}

/// True when the captured template's `sourceEnvironmentConfigFingerprint`
/// matches the (excluded) base config. A missing fingerprint returns true
/// (matches Node "if (!expectedFingerprint) return true"). Mirrors Node
/// `environmentCustomImageTemplateMatchesBaseConfig`.
pub fn environment_custom_image_template_matches_base_config(
    input: &MatchBaseConfigInput,
) -> bool {
    let Some(expected) = input.template.source_environment_config_fingerprint.as_ref() else {
        return true;
    };
    // Combine static runtime-only excludes + caller-provided secret-ref paths
    let mut exclude: Vec<String> = ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS
        .iter()
        .map(|s| s.to_string())
        .collect();
    exclude.extend(input.secret_ref_exclude_paths.iter().cloned());
    let exclude_refs: Vec<&str> = exclude.iter().map(|s| s.as_str()).collect();
    let actual = fingerprint_environment_sandbox_provider_config(&input.base_config, Some(&exclude_refs));
    actual == *expected
}

// ============================================================================
// Classify config change
// ============================================================================

#[derive(Debug, Clone)]
pub struct ClassifyConfigChangeInput {
    pub template: EnvironmentCustomImageTemplate,
    pub previous_config: Value,
    pub next_config: Value,
    pub secret_ref_exclude_paths: Vec<String>,
    pub template_identity_paths: Vec<String>,
}

/// Mirrors Node `classifyEnvironmentCustomImageConfigChange`.
pub fn classify_environment_custom_image_config_change(
    input: &ClassifyConfigChangeInput,
) -> EnvironmentCustomImageConfigChangeKind {
    let exclude: Vec<String> = input.secret_ref_exclude_paths.clone();
    let previous_match = environment_custom_image_template_matches_base_config(
        &MatchBaseConfigInput {
            template: input.template.clone(),
            base_config: input.previous_config.clone(),
            secret_ref_exclude_paths: exclude.clone(),
        },
    );
    if !previous_match {
        return EnvironmentCustomImageConfigChangeKind::None;
    }
    let next_match = environment_custom_image_template_matches_base_config(
        &MatchBaseConfigInput {
            template: input.template.clone(),
            base_config: input.next_config.clone(),
            secret_ref_exclude_paths: exclude,
        },
    );
    if next_match {
        return EnvironmentCustomImageConfigChangeKind::None;
    }
    let binding = resolve_environment_custom_image_runtime_config_binding(
        ResolveBindingInput {
            template_kind: Some(input.template.template_kind.as_str().to_string()),
            metadata: input.template.metadata.clone(),
        },
    );
    let mut breaking_paths: HashSet<String> = HashSet::new();
    breaking_paths.insert("provider".to_string());
    breaking_paths.insert(binding.field.clone());
    for f in &binding.unset_fields {
        breaking_paths.insert(f.clone());
    }
    for f in ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_SOURCE_FIELDS {
        breaking_paths.insert((*f).to_string());
    }
    for f in &input.template_identity_paths {
        breaking_paths.insert(f.clone());
    }
    let prev_value = input.previous_config.clone();
    let next_value = input.next_config.clone();
    for path in &breaking_paths {
        let before = read_config_value_at_path(&prev_value, path);
        let after = read_config_value_at_path(&next_value, path);
        if stable_stringify(before.unwrap_or(&Value::Null))
            != stable_stringify(after.unwrap_or(&Value::Null))
        {
            return EnvironmentCustomImageConfigChangeKind::Breaking;
        }
    }
    EnvironmentCustomImageConfigChangeKind::Relinkable
}

// ============================================================================
// Row mapper
// ============================================================================

/// Raw DB row shape (subset — matches Node `environmentCustomImageTemplateFromRow`).
///
/// The full DB schema is much larger; this represents only the fields needed
/// by parity helpers and is suitable for use from sqlx or hand-constructed
/// fixtures.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvironmentCustomImageTemplateRow {
    pub id: String,
    #[serde(rename = "environmentId")]
    pub environment_id: String,
    pub provider: String,
    #[serde(rename = "templateKind")]
    pub template_kind: String,
    #[serde(rename = "templateRef")]
    pub template_ref: Option<String>,
    #[serde(rename = "sourceTemplateRef")]
    pub source_template_ref: Option<String>,
    #[serde(rename = "sourceEnvironmentConfigFingerprint")]
    pub source_environment_config_fingerprint: Option<String>,
    pub status: String,
    #[serde(rename = "createdByUserId")]
    pub created_by_user_id: Option<String>,
    #[serde(rename = "createdByAgentId")]
    pub created_by_agent_id: Option<String>,
    #[serde(rename = "capturedAt")]
    pub captured_at: Option<String>,
    #[serde(rename = "lastUsedAt")]
    pub last_used_at: Option<String>,
    #[serde(rename = "supersededByTemplateId")]
    pub superseded_by_template_id: Option<String>,
    pub metadata: Option<Value>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

/// Pure DB-row mapper; mirrors Node `environmentCustomImageTemplateFromRow`.
pub fn environment_custom_image_template_from_row(
    row: &EnvironmentCustomImageTemplateRow,
) -> EnvironmentCustomImageTemplate {
    EnvironmentCustomImageTemplate {
        id: row.id.clone(),
        environment_id: row.environment_id.clone(),
        provider: row.provider.clone(),
        template_kind: read_environment_custom_image_template_kind(Some(&row.template_kind)),
        template_ref: row.template_ref.clone(),
        source_template_ref: row.source_template_ref.clone(),
        source_environment_config_fingerprint: row.source_environment_config_fingerprint.clone(),
        status: row.status.clone(),
        created_by_user_id: row.created_by_user_id.clone(),
        created_by_agent_id: row.created_by_agent_id.clone(),
        captured_at: row.captured_at.clone(),
        last_used_at: row.last_used_at.clone(),
        superseded_by_template_id: row.superseded_by_template_id.clone(),
        metadata: row.metadata.clone(),
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}
