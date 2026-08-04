//! `PAPERCLIP_MANAGED_CONFIG` 解析器（与 Node `managed-config.ts` 的
//! `parseManagedConfigEnv` / `getManagedInstanceConfig` 1:1 对齐）。
//!
//! ## 失败语义
//! - env var **缺失** → 返回 `None`（self-hosted）
//! - env var 存在但 **空** / **非 JSON** / **字段错误** / **未支持 version**
//!   → 抛出 `ManagedConfigError`，managed instance 拒绝启动（fail closed）
//! - `features` / `plugins.autoInstall` 缺失 → 抛错（防止 truncated doc 静默）
//! - `environments` 缺失 → OK（pre-section 文档必须仍可启动新 build）
//! - `environments` 中 provider 不在 `autoInstall` → 抛错
//! - `environments[].config` 含 secret-like key → 抛错

use std::collections::HashMap;
use std::sync::Mutex;

use super::secrets::find_secret_like_config_key;
use super::types::{
    ManagedConfigEnv, ManagedEnvironmentSpec, ManagedInstanceConfig, MANAGED_CONFIG_ENV_KEY,
    SUPPORTED_MANAGED_CONFIG_VERSION,
};
use crate::feature_catalog::{is_managed, InstanceFeatureKey};

// ============================================================================
// ManagedConfigError
// ============================================================================

/// Managed-config 解析错误（与 Node throw 1:1 对齐）。
#[derive(Debug, thiserror::Error)]
pub enum ManagedConfigError {
    #[error("{MANAGED_CONFIG_ENV_KEY} {detail}")]
    Parse { detail: String },
}

impl ManagedConfigError {
    fn parse(detail: impl Into<String>) -> Self {
        Self::Parse {
            detail: detail.into(),
        }
    }
}

// ============================================================================
// Cache
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheEntry {
    raw: Option<String>,
    config: Option<ManagedInstanceConfig>,
}

static CACHE: std::sync::OnceLock<Mutex<CacheEntry>> = std::sync::OnceLock::new();

fn cache() -> &'static Mutex<CacheEntry> {
    CACHE.get_or_init(|| {
        Mutex::new(CacheEntry {
            raw: None,
            config: None,
        })
    })
}

fn reset_cache() {
    let mut entry = cache().lock().expect("cache lock poisoned");
    entry.raw = None;
    entry.config = None;
}

// ============================================================================
// Helpers
// ============================================================================

fn fail(detail: impl Into<String>) -> ManagedConfigError {
    ManagedConfigError::parse(detail)
}

fn is_plain_object(value: &serde_json::Value) -> bool {
    value.is_object()
}

fn describe_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Array(_) => "an array".to_string(),
        other => other.to_string(),
    }
}

// ============================================================================
// parse_managed_config_env
// ============================================================================

/// 从 raw env map 解析 `PAPERCLIP_MANAGED_CONFIG` 文档（与 Node
/// `parseManagedConfigEnv(env)` 1:1 对齐）。
///
/// - env var 缺失 → 返回 `None`
/// - env var 存在但空 / 任何字段错误 → 抛 `ManagedConfigError`
pub fn parse_managed_config_env(
    env: ManagedConfigEnv<'_>,
) -> Result<Option<ManagedInstanceConfig>, ManagedConfigError> {
    let raw = env.get(MANAGED_CONFIG_ENV_KEY);
    if raw.is_none() {
        return Ok(None);
    }
    let raw_str = raw.unwrap();
    if raw_str.trim().is_empty() {
        return Err(fail(
            "is set but blank; a managed instance requires the full JSON document (unset the variable entirely for self-hosted mode)",
        ));
    }

    // 1) JSON parse
    let doc: serde_json::Value =
        serde_json::from_str(raw_str).map_err(|err| fail(format!("is not valid JSON: {}", err)))?;

    if !is_plain_object(&doc) {
        return Err(fail(format!(
            "must be a JSON object (got {})",
            describe_json_value(&doc)
        )));
    }

    // 2) Top-level keys whitelist
    let allowed_top_level_keys: std::collections::HashSet<&str> = [
        "v",
        "mode",
        "catalogVersion",
        "features",
        "plugins",
        "environments",
    ]
    .iter()
    .copied()
    .collect();
    let doc_obj = doc.as_object().unwrap();
    for key in doc_obj.keys() {
        if !allowed_top_level_keys.contains(key.as_str()) {
            return Err(fail(format!(
                "has unknown top-level key \"{}\" (allowed: v, mode, catalogVersion, features, plugins, environments)",
                key
            )));
        }
    }

    // 3) v + mode + catalogVersion
    let v = doc_obj
        .get("v")
        .ok_or_else(|| fail("requires a \"v\" field"))?;
    let v_u64 = match v {
        serde_json::Value::Number(n) => n.as_u64().ok_or_else(|| {
            fail(format!(
                "has unsupported \"v\" {}; this build supports v={}",
                describe_json_value(v),
                SUPPORTED_MANAGED_CONFIG_VERSION
            ))
        })?,
        _ => {
            return Err(fail(format!(
                "has unsupported \"v\" {}; this build supports v={}",
                describe_json_value(v),
                SUPPORTED_MANAGED_CONFIG_VERSION
            )));
        }
    };
    if v_u64 != SUPPORTED_MANAGED_CONFIG_VERSION as u64 {
        return Err(fail(format!(
            "has unsupported \"v\" {}; this build supports v={}",
            describe_json_value(v),
            SUPPORTED_MANAGED_CONFIG_VERSION
        )));
    }

    let mode = doc_obj
        .get("mode")
        .ok_or_else(|| fail("requires a \"mode\" field"))?;
    if mode.as_str() != Some("cloud") {
        return Err(fail(format!(
            "has invalid \"mode\" {}; expected \"cloud\"",
            describe_json_value(mode)
        )));
    }

    let catalog_version = doc_obj
        .get("catalogVersion")
        .ok_or_else(|| fail("requires a \"catalogVersion\" field"))?;
    let catalog_version_str = catalog_version.as_str().ok_or_else(|| {
        fail(format!(
            "requires a non-empty string \"catalogVersion\" (got {})",
            describe_json_value(catalog_version)
        ))
    })?;
    if catalog_version_str.trim().is_empty() {
        return Err(fail(format!(
            "requires a non-empty string \"catalogVersion\" (got {})",
            describe_json_value(catalog_version)
        )));
    }

    // 4) features
    let features_value = doc_obj.get("features").ok_or_else(|| {
        fail("requires a \"features\" object mapping feature key → boolean (use {} for none)")
    })?;
    if !is_plain_object(features_value) {
        return Err(fail(format!(
            "\"features\" must be an object mapping feature key → boolean (got {})",
            describe_json_value(features_value)
        )));
    }
    let features_obj = features_value.as_object().unwrap();
    let mut features: HashMap<InstanceFeatureKey, bool> = HashMap::new();
    for (key, value) in features_obj {
        let feature_key = InstanceFeatureKey::parse(key).ok_or_else(|| {
            fail(format!(
                "\"features\" has unknown feature key \"{}\"; known keys are the boolean flags of instanceExperimentalSettingsSchema",
                key
            ))
        })?;
        // Catalog-compatibility enforcement: tier must be "managed"
        if !is_managed(feature_key) {
            return Err(fail(format!(
                "\"features\" key \"{}\" has tier \"{}\" in this build's feature catalog; only tier \"managed\" keys may be set by a managed-config document (catalogVersion {} is incompatible with this build)",
                key,
                crate::feature_catalog::tier_of(feature_key)
                    .map(|t| t.as_str())
                    .unwrap_or("unknown"),
                catalog_version_str
            )));
        }
        let value_bool = value.as_bool().ok_or_else(|| {
            fail(format!(
                "\"features.{}\" must be a boolean (got {})",
                key,
                describe_json_value(value)
            ))
        })?;
        features.insert(feature_key, value_bool);
    }

    // 5) plugins.autoInstall
    let plugins_value = doc_obj.get("plugins").ok_or_else(|| {
        fail("requires a \"plugins\" object with an \"autoInstall\" array (use { \"autoInstall\": [] } for none)")
    })?;
    if !is_plain_object(plugins_value) {
        return Err(fail(format!(
            "\"plugins\" must be an object (got {})",
            describe_json_value(plugins_value)
        )));
    }
    let plugins_obj = plugins_value.as_object().unwrap();
    for key in plugins_obj.keys() {
        if key != "autoInstall" {
            return Err(fail(format!(
                "\"plugins\" has unknown key \"{}\" (allowed: autoInstall)",
                key
            )));
        }
    }
    let raw_auto_install = plugins_obj.get("autoInstall").ok_or_else(|| {
        fail("requires a \"plugins.autoInstall\" array of plugin keys (use [] for none)")
    })?;
    let raw_auto_install_arr = raw_auto_install.as_array().ok_or_else(|| {
        fail(format!(
            "\"plugins.autoInstall\" must be an array of plugin keys (got {})",
            describe_json_value(raw_auto_install)
        ))
    })?;
    let mut auto_install: Vec<String> = Vec::new();
    for entry in raw_auto_install_arr {
        let s = entry.as_str().ok_or_else(|| {
            fail(format!(
                "\"plugins.autoInstall\" entries must be non-empty strings without surrounding whitespace (got {})",
                describe_json_value(entry)
            ))
        })?;
        if s.is_empty() || s.trim() != s {
            return Err(fail(format!(
                "\"plugins.autoInstall\" entries must be non-empty strings without surrounding whitespace (got {})",
                describe_json_value(entry)
            )));
        }
        if auto_install.iter().any(|existing| existing == s) {
            return Err(fail(format!(
                "\"plugins.autoInstall\" has duplicate entry \"{}\"",
                s
            )));
        }
        auto_install.push(s.to_string());
    }

    // 6) environments (OPTIONAL)
    let mut environment_specs: Vec<ManagedEnvironmentSpec> = Vec::new();
    if let Some(envs_value) = doc_obj.get("environments") {
        let envs_arr = envs_value.as_array().ok_or_else(|| {
            fail(format!(
                "\"environments\" must be an array of environment objects (got {})",
                describe_json_value(envs_value)
            ))
        })?;
        if envs_arr.len() > 1 {
            return Err(fail(
                "\"environments\" supports at most one entry: each entry provisions the single Paperclip-managed sandbox environment (DB invariant environments_managed_sandbox_idx)",
            ));
        }
        for (index, entry_value) in envs_arr.iter().enumerate() {
            if !is_plain_object(entry_value) {
                return Err(fail(format!(
                    "\"environments[{}]\" must be an object (got {})",
                    index,
                    describe_json_value(entry_value)
                )));
            }
            let entry_obj = entry_value.as_object().unwrap();
            let allowed_entry_keys: std::collections::HashSet<&str> =
                ["name", "description", "provider", "config"]
                    .iter()
                    .copied()
                    .collect();
            for key in entry_obj.keys() {
                if !allowed_entry_keys.contains(key.as_str()) {
                    return Err(fail(format!(
                        "\"environments[{}]\" has unknown key \"{}\" (allowed: name, description, provider, config)",
                        index, key
                    )));
                }
            }

            // name
            let name = entry_obj
                .get("name")
                .ok_or_else(|| fail(format!("\"environments[{}].name\" is required", index)))?;
            let name_str = name.as_str().ok_or_else(|| {
                fail(format!(
                    "\"environments[{}].name\" must be a non-empty string without surrounding whitespace (got {})",
                    index,
                    describe_json_value(name)
                ))
            })?;
            if name_str.is_empty() || name_str.trim() != name_str {
                return Err(fail(format!(
                    "\"environments[{}].name\" must be a non-empty string without surrounding whitespace (got {})",
                    index,
                    describe_json_value(name)
                )));
            }

            // description (optional)
            let mut description: Option<String> = None;
            if let Some(desc_value) = entry_obj.get("description") {
                let desc_str = desc_value.as_str().ok_or_else(|| {
                    fail(format!(
                        "\"environments[{}].description\" must be a non-empty string when present (got {})",
                        index,
                        describe_json_value(desc_value)
                    ))
                })?;
                if desc_str.trim().is_empty() {
                    return Err(fail(format!(
                        "\"environments[{}].description\" must be a non-empty string when present (got {})",
                        index,
                        describe_json_value(desc_value)
                    )));
                }
                description = Some(desc_str.to_string());
            }

            // provider
            let provider = entry_obj
                .get("provider")
                .ok_or_else(|| fail(format!("\"environments[{}].provider\" is required", index)))?;
            let provider_str = provider.as_str().ok_or_else(|| {
                fail(format!(
                    "\"environments[{}].provider\" must be a non-empty string without surrounding whitespace (got {})",
                    index,
                    describe_json_value(provider)
                ))
            })?;
            if provider_str.is_empty() || provider_str.trim() != provider_str {
                return Err(fail(format!(
                    "\"environments[{}].provider\" must be a non-empty string without surrounding whitespace (got {})",
                    index,
                    describe_json_value(provider)
                )));
            }

            // Coherence: provider must be in auto_install
            if !auto_install.iter().any(|p| p == provider_str) {
                return Err(fail(format!(
                    "\"environments[{}].provider\" is \"{}\", which is not in \"plugins.autoInstall\"; a managed environment requires its provider plugin to be provisioned",
                    index, provider_str
                )));
            }

            // config (optional, but must be an object, must not set "provider")
            let mut config_map: HashMap<String, serde_json::Value> = HashMap::new();
            if let Some(config_value) = entry_obj.get("config") {
                if !is_plain_object(config_value) {
                    return Err(fail(format!(
                        "\"environments[{}].config\" must be an object (got {})",
                        index,
                        describe_json_value(config_value)
                    )));
                }
                let config_obj = config_value.as_object().unwrap();
                if config_obj.contains_key("provider") {
                    return Err(fail(format!(
                        "\"environments[{}].config\" must not set \"provider\"; it is forced from the entry's provider key",
                        index
                    )));
                }
                // Secret-like key detection
                if let Some(secret_like) = find_secret_like_config_key(config_value, "") {
                    return Err(fail(format!(
                        "\"environments[{}].config\" key \"{}\" looks secret-bearing; credentials are delivered to managed instances as process environment variables (the provider's documented env fallback), never in the managed-config document",
                        index, secret_like
                    )));
                }
                for (k, v) in config_obj {
                    config_map.insert(k.clone(), v.clone());
                }
            }

            environment_specs.push(ManagedEnvironmentSpec {
                name: name_str.to_string(),
                description,
                provider: provider_str.to_string(),
                config: config_map,
            });
        }
    }

    Ok(Some(ManagedInstanceConfig {
        v: SUPPORTED_MANAGED_CONFIG_VERSION,
        mode: "cloud".to_string(),
        catalog_version: catalog_version_str.to_string(),
        features,
        auto_install,
        environments: environment_specs,
    }))
}

// ============================================================================
// get_managed_instance_config
// ============================================================================

/// Parse-once accessor（与 Node `getManagedInstanceConfig(env)` 1:1 对齐）。
///
/// - 缓存键：raw env 值（不是解析后 config）
/// - raw 变化时重新解析
/// - 解析失败抛错，**不**缓存错误（每次调用都重抛，行为对齐 Node
///   "rethrows parse failures on every call"）
pub fn get_managed_instance_config(
    env: ManagedConfigEnv<'_>,
) -> Result<Option<ManagedInstanceConfig>, ManagedConfigError> {
    let raw_value = env.get(MANAGED_CONFIG_ENV_KEY).cloned();
    {
        let cache = cache().lock().expect("cache lock poisoned");
        if cache.raw == raw_value && cache.config.is_none() == (raw_value.is_none()) {
            // cache hit (note: we never cache errors per Node semantics)
            // We need to distinguish: if cache.config exists, return it.
            // If neither env had it nor cache has it, return None.
            // If env had it and cache has None (error cached), reparse.
            // Per Node: "rethrows parse failures on every call instead of caching them"
            // So if cache was error, we should reparse.
            // Simplest: only return cached value when raw matched AND cache has a value (Some) OR neither has the key.
            // We track "raw changed" by comparing. If raw same, return cached config (None or Some).
            if raw_value.is_none() && cache.raw.is_none() {
                return Ok(None);
            }
            if let Some(cached_config) = &cache.config {
                return Ok(Some(cached_config.clone()));
            }
        }
    }

    // Parse (or reparse on cache miss / raw change / error)
    let result = parse_managed_config_env(env);

    // Only cache successful parses (Node: "rethrows parse failures on every call")
    if let Ok(Some(ref config)) = result {
        let mut cache = cache().lock().expect("cache lock poisoned");
        cache.raw = raw_value;
        cache.config = Some(config.clone());
    } else if result.is_err() {
        // Reset cache so next call reparses (Node: rethrows on every call)
        reset_cache();
    }

    result
}

/// 清空 cache（用于测试隔离）。
pub fn clear_managed_config_cache() {
    reset_cache();
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env_with(json_body: Option<&str>) -> HashMap<String, String> {
        let mut env = HashMap::new();
        if let Some(body) = json_body {
            env.insert(MANAGED_CONFIG_ENV_KEY.to_string(), body.to_string());
        }
        env
    }

    fn minimal_valid_doc() -> serde_json::Value {
        json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {},
            "plugins": { "autoInstall": [] },
        })
    }

    fn serialize_doc(doc: &serde_json::Value) -> String {
        serde_json::to_string(doc).unwrap()
    }

    // ----- absent env returns None -----

    #[test]
    fn absent_env_returns_none() {
        let env: HashMap<String, String> = HashMap::new();
        assert!(parse_managed_config_env(&env).unwrap().is_none());
    }

    #[test]
    fn absent_env_via_getter_returns_none() {
        clear_managed_config_cache();
        let env: HashMap<String, String> = HashMap::new();
        assert!(get_managed_instance_config(&env).unwrap().is_none());
    }

    // ----- blank env -----

    #[test]
    fn blank_env_throws() {
        let env = env_with(Some(""));
        let err = parse_managed_config_env(&env).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("is set but blank"));
    }

    #[test]
    fn whitespace_only_env_throws() {
        let env = env_with(Some("   "));
        assert!(parse_managed_config_env(&env).is_err());
    }

    // ----- happy path -----

    #[test]
    fn parses_complete_valid_document() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {
                "enableEnvironments": true,
                "enableApps": false
            },
            "plugins": { "autoInstall": ["kubernetes", "daytona"] }
        });
        let env = env_with(Some(&serialize_doc(&doc)));
        let cfg = parse_managed_config_env(&env).unwrap().unwrap();
        assert_eq!(cfg.v, 1);
        assert_eq!(cfg.mode, "cloud");
        assert_eq!(cfg.catalog_version, "2026.720.0");
        assert_eq!(
            cfg.features.get(&InstanceFeatureKey::EnableEnvironments),
            Some(&true)
        );
        assert_eq!(
            cfg.features.get(&InstanceFeatureKey::EnableApps),
            Some(&false)
        );
        assert_eq!(cfg.auto_install, vec!["kubernetes", "daytona"]);
        assert!(cfg.environments.is_empty());
    }

    #[test]
    fn accepts_empty_features_and_auto_install() {
        clear_managed_config_cache();
        let doc = minimal_valid_doc();
        let env = env_with(Some(&serialize_doc(&doc)));
        let cfg = parse_managed_config_env(&env).unwrap().unwrap();
        assert!(cfg.features.is_empty());
        assert!(cfg.auto_install.is_empty());
    }

    // ----- missing required sections -----

    #[test]
    fn missing_features_section_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut().unwrap().remove("features");
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("requires a \"features\""));
    }

    #[test]
    fn missing_plugins_section_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut().unwrap().remove("plugins");
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("requires a \"plugins\""));
    }

    #[test]
    fn missing_auto_install_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc["plugins"]
            .as_object_mut()
            .unwrap()
            .remove("autoInstall");
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err
            .to_string()
            .contains("requires a \"plugins.autoInstall\""));
    }

    // ----- invalid JSON -----

    #[test]
    fn invalid_json_throws() {
        clear_managed_config_cache();
        let env = env_with(Some("{not json"));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("is not valid JSON"));
    }

    #[test]
    fn non_object_document_throws() {
        clear_managed_config_cache();
        let env = env_with(Some("[1,2,3]"));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("must be a JSON object"));
    }

    #[test]
    fn null_document_throws() {
        clear_managed_config_cache();
        let env = env_with(Some("null"));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("must be a JSON object"));
    }

    // ----- unknown top-level keys -----

    #[test]
    fn unknown_top_level_key_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut()
            .unwrap()
            .insert("bogus".to_string(), json!("x"));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("unknown top-level key \"bogus\""));
    }

    // ----- v / mode / catalogVersion -----

    #[test]
    fn unsupported_v_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut()
            .unwrap()
            .insert("v".to_string(), json!(2));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("unsupported \"v\""));
    }

    #[test]
    fn non_cloud_mode_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut()
            .unwrap()
            .insert("mode".to_string(), json!("self"));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("invalid \"mode\""));
    }

    #[test]
    fn missing_catalog_version_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut().unwrap().remove("catalogVersion");
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("\"catalogVersion\""));
    }

    #[test]
    fn empty_catalog_version_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut()
            .unwrap()
            .insert("catalogVersion".to_string(), json!(""));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("\"catalogVersion\""));
    }

    #[test]
    fn non_string_catalog_version_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut()
            .unwrap()
            .insert("catalogVersion".to_string(), json!(42));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("\"catalogVersion\""));
    }

    // ----- features -----

    #[test]
    fn features_must_be_object() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut()
            .unwrap()
            .insert("features".to_string(), json!([]));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("\"features\" must be an object"));
    }

    #[test]
    fn unknown_feature_key_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc["features"]
            .as_object_mut()
            .unwrap()
            .insert("notARealKey".to_string(), json!(true));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err
            .to_string()
            .contains("unknown feature key \"notARealKey\""));
    }

    #[test]
    fn non_managed_feature_key_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        // enableStreamlinedLeftNavigation is tier "preference"
        doc["features"]
            .as_object_mut()
            .unwrap()
            .insert("enableStreamlinedLeftNavigation".to_string(), json!(true));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("only tier \"managed\" keys"));
        assert!(err.to_string().contains("enableStreamlinedLeftNavigation"));
    }

    #[test]
    fn non_boolean_feature_value_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc["features"]
            .as_object_mut()
            .unwrap()
            .insert("enableEnvironments".to_string(), json!("true"));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("must be a boolean"));
    }

    // ----- plugins.autoInstall -----

    #[test]
    fn plugins_must_be_object() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut()
            .unwrap()
            .insert("plugins".to_string(), json!([]));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("\"plugins\" must be an object"));
    }

    #[test]
    fn unknown_plugins_key_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc["plugins"]
            .as_object_mut()
            .unwrap()
            .insert("removeAll".to_string(), json!(true));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("\"plugins\" has unknown key"));
    }

    #[test]
    fn auto_install_must_be_array() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc["plugins"]
            .as_object_mut()
            .unwrap()
            .insert("autoInstall".to_string(), json!("kubernetes"));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("must be an array of plugin keys"));
    }

    #[test]
    fn auto_install_entry_must_be_non_empty_string() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc["plugins"]
            .as_object_mut()
            .unwrap()
            .insert("autoInstall".to_string(), json!(["kubernetes", ""]));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("must be non-empty strings"));
    }

    #[test]
    fn auto_install_entry_with_whitespace_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc["plugins"]
            .as_object_mut()
            .unwrap()
            .insert("autoInstall".to_string(), json!([" kubernetes"]));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("must be non-empty strings"));
    }

    #[test]
    fn duplicate_auto_install_entry_throws() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc["plugins"].as_object_mut().unwrap().insert(
            "autoInstall".to_string(),
            json!(["kubernetes", "kubernetes"]),
        );
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("duplicate entry \"kubernetes\""));
    }

    // ----- environments -----

    #[test]
    fn environments_absent_is_ok() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut().unwrap().remove("environments");
        let env = env_with(Some(&serialize_doc(&doc)));
        let cfg = parse_managed_config_env(&env).unwrap().unwrap();
        assert!(cfg.environments.is_empty());
    }

    #[test]
    fn parses_declared_environment() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {},
            "plugins": { "autoInstall": ["daytona"] },
            "environments": [{
                "name": "Daytona",
                "provider": "daytona",
                "config": { "target": "us" }
            }]
        });
        let env = env_with(Some(&serialize_doc(&doc)));
        let cfg = parse_managed_config_env(&env).unwrap().unwrap();
        assert_eq!(cfg.environments.len(), 1);
        assert_eq!(cfg.environments[0].name, "Daytona");
        assert_eq!(cfg.environments[0].provider, "daytona");
        assert_eq!(
            cfg.environments[0].config.get("target"),
            Some(&serde_json::json!("us"))
        );
    }

    #[test]
    fn environment_with_optional_description() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {},
            "plugins": { "autoInstall": ["kubernetes"] },
            "environments": [{
                "name": "k8s",
                "description": "Primary cluster",
                "provider": "kubernetes"
            }]
        });
        let env = env_with(Some(&serialize_doc(&doc)));
        let cfg = parse_managed_config_env(&env).unwrap().unwrap();
        assert_eq!(
            cfg.environments[0].description.as_deref(),
            Some("Primary cluster")
        );
        assert!(cfg.environments[0].config.is_empty());
    }

    #[test]
    fn environments_must_be_array() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut()
            .unwrap()
            .insert("environments".to_string(), json!("nope"));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err
            .to_string()
            .contains("\"environments\" must be an array"));
    }

    #[test]
    fn environment_entry_must_be_object() {
        clear_managed_config_cache();
        let mut doc = minimal_valid_doc();
        doc.as_object_mut()
            .unwrap()
            .insert("environments".to_string(), json!([42]));
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err
            .to_string()
            .contains("\"environments[0]\" must be an object"));
    }

    #[test]
    fn more_than_one_environment_throws() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {},
            "plugins": { "autoInstall": ["kubernetes"] },
            "environments": [
                { "name": "a", "provider": "kubernetes" },
                { "name": "b", "provider": "kubernetes" }
            ]
        });
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("at most one entry"));
    }

    #[test]
    fn environment_unknown_entry_key_throws() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {},
            "plugins": { "autoInstall": ["kubernetes"] },
            "environments": [{
                "name": "k8s",
                "provider": "kubernetes",
                "bogus": "value"
            }]
        });
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("unknown key \"bogus\""));
    }

    #[test]
    fn environment_name_must_be_valid() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {},
            "plugins": { "autoInstall": ["kubernetes"] },
            "environments": [{
                "name": "  ",
                "provider": "kubernetes"
            }]
        });
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("\"environments[0].name\""));
    }

    #[test]
    fn environment_description_must_be_non_empty_when_present() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {},
            "plugins": { "autoInstall": ["kubernetes"] },
            "environments": [{
                "name": "k8s",
                "description": "   ",
                "provider": "kubernetes"
            }]
        });
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("\"environments[0].description\""));
    }

    #[test]
    fn environment_provider_not_in_auto_install_throws() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {},
            "plugins": { "autoInstall": ["kubernetes"] },
            "environments": [{
                "name": "Daytona",
                "provider": "daytona"
            }]
        });
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("not in \"plugins.autoInstall\""));
    }

    #[test]
    fn environment_config_setting_provider_throws() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {},
            "plugins": { "autoInstall": ["kubernetes"] },
            "environments": [{
                "name": "k8s",
                "provider": "kubernetes",
                "config": { "provider": "kubernetes" }
            }]
        });
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("must not set \"provider\""));
    }

    #[test]
    fn environment_secret_like_top_level_throws() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {},
            "plugins": { "autoInstall": ["kubernetes"] },
            "environments": [{
                "name": "k8s",
                "provider": "kubernetes",
                "config": { "apiKey": "leaked" }
            }]
        });
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("looks secret-bearing"));
    }

    #[test]
    fn environment_secret_like_nested_throws() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1,
            "mode": "cloud",
            "catalogVersion": "2026.720.0",
            "features": {},
            "plugins": { "autoInstall": ["kubernetes"] },
            "environments": [{
                "name": "k8s",
                "provider": "kubernetes",
                "config": { "auth": { "token": "leaked" } }
            }]
        });
        let env = env_with(Some(&serialize_doc(&doc)));
        let err = parse_managed_config_env(&env).unwrap_err();
        assert!(err.to_string().contains("auth.token"));
    }

    // ----- cache -----

    #[test]
    fn get_caches_by_raw_value() {
        clear_managed_config_cache();
        let doc = json!({
            "v": 1, "mode": "cloud", "catalogVersion": "x",
            "features": {}, "plugins": { "autoInstall": ["kubernetes"] }
        });
        let body = serialize_doc(&doc);
        let env1 = env_with(Some(&body));
        let cfg1 = get_managed_instance_config(&env1).unwrap().unwrap();
        // Second call: same raw value should hit cache (we can't directly observe,
        // but it must produce the same config)
        let cfg2 = get_managed_instance_config(&env1).unwrap().unwrap();
        assert_eq!(cfg1, cfg2);
    }

    #[test]
    fn get_reparses_when_raw_changes() {
        clear_managed_config_cache();
        let doc_a = json!({
            "v": 1, "mode": "cloud", "catalogVersion": "x",
            "features": {}, "plugins": { "autoInstall": ["kubernetes"] }
        });
        let doc_b = json!({
            "v": 1, "mode": "cloud", "catalogVersion": "x",
            "features": {}, "plugins": { "autoInstall": ["daytona"] }
        });
        let env_a = env_with(Some(&serialize_doc(&doc_a)));
        let env_b = env_with(Some(&serialize_doc(&doc_b)));
        let cfg_a = get_managed_instance_config(&env_a).unwrap().unwrap();
        assert_eq!(cfg_a.auto_install, vec!["kubernetes"]);
        let cfg_b = get_managed_instance_config(&env_b).unwrap().unwrap();
        assert_eq!(cfg_b.auto_install, vec!["daytona"]);
    }

    #[test]
    fn get_rethrows_parse_failures_on_every_call() {
        clear_managed_config_cache();
        let env = env_with(Some("{bad"));
        let err1 = get_managed_instance_config(&env);
        assert!(err1.is_err());
        let err2 = get_managed_instance_config(&env);
        assert!(err2.is_err());
    }
}
