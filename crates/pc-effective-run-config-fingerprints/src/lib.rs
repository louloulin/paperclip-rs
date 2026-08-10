//! Effective run config fingerprints (session / workspace / lease).
//!
//! 1:1 port of Node `paperclip/server/src/services/effective-run-config-fingerprints.ts`.
//!
//! Given three potentially-mutating run configuration snapshots
//! (session / workspace / lease) and an optional secret manifest, this
//! crate computes a stable canonical JSON representation and a SHA-256
//! fingerprint for each category. The fingerprint can be used to detect
//! "did anything that materially affects the run change between two
//! states?" — sensitive values (secrets, tokens, passwords, …) are
//! redacted, volatile identifiers (runId, traceId, …) and host paths
//! (cwd, homeDir, …) are dropped before hashing, and PAPERCLIP_*
//! generated env vars are excluded from env canonicalization.
//!
//! Pure logic — no DB, no I/O. The crate exposes the canonical value
//! type as `serde_json::Value` so callers can serialise it back to JSON
//! without a custom intermediate type.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------
// Public constants (mirror Node exports)
// ---------------------------------------------------------------------

pub const EFFECTIVE_RUN_CONFIG_FINGERPRINT_VERSION: u32 = 1;
pub const EFFECTIVE_RUN_CONFIG_FINGERPRINT_ALGORITHM: &str = "sha256";
pub const EFFECTIVE_RUN_CONFIG_FINGERPRINT_CATEGORIES: [&str; 3] = ["session", "workspace", "lease"];

pub type EffectiveRunConfigFingerprintCategory = &'static str;
pub type EffectiveRunConfigChangedCategory = &'static str;

/// Recursive canonical value:
/// `null | bool | number | string | array | object`.
pub type EffectiveRunConfigCanonicalValue = Value;

// ---------------------------------------------------------------------
// Secret manifest
// ---------------------------------------------------------------------

/// Normalised secret metadata used to build the secret manifest index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveRunConfigSecretVersionMetadata {
    pub config_path: String,
    pub env_key: Option<String>,
    pub secret_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
    /// Either a number or a string. Captured as raw `Value` so we can
    /// round-trip both shapes.
    pub version: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_version_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Input shape accepted for secret manifest entries. We accept any
/// JSON object and read only the fields we need.
pub type EffectiveRunConfigSecretManifestEntry = Value;

#[derive(Default, Debug, Clone)]
struct SecretManifestIndex {
    by_config_path: BTreeMap<String, EffectiveRunConfigSecretVersionMetadata>,
    by_env_key: BTreeMap<String, EffectiveRunConfigSecretVersionMetadata>,
}

// ---------------------------------------------------------------------
// Public DTOs
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveRunConfigFingerprint {
    pub version: u32,
    pub category: String,
    pub algorithm: String,
    pub fingerprint: String,
    pub canonical_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveRunConfigFingerprints {
    pub version: u32,
    pub categories: Vec<String>,
    pub session_fingerprint: EffectiveRunConfigFingerprint,
    pub workspace_fingerprint: EffectiveRunConfigFingerprint,
    pub lease_fingerprint: EffectiveRunConfigFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveRunConfigFingerprintDiff {
    pub version: u32,
    pub has_changes: bool,
    pub changed_categories: Vec<String>,
    pub changed: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Default)]
pub struct EffectiveRunConfigFingerprintInput {
    pub session: Option<Value>,
    pub workspace: Option<Value>,
    pub lease: Option<Value>,
    pub secret_manifest: Option<Vec<EffectiveRunConfigSecretManifestEntry>>,
}

// ---------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------

/// Canonicalise the value of a single fingerprint category.
pub fn canonicalize_effective_run_config_category(input: CanonicalizeCategoryInput<'_>) -> Value {
    let secrets = build_secret_manifest_index(input.secret_manifest);
    let effective: &Value = match input.value {
        None | Some(Value::Null) => &Value::Object(Map::new()),
        Some(v) => v,
    };
    let canonical = canonicalize_value(
        effective,
        &CanonicalizeContext {
            category: input.category,
            path: Vec::new(),
            secrets: &secrets,
        },
    );
    match canonical {
        Some(v) => v,
        None => Value::Object(Map::new()),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CanonicalizeCategoryInput<'a> {
    pub category: EffectiveRunConfigFingerprintCategory,
    pub value: Option<&'a Value>,
    pub secret_manifest: Option<&'a [EffectiveRunConfigSecretManifestEntry]>,
}

/// Compute per-subcategory fingerprints under a parent category.
///
/// For each subcategory name, this extracts the corresponding value from
/// the canonicalized parent value (if present), wraps it in a
/// single-key object, and fingerprints that. Subcategories that are not
/// present in the canonical form get an empty-object fingerprint.
pub fn create_effective_run_config_subcategory_fingerprints<T: AsRef<str>>(
    input: SubcategoryInput<'_, T>,
) -> BTreeMap<String, String> {
    let category = input.category;
    let canonical_parent =
        canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category,
            value: Some(&input.value),
            secret_manifest: input.secret_manifest,
        });
    let record = canonical_record(&canonical_parent);
    let mut out = BTreeMap::new();
    for sub in input.subcategories {
        let sub_val: Value = if record.contains_key(sub.as_ref()) {
            let mut one = Map::new();
            one.insert(
                sub.as_ref().to_string(),
                record.get(sub.as_ref()).cloned().unwrap_or(Value::Null),
            );
            Value::Object(one)
        } else {
            Value::Object(Map::new())
        };
        let fp = create_category_fingerprint_from_canonical_value(category, &sub_val);
        out.insert(sub.as_ref().to_string(), fp.fingerprint);
    }
    out
}

#[derive(Debug, Clone)]
pub struct SubcategoryInput<'a, T: AsRef<str>> {
    pub category: EffectiveRunConfigFingerprintCategory,
    pub value: Value,
    pub subcategories: &'a [T],
    pub secret_manifest: Option<&'a [EffectiveRunConfigSecretManifestEntry]>,
}

/// Compute the three category fingerprints for a run config snapshot.
pub fn create_effective_run_config_fingerprints(
    input: &EffectiveRunConfigFingerprintInput,
) -> EffectiveRunConfigFingerprints {
    let manifest_slice = input.secret_manifest.as_deref();
    let session = create_category_fingerprint(CategoryFingerprintInput {
        category: "session",
        value: input.session.as_ref(),
        secret_manifest: manifest_slice,
    });
    let workspace = create_category_fingerprint(CategoryFingerprintInput {
        category: "workspace",
        value: input.workspace.as_ref(),
        secret_manifest: manifest_slice,
    });
    let lease = create_category_fingerprint(CategoryFingerprintInput {
        category: "lease",
        value: input.lease.as_ref(),
        secret_manifest: manifest_slice,
    });
    EffectiveRunConfigFingerprints {
        version: EFFECTIVE_RUN_CONFIG_FINGERPRINT_VERSION,
        categories: EFFECTIVE_RUN_CONFIG_FINGERPRINT_CATEGORIES
            .iter()
            .map(|s| s.to_string())
            .collect(),
        session_fingerprint: session,
        workspace_fingerprint: workspace,
        lease_fingerprint: lease,
    }
}

#[derive(Debug, Clone, Copy)]
struct CategoryFingerprintInput<'a> {
    category: EffectiveRunConfigFingerprintCategory,
    value: Option<&'a Value>,
    secret_manifest: Option<&'a [Value]>,
}

/// Diff two fingerprint sets: returns which categories changed.
pub fn diff_effective_run_config_fingerprints(
    previous: &EffectiveRunConfigFingerprints,
    next: &EffectiveRunConfigFingerprints,
) -> EffectiveRunConfigFingerprintDiff {
    let mut changed = BTreeMap::new();
    for cat in EFFECTIVE_RUN_CONFIG_FINGERPRINT_CATEGORIES {
        let prev = fingerprint_for_category(previous, cat);
        let curr = fingerprint_for_category(next, cat);
        changed.insert(cat.to_string(), prev != curr);
    }
    let changed_categories: Vec<String> = changed
        .iter()
        .filter_map(|(k, v)| if *v { Some(k.clone()) } else { None })
        .collect();
    EffectiveRunConfigFingerprintDiff {
        version: EFFECTIVE_RUN_CONFIG_FINGERPRINT_VERSION,
        has_changes: !changed_categories.is_empty(),
        changed_categories,
        changed,
    }
}

fn fingerprint_for_category<'a>(
    fps: &'a EffectiveRunConfigFingerprints,
    category: &'a str,
) -> &'a str {
    match category {
        "session" => &fps.session_fingerprint.fingerprint,
        "workspace" => &fps.workspace_fingerprint.fingerprint,
        "lease" => &fps.lease_fingerprint.fingerprint,
        other => panic!("unknown fingerprint category: {other}"),
    }
}

// ---------------------------------------------------------------------
// Implementation: fingerprints
// ---------------------------------------------------------------------

fn create_category_fingerprint(input: CategoryFingerprintInput<'_>) -> EffectiveRunConfigFingerprint {
    let canonical = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
        category: input.category,
        value: input.value,
        secret_manifest: input.secret_manifest,
    });
    create_category_fingerprint_from_canonical_value(input.category, &canonical)
}

fn create_category_fingerprint_from_canonical_value(
    category: EffectiveRunConfigFingerprintCategory,
    canonical_value: &Value,
) -> EffectiveRunConfigFingerprint {
    let envelope = serde_json::json!({
        "version": EFFECTIVE_RUN_CONFIG_FINGERPRINT_VERSION,
        "category": category,
        "value": canonical_value,
    });
    let canonical_json = stable_stringify(&envelope);
    let mut hasher = Sha256::new();
    hasher.update(canonical_json.as_bytes());
    let digest = hex::encode(hasher.finalize());
    let fingerprint = format!(
        "v{EFFECTIVE_RUN_CONFIG_FINGERPRINT_VERSION}:{EFFECTIVE_RUN_CONFIG_FINGERPRINT_ALGORITHM}:{digest}"
    );
    EffectiveRunConfigFingerprint {
        version: EFFECTIVE_RUN_CONFIG_FINGERPRINT_VERSION,
        category: category.to_string(),
        algorithm: EFFECTIVE_RUN_CONFIG_FINGERPRINT_ALGORITHM.to_string(),
        fingerprint,
        canonical_json,
    }
}

// ---------------------------------------------------------------------
// Implementation: canonicalization
// ---------------------------------------------------------------------

#[derive(Clone)]
struct CanonicalizeContext<'a> {
    category: EffectiveRunConfigFingerprintCategory,
    path: Vec<String>,
    secrets: &'a SecretManifestIndex,
}

/// Internal marker returned by helpers that may want to drop the
/// sub-tree entirely from the canonical form.
const OMIT: Option<Value> = None;

fn canonicalize_value(value: &Value, context: &CanonicalizeContext<'_>) -> Option<Value> {
    if value.is_null() {
        return Some(Value::Null);
    }
    if let Some(date_repr) = try_match_date(value) {
        // Node checks `value instanceof Date` — for JSON input we never
        // see Date objects, but we treat any string matching an ISO-8601
        // date as a date for the OMIT purposes. The Node check excludes
        // Date instances outright; we mirror by omitting values whose
        // raw type is `Date`. For our JSON-only world, that means we
        // never OMIT on this branch.
        let _ = date_repr;
    }
    if value.is_array() {
        let arr = value.as_array().unwrap();
        let mut out = Vec::with_capacity(arr.len());
        for (idx, entry) in arr.iter().enumerate() {
            let next = canonicalize_value(
                entry,
                &CanonicalizeContext {
                    path: {
                        let mut p = context.path.clone();
                        p.push(idx.to_string());
                        p
                    },
                    ..*context
                },
            );
            out.push(next.unwrap_or(Value::Null));
        }
        return Some(Value::Array(out));
    }
    if let Some(obj) = value.as_object() {
        if is_secret_ref_binding(value) {
            return Some(canonical_secret_ref_binding(
                obj,
                &context.path.join("."),
            ));
        }
        let mut canonical_object: Map<String, Value> = Map::new();
        for key in obj.keys() {
            if key == "env" {
                let env_value = canonicalize_env_record(obj.get(key).unwrap(), context);
                if let Some(env_obj) = env_value {
                    if let Some(env_map) = env_obj.as_object() {
                        if !env_map.is_empty() {
                            canonical_object.insert("env".to_string(), env_obj);
                        }
                    }
                }
                continue;
            }
            if should_omit_object_key(context.category, key) {
                continue;
            }
            if is_sensitive_config_key(key) {
                canonical_object.insert(key.to_string(), redacted_value());
                continue;
            }
            let next = canonicalize_value(
                obj.get(key).unwrap(),
                &CanonicalizeContext {
                    path: {
                        let mut p = context.path.clone();
                        p.push(key.clone());
                        p
                    },
                    ..*context
                },
            );
            if let Some(v) = next {
                canonical_object.insert(key.to_string(), v);
            }
        }
        return Some(if canonical_object.is_empty() {
            Value::Object(Map::new())
        } else {
            Value::Object(canonical_object)
        });
    }
    match value {
        Value::Bool(b) => Some(Value::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Value::Number(serde_json::Number::from(i)))
            } else if let Some(u) = n.as_u64() {
                Some(Value::Number(serde_json::Number::from(u)))
            } else if let Some(f) = n.as_f64() {
                if f.is_finite() {
                    serde_json::Number::from_f64(f).map(Value::Number)
                } else {
                    Some(Value::Null)
                }
            } else {
                Some(Value::Null)
            }
        }
        Value::String(s) => Some(Value::String(s.clone())),
        _ => Some(Value::Null),
    }
}

/// `Date` instances in Node are dropped by `instanceof Date` check; for
/// our JSON-only world this is a no-op. We keep the function so the
/// canonicalize pipeline remains a faithful 1:1 port.
fn try_match_date(_value: &Value) -> Option<()> {
    None
}

fn redacted_value() -> Value {
    serde_json::json!({"type": "redacted", "present": true})
}

fn canonicalize_env_record(
    env_value: &Value,
    context: &CanonicalizeContext<'_>,
) -> Option<Value> {
    let env_obj = match env_value.as_object() {
        Some(o) => o,
        None => return OMIT,
    };
    let mut canonical_env: BTreeMap<String, Value> = BTreeMap::new();
    for key in env_obj.keys() {
        if is_generated_runtime_env_key(key) {
            continue;
        }
        let manifest_entry = context
            .secrets
            .by_config_path
            .get(&format!("env.{key}"))
            .cloned()
            .or_else(|| context.secrets.by_env_key.get(key).cloned());
        if let Some(entry) = manifest_entry {
            canonical_env.insert(key.to_string(), canonical_secret_metadata(&entry));
            continue;
        }
        let raw_binding = env_obj.get(key).unwrap();
        if is_secret_ref_binding(raw_binding) {
            canonical_env.insert(
                key.to_string(),
                canonical_secret_ref_binding(
                    raw_binding.as_object().unwrap(),
                    &format!("env.{key}"),
                ),
            );
            continue;
        }
        let present = !raw_binding.is_null();
        let value_hash = hash_plain_env_value(raw_binding);
        let mut entry_obj = Map::new();
        entry_obj.insert("type".to_string(), Value::String("plain_env".to_string()));
        entry_obj.insert("present".to_string(), Value::Bool(present));
        if let Some(hash) = value_hash {
            entry_obj.insert("valueHash".to_string(), Value::String(hash));
        }
        canonical_env.insert(key.to_string(), Value::Object(entry_obj));
    }
    if canonical_env.is_empty() {
        return OMIT;
    }
    Some(Value::Object(
        canonical_env
            .into_iter()
            .collect::<Map<String, Value>>(),
    ))
}

fn canonical_secret_metadata(entry: &EffectiveRunConfigSecretVersionMetadata) -> Value {
    let mut out = Map::new();
    out.insert("type".to_string(), Value::String("secret_metadata".to_string()));
    out.insert("configPath".to_string(), Value::String(entry.config_path.clone()));
    if let Some(env_key) = &entry.env_key {
        out.insert("envKey".to_string(), Value::String(env_key.clone()));
    }
    out.insert("secretId".to_string(), Value::String(entry.secret_id.clone()));
    out.insert("version".to_string(), entry.version.clone());
    if let Some(b) = &entry.binding_id {
        out.insert("bindingId".to_string(), Value::String(b.clone()));
    }
    if let Some(p) = &entry.provider {
        out.insert("provider".to_string(), Value::String(p.clone()));
    }
    if let Some(r) = &entry.provider_version_ref {
        out.insert("providerVersionRef".to_string(), Value::String(r.clone()));
    }
    if let Some(o) = &entry.outcome {
        out.insert("outcome".to_string(), Value::String(o.clone()));
    }
    Value::Object(out)
}

fn canonical_secret_ref_binding(value: &Map<String, Value>, config_path: &str) -> Value {
    let mut out = Map::new();
    out.insert("type".to_string(), Value::String("secret_ref".to_string()));
    out.insert("configPath".to_string(), Value::String(config_path.to_string()));
    if let Some(secret_id) = read_string(value.get("secretId")) {
        out.insert("secretId".to_string(), Value::String(secret_id));
    }
    let version = read_version(value.get("version")).unwrap_or(Value::String("latest".to_string()));
    out.insert("versionSelector".to_string(), version);
    out.insert("unresolved".to_string(), Value::Bool(true));
    Value::Object(out)
}

fn is_secret_ref_binding(value: &Value) -> bool {
    if let Some(obj) = value.as_object() {
        if obj.get("type").and_then(|v| v.as_str()) == Some("secret_ref") {
            if let Some(sid) = obj.get("secretId").and_then(|v| v.as_str()) {
                return !sid.trim().is_empty();
            }
        }
    }
    false
}

// ---------------------------------------------------------------------
// Implementation: secret manifest index
// ---------------------------------------------------------------------

fn build_secret_manifest_index(
    manifest: Option<&[Value]>,
) -> SecretManifestIndex {
    let mut idx = SecretManifestIndex::default();
    let Some(manifest) = manifest else {
        return idx;
    };
    for entry in manifest {
        if let Some(normalized) = normalize_secret_manifest_entry(entry) {
            if !normalized.config_path.is_empty() {
                idx.by_config_path
                    .insert(normalized.config_path.clone(), normalized.clone());
            }
            if let Some(env_key) = &normalized.env_key {
                idx.by_env_key
                    .insert(env_key.clone(), normalized.clone());
            }
        }
    }
    idx
}

fn normalize_secret_manifest_entry(
    entry: &Value,
) -> Option<EffectiveRunConfigSecretVersionMetadata> {
    let obj = entry.as_object()?;
    let secret_id = read_string(obj.get("secretId"))?;
    let version = read_version(obj.get("version"))?;
    let mut normalized = EffectiveRunConfigSecretVersionMetadata {
        config_path: read_string(obj.get("configPath")).unwrap_or_default(),
        env_key: read_string(obj.get("envKey")),
        secret_id,
        binding_id: None,
        version,
        provider: None,
        provider_version_ref: None,
        outcome: None,
    };
    if let Some(b) = read_string(obj.get("bindingId")) {
        normalized.binding_id = Some(b);
    }
    if let Some(p) = read_string(obj.get("provider")) {
        normalized.provider = Some(p);
    }
    if let Some(pvr) = read_string(obj.get("providerVersionRef")) {
        normalized.provider_version_ref = Some(pvr);
    }
    match obj.get("outcome").and_then(|v| v.as_str()) {
        Some("success") | Some("failure") => {
            normalized.outcome = Some(obj.get("outcome").unwrap().as_str().unwrap().to_string());
        }
        _ => {}
    }
    Some(normalized)
}

// ---------------------------------------------------------------------
// Implementation: omission + sensitivity rules
// ---------------------------------------------------------------------

fn is_generated_runtime_env_key(key: &str) -> bool {
    key.starts_with("PAPERCLIP_")
}

static SENSITIVE_CONFIG_KEY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:api[_-]?key|access[_-]?token|auth(?:orization)?|bearer|cookie|credential|jwt|password|passwd|private[_-]?key|secret|token)$")
        .expect("SENSITIVE_CONFIG_KEY_RE")
});

fn is_sensitive_config_key(key: &str) -> bool {
    SENSITIVE_CONFIG_KEY_RE.is_match(key)
}

fn should_omit_object_key(category: EffectiveRunConfigFingerprintCategory, key: &str) -> bool {
    if VOLATILE_CONFIG_KEYS.contains(key) {
        return true;
    }
    if HOST_NOISE_KEYS.contains(key) {
        return true;
    }
    if is_timestamp_noise_key(key) {
        return true;
    }
    if category == "session" && SESSION_HOST_PATH_KEYS.contains(key) {
        return true;
    }
    if category == "lease" && (key == "remoteCwd" || key == "workspaceRemoteDir") {
        return true;
    }
    false
}

static VOLATILE_CONFIG_KEYS: Lazy<BTreeSet<&'static str>> = Lazy::new(|| {
    [
        "checkoutRunId",
        "executionRunId",
        "externalRunId",
        "heartbeatRunId",
        "invocationId",
        "leaseId",
        "providerLeaseId",
        "requestId",
        "runId",
        "sessionDisplayId",
        "sessionId",
        "spanId",
        "traceId",
    ]
    .into_iter()
    .collect()
});

static HOST_NOISE_KEYS: Lazy<BTreeSet<&'static str>> = Lazy::new(|| {
    [
        "agentHome",
        "homeDir",
        "hostCwd",
        "localHome",
        "tempDir",
        "tmpDir",
        "userHome",
    ]
    .into_iter()
    .collect()
});

static SESSION_HOST_PATH_KEYS: Lazy<BTreeSet<&'static str>> = Lazy::new(|| {
    [
        "cwd",
        "localPath",
        "remoteCwd",
        "workspaceCwd",
        "workspacePath",
        "workspaceRemoteDir",
        "worktreePath",
    ]
    .into_iter()
    .collect()
});

static TIMESTAMP_NOISE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(created|updated|started|finished|completed|cancelled|resolved|used|heartbeat)At$")
        .expect("TIMESTAMP_NOISE_RE")
});
static TIMESTAMP_NOISE_NEG_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(revision|version)").expect("TIMESTAMP_NOISE_NEG_RE"));

fn is_timestamp_noise_key(key: &str) -> bool {
    TIMESTAMP_NOISE_RE.is_match(key) && !TIMESTAMP_NOISE_NEG_RE.is_match(key)
}

// ---------------------------------------------------------------------
// Implementation: hashing + canonical JSON
// ---------------------------------------------------------------------

fn hash_plain_env_value(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let canonical = canonicalize_plain_env_value_for_hash(value);
    let canonical_json = stable_stringify(&canonical);
    let mut hasher = Sha256::new();
    hasher.update(canonical_json.as_bytes());
    let digest = hex::encode(hasher.finalize());
    Some(format!("sha256:{digest}"))
}

fn canonicalize_plain_env_value_for_hash(value: &Value) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    if value.is_array() {
        let arr = value.as_array().unwrap();
        return Value::Array(
            arr.iter()
                .map(canonicalize_plain_env_value_for_hash)
                .collect(),
        );
    }
    if let Some(obj) = value.as_object() {
        let mut sorted_keys: Vec<&String> = obj.keys().collect();
        sorted_keys.sort();
        let mut out = Map::new();
        for key in sorted_keys {
            let next = canonicalize_plain_env_value_for_hash(obj.get(key).unwrap());
            if !next.is_null() {
                out.insert(key.clone(), next);
            }
        }
        return Value::Object(out);
    }
    if let Some(n) = value.as_number() {
        if let Some(i) = n.as_i64() {
            return Value::Number(serde_json::Number::from(i));
        }
        if let Some(u) = n.as_u64() {
            return Value::Number(serde_json::Number::from(u));
        }
        if let Some(f) = n.as_f64() {
            if f.is_finite() {
                if let Some(num) = serde_json::Number::from_f64(f) {
                    return Value::Number(num);
                }
            }
            return Value::Null;
        }
    }
    if let Some(s) = value.as_str() {
        return Value::String(s.to_string());
    }
    if let Some(b) = value.as_bool() {
        return Value::Bool(b);
    }
    Value::Null
}

/// Canonical JSON: sorts object keys, recursively visits arrays, returns
/// raw JSON otherwise.
fn stable_stringify(value: &Value) -> String {
    match value {
        Value::Array(arr) => {
            let mut buf = String::from("[");
            for (i, v) in arr.iter().enumerate() {
                if i > 0 {
                    buf.push(',');
                }
                buf.push_str(&stable_stringify(v));
            }
            buf.push(']');
            buf
        }
        Value::Object(obj) => {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            let mut buf = String::from("{");
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    buf.push(',');
                }
                buf.push_str(&serde_json::to_string(k).unwrap());
                buf.push(':');
                let v = obj.get(*k).unwrap();
                buf.push_str(&stable_stringify(v));
            }
            buf.push('}');
            buf
        }
        other => serde_json::to_string(other).unwrap(),
    }
}

// ---------------------------------------------------------------------
// Implementation: misc helpers
// ---------------------------------------------------------------------

fn read_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        _ => None,
    }
}

fn read_version(value: Option<&Value>) -> Option<Value> {
    match value? {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Value::Number(serde_json::Number::from(i)))
            } else if let Some(u) = n.as_u64() {
                Some(Value::Number(serde_json::Number::from(u)))
            } else if let Some(f) = n.as_f64() {
                if f.is_finite() {
                    serde_json::Number::from_f64(f).map(Value::Number)
                } else {
                    None
                }
            } else {
                None
            }
        }
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(Value::String(t.to_string()))
            }
        }
        _ => None,
    }
}

fn canonical_record(value: &Value) -> Map<String, Value> {
    match value {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -------- stable_stringify --------

    #[test]
    fn stable_stringify_sorts_object_keys() {
        let v = json!({"b": 1, "a": 2, "c": 3});
        assert_eq!(stable_stringify(&v), r#"{"a":2,"b":1,"c":3}"#);
    }

    #[test]
    fn stable_stringify_nested_sorting() {
        let v = json!({"z": {"y": 1, "x": 2}, "a": [{"d": 1, "c": 2}]});
        assert_eq!(
            stable_stringify(&v),
            r#"{"a":[{"c":2,"d":1}],"z":{"x":2,"y":1}}"#
        );
    }

    // -------- canonicalize: empty / null --------

    #[test]
    fn canonicalize_empty_value_yields_empty_object() {
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&json!({})),
            secret_manifest: None,
        });
        assert_eq!(v, json!({}));
    }

    #[test]
    fn canonicalize_null_value_yields_empty_object() {
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&Value::Null),
            secret_manifest: None,
        });
        assert_eq!(v, json!({}));
    }

    #[test]
    fn canonicalize_missing_value_yields_empty_object() {
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "workspace",
            value: None,
            secret_manifest: None,
        });
        assert_eq!(v, json!({}));
    }

    // -------- canonicalize: redaction + omission --------

    #[test]
    fn sensitive_keys_are_redacted() {
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&json!({"apiKey": "sk-abc", "name": "ok", "Authorization": "Bearer xyz", "token": "t"})),
            secret_manifest: None,
        });
        assert_eq!(v["apiKey"], json!({"type": "redacted", "present": true}));
        assert_eq!(v["Authorization"], json!({"type": "redacted", "present": true}));
        assert_eq!(v["token"], json!({"type": "redacted", "present": true}));
        assert_eq!(v["name"], json!("ok"));
    }

    #[test]
    fn volatile_keys_are_omitted() {
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&json!({"runId": "abc", "traceId": "t1", "keep": 1})),
            secret_manifest: None,
        });
        assert!(v.get("runId").is_none());
        assert!(v.get("traceId").is_none());
        assert_eq!(v["keep"], json!(1));
    }

    #[test]
    fn host_noise_keys_are_omitted() {
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&json!({"homeDir": "/home/x", "tmpDir": "/tmp", "agentHome": "/a", "keep": 1})),
            secret_manifest: None,
        });
        assert!(v.get("homeDir").is_none());
        assert!(v.get("tmpDir").is_none());
        assert!(v.get("agentHome").is_none());
        assert_eq!(v["keep"], json!(1));
    }

    #[test]
    fn timestamp_noise_keys_are_omitted() {
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&json!({"createdAt": "2024-01-01", "updatedAt": "t", "finishedAt": "t", "revision": 7, "version": "1.0"})),
            secret_manifest: None,
        });
        assert!(v.get("createdAt").is_none());
        assert!(v.get("updatedAt").is_none());
        assert!(v.get("finishedAt").is_none());
        // version-style suffix is preserved
        assert_eq!(v["revision"], json!(7));
        assert_eq!(v["version"], json!("1.0"));
    }

    #[test]
    fn session_host_path_keys_are_omitted_for_session_only() {
        let session = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&json!({"cwd": "/x", "localPath": "/y", "keep": 1})),
            secret_manifest: None,
        });
        assert!(session.get("cwd").is_none());
        assert!(session.get("localPath").is_none());
        assert_eq!(session["keep"], json!(1));

        let lease = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "lease",
            value: Some(&json!({"cwd": "/x", "localPath": "/y", "keep": 1})),
            secret_manifest: None,
        });
        // cwd and localPath are only omitted in the session category
        assert_eq!(lease["cwd"], json!("/x"));
        assert_eq!(lease["localPath"], json!("/y"));
    }

    #[test]
    fn lease_remote_paths_omitted() {
        let lease = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "lease",
            value: Some(&json!({"remoteCwd": "/r", "workspaceRemoteDir": "/w", "cwd": "/x"})),
            secret_manifest: None,
        });
        assert!(lease.get("remoteCwd").is_none());
        assert!(lease.get("workspaceRemoteDir").is_none());
        assert_eq!(lease["cwd"], json!("/x"));
    }

    // -------- canonicalize: secret ref bindings --------

    #[test]
    fn secret_ref_binding_is_canonicalized_in_place() {
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&json!({"creds": {"type": "secret_ref", "secretId": "s1", "version": 3}})),
            secret_manifest: None,
        });
        assert_eq!(
            v["creds"],
            json!({
                "type": "secret_ref",
                "configPath": "creds",
                "secretId": "s1",
                "versionSelector": 3,
                "unresolved": true
            })
        );
    }

    #[test]
    fn secret_ref_binding_falls_back_to_latest_when_version_missing() {
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&json!({"creds": {"type": "secret_ref", "secretId": "s1"}})),
            secret_manifest: None,
        });
        assert_eq!(v["creds"]["versionSelector"], json!("latest"));
    }

    // -------- canonicalize: env record --------

    #[test]
    fn env_canonicalization_drops_generated_keys() {
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&json!({"env": {"PAPERCLIP_INTERNAL": "x", "FOO": "bar"}})),
            secret_manifest: None,
        });
        assert!(v["env"].get("PAPERCLIP_INTERNAL").is_none());
        assert_eq!(v["env"]["FOO"]["type"], json!("plain_env"));
        assert_eq!(v["env"]["FOO"]["present"], json!(true));
        assert!(v["env"]["FOO"]["valueHash"].is_string());
    }

    #[test]
    fn env_canonicalization_uses_manifest_metadata() {
        let manifest = vec![json!({
            "configPath": "env.API_TOKEN",
            "envKey": "API_TOKEN",
            "secretId": "s-api",
            "version": 7,
            "provider": "vault",
            "outcome": "success"
        })];
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&json!({"env": {"API_TOKEN": "raw"}})),
            secret_manifest: Some(&manifest),
        });
        assert_eq!(v["env"]["API_TOKEN"]["type"], json!("secret_metadata"));
        assert_eq!(v["env"]["API_TOKEN"]["secretId"], json!("s-api"));
        assert_eq!(v["env"]["API_TOKEN"]["version"], json!(7));
        assert_eq!(v["env"]["API_TOKEN"]["provider"], json!("vault"));
        assert_eq!(v["env"]["API_TOKEN"]["outcome"], json!("success"));
    }

    #[test]
    fn env_canonicalization_resolves_secret_ref_binding() {
        let v = canonicalize_effective_run_config_category(CanonicalizeCategoryInput {
            category: "session",
            value: Some(&json!({"env": {"FOO": {"type": "secret_ref", "secretId": "s1"}}})),
            secret_manifest: None,
        });
        assert_eq!(v["env"]["FOO"]["type"], json!("secret_ref"));
        assert_eq!(v["env"]["FOO"]["configPath"], json!("env.FOO"));
        assert_eq!(v["env"]["FOO"]["secretId"], json!("s1"));
    }

    // -------- fingerprints --------

    #[test]
    fn fingerprint_is_stable_for_same_input() {
        let input = EffectiveRunConfigFingerprintInput {
            session: Some(json!({"name": "s1", "v": 1})),
            ..Default::default()
        };
        let a = create_effective_run_config_fingerprints(&input);
        let b = create_effective_run_config_fingerprints(&input);
        assert_eq!(a.session_fingerprint.fingerprint, b.session_fingerprint.fingerprint);
        assert!(a.session_fingerprint.fingerprint.starts_with("v1:sha256:"));
    }

    #[test]
    fn fingerprint_changes_when_value_changes() {
        let mut a = EffectiveRunConfigFingerprintInput::default();
        a.session = Some(json!({"k": 1}));
        let mut b = EffectiveRunConfigFingerprintInput::default();
        b.session = Some(json!({"k": 2}));
        let fa = create_effective_run_config_fingerprints(&a);
        let fb = create_effective_run_config_fingerprints(&b);
        assert_ne!(fa.session_fingerprint.fingerprint, fb.session_fingerprint.fingerprint);
    }

    #[test]
    fn fingerprint_ignores_object_key_order() {
        let mut a = EffectiveRunConfigFingerprintInput::default();
        a.session = Some(json!({"a": 1, "b": 2}));
        let mut b = EffectiveRunConfigFingerprintInput::default();
        b.session = Some(json!({"b": 2, "a": 1}));
        let fa = create_effective_run_config_fingerprints(&a);
        let fb = create_effective_run_config_fingerprints(&b);
        assert_eq!(fa.session_fingerprint.fingerprint, fb.session_fingerprint.fingerprint);
    }

    #[test]
    fn fingerprint_redacts_secrets() {
        let mut a = EffectiveRunConfigFingerprintInput::default();
        a.session = Some(json!({"apiKey": "secret-a"}));
        let mut b = EffectiveRunConfigFingerprintInput::default();
        b.session = Some(json!({"apiKey": "secret-b"}));
        let fa = create_effective_run_config_fingerprints(&a);
        let fb = create_effective_run_config_fingerprints(&b);
        assert_eq!(fa.session_fingerprint.fingerprint, fb.session_fingerprint.fingerprint);
    }

    #[test]
    fn fingerprint_omits_volatile_keys() {
        let mut a = EffectiveRunConfigFingerprintInput::default();
        a.session = Some(json!({"runId": "abc", "name": "x"}));
        let mut b = EffectiveRunConfigFingerprintInput::default();
        b.session = Some(json!({"runId": "xyz", "name": "x"}));
        let fa = create_effective_run_config_fingerprints(&a);
        let fb = create_effective_run_config_fingerprints(&b);
        assert_eq!(fa.session_fingerprint.fingerprint, fb.session_fingerprint.fingerprint);
    }

    #[test]
    fn fingerprint_canonical_json_is_stable() {
        let input = EffectiveRunConfigFingerprintInput {
            session: Some(json!({"z": 1, "a": {"y": 2, "x": 3}})),
            ..Default::default()
        };
        let f = create_effective_run_config_fingerprints(&input);
        assert!(f.session_fingerprint.canonical_json.contains(r#""a":{"x":3,"y":2}"#));
        assert!(f.session_fingerprint.canonical_json.contains(r#""z":1"#));
    }

    #[test]
    fn categories_use_canonical_names() {
        let input = EffectiveRunConfigFingerprintInput {
            session: Some(json!({"a": 1})),
            workspace: Some(json!({"b": 2})),
            lease: Some(json!({"c": 3})),
            secret_manifest: None,
        };
        let f = create_effective_run_config_fingerprints(&input);
        assert_eq!(f.session_fingerprint.category, "session");
        assert_eq!(f.workspace_fingerprint.category, "workspace");
        assert_eq!(f.lease_fingerprint.category, "lease");
        assert_eq!(f.categories, vec!["session", "workspace", "lease"]);
    }

    // -------- subcategory fingerprints --------

    #[test]
    fn subcategory_fingerprints_extract_each_subkey() {
        let value = json!({
            "alpha": {"x": 1},
            "beta": {"y": 2},
        });
        let fps = create_effective_run_config_subcategory_fingerprints(SubcategoryInput {
            category: "session",
            value: value.clone(),
            subcategories: &["alpha", "beta", "missing"],
            secret_manifest: None,
        });
        assert!(fps.contains_key("alpha"));
        assert!(fps.contains_key("beta"));
        assert!(fps.contains_key("missing"));
        // alpha and beta fingerprints must differ
        assert_ne!(fps["alpha"], fps["beta"]);
        // missing gets the empty-object fingerprint
        let empty_fp = create_effective_run_config_subcategory_fingerprints(SubcategoryInput {
            category: "session",
            value: json!({}),
            subcategories: &["missing"],
            secret_manifest: None,
        });
        assert_eq!(fps["missing"], empty_fp["missing"]);
    }

    // -------- diff --------

    #[test]
    fn diff_detects_no_changes_when_identical() {
        let input = EffectiveRunConfigFingerprintInput {
            session: Some(json!({"a": 1})),
            workspace: Some(json!({"b": 2})),
            lease: Some(json!({"c": 3})),
            secret_manifest: None,
        };
        let a = create_effective_run_config_fingerprints(&input);
        let b = create_effective_run_config_fingerprints(&input);
        let d = diff_effective_run_config_fingerprints(&a, &b);
        assert!(!d.has_changes);
        assert!(d.changed_categories.is_empty());
    }

    #[test]
    fn diff_detects_session_change_only() {
        let mut a = EffectiveRunConfigFingerprintInput {
            session: Some(json!({"a": 1})),
            workspace: Some(json!({"b": 2})),
            lease: Some(json!({"c": 3})),
            secret_manifest: None,
        };
        let mut b = a.clone();
        b.session = Some(json!({"a": 999}));
        let fa = create_effective_run_config_fingerprints(&a);
        let fb = create_effective_run_config_fingerprints(&b);
        let d = diff_effective_run_config_fingerprints(&fa, &fb);
        assert!(d.has_changes);
        assert_eq!(d.changed_categories, vec!["session".to_string()]);
        assert!(!d.changed["workspace"]);
        assert!(!d.changed["lease"]);
    }

    #[test]
    fn diff_detects_multiple_category_changes() {
        let mut a = EffectiveRunConfigFingerprintInput::default();
        a.session = Some(json!({"a": 1}));
        let mut b = EffectiveRunConfigFingerprintInput::default();
        b.workspace = Some(json!({"b": 1}));
        b.lease = Some(json!({"c": 1}));
        let fa = create_effective_run_config_fingerprints(&a);
        let fb = create_effective_run_config_fingerprints(&b);
        let d = diff_effective_run_config_fingerprints(&fa, &fb);
        assert!(d.has_changes);
        assert_eq!(d.changed_categories.len(), 3);
    }

    // -------- canonicalize_plain_env_value_for_hash --------

    #[test]
    fn plain_env_value_canonicalization_drops_nulls_and_sorts() {
        let v = json!({"b": 1, "a": null, "c": "x"});
        let canonical = canonicalize_plain_env_value_for_hash(&v);
        // nulls are dropped, keys sorted
        assert_eq!(canonical, json!({"b": 1, "c": "x"}));
    }

    #[test]
    fn hash_plain_env_value_returns_sha256_prefix() {
        let h = hash_plain_env_value(&json!("v")).unwrap();
        assert!(h.starts_with("sha256:"));
        assert_eq!(h.len(), "sha256:".len() + 64);
    }

    #[test]
    fn hash_plain_env_value_is_none_for_null() {
        assert!(hash_plain_env_value(&Value::Null).is_none());
    }

    // -------- secret manifest index --------

    #[test]
    fn secret_manifest_index_lookup_by_env_key() {
        let manifest = vec![json!({
            "configPath": "env.API",
            "envKey": "API",
            "secretId": "s1",
            "version": 1
        })];
        let idx = build_secret_manifest_index(Some(&manifest));
        assert!(idx.by_env_key.contains_key("API"));
        assert!(idx.by_config_path.contains_key("env.API"));
    }

    #[test]
    fn secret_manifest_index_skips_invalid_entries() {
        let manifest = vec![
            json!({"secretId": "", "version": 1}),
            json!({"secretId": "ok", "version": null}),
            json!({"secretId": "ok", "version": 2, "configPath": "x.y"}),
        ];
        let idx = build_secret_manifest_index(Some(&manifest));
        assert_eq!(idx.by_config_path.len(), 1);
        assert!(idx.by_config_path.contains_key("x.y"));
    }

    // -------- serde roundtrip --------

    #[test]
    fn fingerprint_dto_serializes_camel_case() {
        let input = EffectiveRunConfigFingerprintInput {
            session: Some(json!({"a": 1})),
            ..Default::default()
        };
        let f = create_effective_run_config_fingerprints(&input);
        let v = serde_json::to_value(&f).unwrap();
        assert!(v["sessionFingerprint"]["fingerprint"].is_string());
        assert!(v["sessionFingerprint"]["canonicalJson"].is_string());
        assert!(v["sessionFingerprint"]["algorithm"].is_string());
    }
}
