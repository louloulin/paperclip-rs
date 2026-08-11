//! 声明式 adapter registry bootstrap。
//!
//! 对应 Node `server/src/services/adapter-registry-bootstrap.ts`（97 行）1:1 复刻。
//! （原 `pc-adapter-registry-bootstrap` crate 已下沉到 `pc-adapter-api`）。


use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::info;

// ============================================================================
// Types
// ============================================================================

/// Adapter registry entry（与 Node `adapterRegistryEntrySchema` 1:1 对齐）。
///
/// JSON 字段用 camelCase（与 zod schema 1:1 对齐）。
/// `.strict()` 由 serde `deny_unknown_fields` 实现。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterRegistryEntry {
    /// Adapter type identifier（e.g. `"claude_local"`, `"codex_local"`）。
    pub adapter_type: String,
    /// 是否启用（默认 `true`）。
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// k8s runtime image override。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_image: Option<String>,
    /// env keys to expose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_keys: Option<Vec<String>>,
    /// FQDNs to allow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fqdns: Option<Vec<String>>,
    /// probe command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe_command: Option<Vec<String>>,
    /// default env.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_env: Option<HashMap<String, String>>,
}

fn default_enabled() -> bool {
    true
}

/// `AdapterRegistryEntry` 的别名（与 Node `AdapterRegistryEntryParsed` 1:1 对齐）。
pub type AdapterRegistryEntryParsed = AdapterRegistryEntry;

/// adapter registry 列表（与 Node `adapterRegistrySchema = z.array(...)` 1:1 对齐）。
pub type AdapterRegistryList = Vec<AdapterRegistryEntry>;

/// env 来源（与 Node `process.env` 1:1 对齐）。
pub type AdapterRegistryEnv = HashMap<String, String>;

// ============================================================================
// Errors
// ============================================================================

/// Adapter registry bootstrap 错误。
#[derive(Debug, Error)]
pub enum RegistryBootstrapError {
    #[error("PAPERCLIP_ADAPTERS_FILE could not be read at \"{path}\": {message}")]
    FileRead { path: String, message: String },
    #[error("PAPERCLIP_ADAPTERS must be valid JSON: {0}")]
    InvalidJson(String),
    #[error("PAPERCLIP_ADAPTERS failed validation: {0}")]
    ValidationFailed(String),
    #[error("PAPERCLIP_ADAPTERS declares adapter type(s) with no installed adapter: {0}")]
    UnknownAdapterTypes(String),
}

pub type RegistryBootstrapResult<T> = std::result::Result<T, RegistryBootstrapError>;

// ============================================================================
// Parse env
// ============================================================================

/// 从 env 解析 adapter registry。
///
/// - `PAPERCLIP_ADAPTERS`（inline JSON）优先级高
/// - `PAPERCLIP_ADAPTERS_FILE`（file path）次之
/// - 都没设 → 返回 `None`（built-in defaults）
/// - 设了但 JSON / file 读不出来 → 抛错
pub fn parse_adapter_registry_env(
    env: &AdapterRegistryEnv,
) -> RegistryBootstrapResult<Option<AdapterRegistryList>> {
    let inline = env
        .get("PAPERCLIP_ADAPTERS")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let file_path = env
        .get("PAPERCLIP_ADAPTERS_FILE")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if inline.is_none() && file_path.is_none() {
        return Ok(None);
    }

    let raw_text = if let Some(text) = inline {
        text
    } else {
        let path = file_path.as_ref().unwrap();
        std::fs::read_to_string(path).map_err(|e| RegistryBootstrapError::FileRead {
            path: path.clone(),
            message: e.to_string(),
        })?
    };

    let parsed: serde_json::Value =
        serde_json::from_str(&raw_text).map_err(|e| RegistryBootstrapError::InvalidJson(e.to_string()))?;

    validate_registry(&parsed)?;
    let list: AdapterRegistryList =
        serde_json::from_value(parsed).map_err(|e| RegistryBootstrapError::ValidationFailed(e.to_string()))?;
    Ok(Some(list))
}

/// 自定义 validation（与 zod `.strict()` + 字段约束 1:1 对齐）。
fn validate_registry(value: &serde_json::Value) -> RegistryBootstrapResult<()> {
    let arr = value
        .as_array()
        .ok_or_else(|| RegistryBootstrapError::ValidationFailed("expected array".into()))?;

    for (idx, entry) in arr.iter().enumerate() {
        let obj = entry
            .as_object()
            .ok_or_else(|| RegistryBootstrapError::ValidationFailed(format!("[{idx}]: not object")))?;

        // adapterType: required, non-empty string
        let adapter_type = obj
            .get("adapterType")
            .ok_or_else(|| {
                RegistryBootstrapError::ValidationFailed(format!("[{idx}]: adapterType required"))
            })?
            .as_str()
            .ok_or_else(|| {
                RegistryBootstrapError::ValidationFailed(format!("[{idx}]: adapterType must be string"))
            })?;
        if adapter_type.is_empty() {
            return Err(RegistryBootstrapError::ValidationFailed(format!(
                "[{idx}]: adapterType must be non-empty"
            )));
        }

        // enabled: optional, default true, must be bool if present
        if let Some(v) = obj.get("enabled") {
            if !v.is_boolean() {
                return Err(RegistryBootstrapError::ValidationFailed(format!(
                    "[{idx}].enabled must be boolean"
                )));
            }
        }

        // runtimeImage / probeCommand: must be string / array<string> if present
        if let Some(v) = obj.get("runtimeImage") {
            if !v.is_string() {
                return Err(RegistryBootstrapError::ValidationFailed(format!(
                    "[{idx}].runtimeImage must be string"
                )));
            }
        }
        if let Some(v) = obj.get("probeCommand") {
            if !v.is_array() {
                return Err(RegistryBootstrapError::ValidationFailed(format!(
                    "[{idx}].probeCommand must be array"
                )));
            }
        }
        if let Some(v) = obj.get("envKeys") {
            if !v.is_array() {
                return Err(RegistryBootstrapError::ValidationFailed(format!(
                    "[{idx}].envKeys must be array"
                )));
            }
        }
        if let Some(v) = obj.get("allowFqdns") {
            if !v.is_array() {
                return Err(RegistryBootstrapError::ValidationFailed(format!(
                    "[{idx}].allowFqdns must be array"
                )));
            }
        }
        if let Some(v) = obj.get("defaultEnv") {
            if !v.is_object() {
                return Err(RegistryBootstrapError::ValidationFailed(format!(
                    "[{idx}].defaultEnv must be object"
                )));
            }
        }
    }

    Ok(())
}

// ============================================================================
// Reconcile availability
// ============================================================================

/// 抽象已知 server adapters 列表（与 Node `listServerAdapters()` 1:1 对齐）。
///
/// 上层可注入实际 adapter registry；测试用 fake。
pub trait KnownAdapters: Send + Sync {
    fn list_adapter_types(&self) -> Vec<String>;
}

/// 抽象 disabled-set 写入（与 Node `setAdapterDisabled()` 1:1 对齐）。
///
/// 上层写到 disk / DB；测试用 `Vec<String>` collector。
pub trait DisabledSetWriter: Send + Sync {
    fn set_disabled(&self, adapter_type: &str, disabled: bool);
}

/// 内存版 disabled-set writer（测试用）。
#[derive(Debug, Default, Clone)]
pub struct InMemoryDisabledSet {
    inner: std::sync::Arc<std::sync::Mutex<HashMap<String, bool>>>,
}

impl InMemoryDisabledSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> HashMap<String, bool> {
        self.inner.lock().unwrap().clone()
    }

    pub fn is_disabled(&self, adapter_type: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(adapter_type)
            .copied()
            .unwrap_or(false)
    }
}

impl DisabledSetWriter for InMemoryDisabledSet {
    fn set_disabled(&self, adapter_type: &str, disabled: bool) {
        self.inner
            .lock()
            .unwrap()
            .insert(adapter_type.to_string(), disabled);
    }
}

/// reconcile 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileResult {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

/// Reconcile：把 `registry` 与已知 adapters 对齐，写入 disabled-set。
///
/// 行为（与 Node 1:1）：
/// - `registry == None` → no-op，返回 `{ enabled: [], disabled: [] }`
/// - declared 中出现未知 type → 抛错（不能 offer harness）
/// - 已知 type 但未 declared → 默认 `enabled`（除非 registry entry `enabled = false`）
/// - 写入 disabled-set 反映 `should_enable` 的反值
pub fn reconcile_adapter_availability(
    registry: Option<&AdapterRegistryList>,
    known: &dyn KnownAdapters,
    writer: &dyn DisabledSetWriter,
) -> RegistryBootstrapResult<ReconcileResult> {
    let registry = match registry {
        None => return Ok(ReconcileResult::default()),
        Some(r) => r,
    };

    let known_types: HashSet<String> = known.list_adapter_types().into_iter().collect();
    let declared: HashMap<String, &AdapterRegistryEntry> =
        registry.iter().map(|e| (e.adapter_type.clone(), e)).collect();

    // 1. declared 中未知 type → 抛错
    let missing: Vec<String> = declared
        .keys()
        .filter(|t| !known_types.contains(*t))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(RegistryBootstrapError::UnknownAdapterTypes(missing.join(", ")));
    }

    let mut enabled = Vec::new();
    let mut disabled = Vec::new();
    for type_name in &known_types {
        // Node 语义：未 declared 的 known adapter 视为 disable（保守策略）
        // declared + enabled=true → enable；declared + enabled=false → disable
        let should_enable = match declared.get(type_name) {
            Some(entry) => entry.enabled,
            None => false,
        };
        writer.set_disabled(type_name, !should_enable);
        if should_enable {
            enabled.push(type_name.clone());
        } else {
            disabled.push(type_name.clone());
        }
    }

    info!(?enabled, ?disabled, "reconciled adapter availability from PAPERCLIP_ADAPTERS");
    Ok(ReconcileResult { enabled, disabled })
}

impl Default for ReconcileResult {
    fn default() -> Self {
        Self {
            enabled: Vec::new(),
            disabled: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(known_types: &[&str]) -> impl KnownAdapters {
        struct K(Vec<String>);
        impl KnownAdapters for K {
            fn list_adapter_types(&self) -> Vec<String> {
                self.0.clone()
            }
        }
        K(known_types.iter().map(|s| s.to_string()).collect())
    }

    fn env_from(pairs: &[(&str, &str)]) -> AdapterRegistryEnv {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // ----- parse -----

    #[test]
    fn r711_parse_returns_none_when_unconfigured() {
        let env = env_from(&[]);
        let r = parse_adapter_registry_env(&env).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn r711_parse_returns_none_when_env_values_empty() {
        let env = env_from(&[("PAPERCLIP_ADAPTERS", "  "), ("PAPERCLIP_ADAPTERS_FILE", "  ")]);
        let r = parse_adapter_registry_env(&env).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn r711_parse_inline_json_minimal() {
        let env = env_from(&[(
            "PAPERCLIP_ADAPTERS",
            r#"[{"adapterType":"claude_local"}]"#,
        )]);
        let r = parse_adapter_registry_env(&env).unwrap().unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].adapter_type, "claude_local");
        assert_eq!(r[0].enabled, true);
    }

    #[test]
    fn r711_parse_inline_json_full() {
        let json = r#"[{"adapterType":"codex_local","enabled":false,"runtimeImage":"img:latest","envKeys":["A"],"allowFqdns":["x.com"],"probeCommand":["a","b"],"defaultEnv":{"K":"V"}}]"#;
        let env = env_from(&[("PAPERCLIP_ADAPTERS", json)]);
        let r = parse_adapter_registry_env(&env).unwrap().unwrap();
        assert_eq!(r[0].adapter_type, "codex_local");
        assert_eq!(r[0].enabled, false);
        assert_eq!(r[0].runtime_image.as_deref(), Some("img:latest"));
        assert_eq!(r[0].env_keys.as_ref().unwrap().len(), 1);
        assert_eq!(r[0].allow_fqdns.as_ref().unwrap()[0], "x.com");
        assert_eq!(r[0].probe_command.as_ref().unwrap().len(), 2);
        assert_eq!(r[0].default_env.as_ref().unwrap().get("K").unwrap(), "V");
    }

    #[test]
    fn r711_parse_invalid_json_throws() {
        let env = env_from(&[("PAPERCLIP_ADAPTERS", "{not json")]);
        assert!(matches!(
            parse_adapter_registry_env(&env),
            Err(RegistryBootstrapError::InvalidJson(_))
        ));
    }

    #[test]
    fn r711_parse_not_array_throws() {
        let env = env_from(&[("PAPERCLIP_ADAPTERS", r#"{"adapterType":"x"}"#)]);
        assert!(matches!(
            parse_adapter_registry_env(&env),
            Err(RegistryBootstrapError::ValidationFailed(_))
        ));
    }

    #[test]
    fn r711_parse_missing_adapter_type_throws() {
        let env = env_from(&[("PAPERCLIP_ADAPTERS", r#"[{"enabled":true}]"#)]);
        assert!(matches!(
            parse_adapter_registry_env(&env),
            Err(RegistryBootstrapError::ValidationFailed(_))
        ));
    }

    #[test]
    fn r711_parse_empty_adapter_type_throws() {
        let env = env_from(&[("PAPERCLIP_ADAPTERS", r#"[{"adapterType":""}]"#)]);
        assert!(matches!(
            parse_adapter_registry_env(&env),
            Err(RegistryBootstrapError::ValidationFailed(_))
        ));
    }

    #[test]
    fn r711_parse_unknown_field_throws() {
        // serde `deny_unknown_fields` 自动拦截
        let env = env_from(&[("PAPERCLIP_ADAPTERS", r#"[{"adapterType":"x","foo":1}]"#)]);
        assert!(parse_adapter_registry_env(&env).is_err());
    }

    #[test]
    fn r711_parse_file_fallback() {
        // 写一个临时 JSON 文件
        let dir = std::env::temp_dir().join("pc-adapter-registry-bootstrap-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("adapters.json");
        std::fs::write(&path, r#"[{"adapterType":"codex_local"}]"#).unwrap();
        let env = env_from(&[("PAPERCLIP_ADAPTERS_FILE", path.to_str().unwrap())]);
        let r = parse_adapter_registry_env(&env).unwrap().unwrap();
        assert_eq!(r[0].adapter_type, "codex_local");
        // 不需要清理 —— /tmp 临时目录
    }

    #[test]
    fn r711_parse_inline_takes_precedence_over_file() {
        // inline 优先级高；file 即使不存在也不报错
        let env = env_from(&[
            ("PAPERCLIP_ADAPTERS", r#"[{"adapterType":"claude_local"}]"#),
            ("PAPERCLIP_ADAPTERS_FILE", "/nonexistent/path"),
        ]);
        let r = parse_adapter_registry_env(&env).unwrap().unwrap();
        assert_eq!(r[0].adapter_type, "claude_local");
    }

    #[test]
    fn r711_parse_file_not_found_throws() {
        let env = env_from(&[("PAPERCLIP_ADAPTERS_FILE", "/nonexistent/path")]);
        assert!(matches!(
            parse_adapter_registry_env(&env),
            Err(RegistryBootstrapError::FileRead { .. })
        ));
    }

    // ----- reconcile -----

    #[test]
    fn r711_reconcile_null_registry_is_noop() {
        let k = known(&["a", "b"]);
        let w = InMemoryDisabledSet::new();
        let r = reconcile_adapter_availability(None, &k, &w).unwrap();
        assert_eq!(r.enabled, Vec::<String>::new());
        assert_eq!(r.disabled, Vec::<String>::new());
        assert!(w.snapshot().is_empty());
    }

    #[test]
    fn r711_reconcile_empty_registry_disables_all() {
        let k = known(&["claude_local", "codex_local", "acpx"]);
        let w = InMemoryDisabledSet::new();
        // registry 是 Some([]) —— 显式空列表（不是 None no-op）
        let r = reconcile_adapter_availability(Some(&vec![]), &k, &w).unwrap();
        assert_eq!(r.enabled.len(), 0);
        assert_eq!(r.disabled.len(), 3);
        // Node 保守策略：未 declared 的 known adapter 视为 disable
        assert!(w.is_disabled("claude_local"));
        assert!(w.is_disabled("codex_local"));
        assert!(w.is_disabled("acpx"));
    }

    #[test]
    fn r711_reconcile_disables_undeclared() {
        // registry 只声明 claude_local → 其他未 declared 的 known adapter 视为 disable
        let registry = vec![AdapterRegistryEntry {
            adapter_type: "claude_local".into(),
            enabled: true,
            runtime_image: None,
            env_keys: None,
            allow_fqdns: None,
            probe_command: None,
            default_env: None,
        }];
        let k = known(&["claude_local", "codex_local"]);
        let w = InMemoryDisabledSet::new();
        let r = reconcile_adapter_availability(Some(&registry), &k, &w).unwrap();
        assert_eq!(r.enabled, vec!["claude_local".to_string()]);
        assert_eq!(r.disabled, vec!["codex_local".to_string()]);
        assert!(!w.is_disabled("claude_local"));
        assert!(w.is_disabled("codex_local"));
    }

    #[test]
    fn r711_reconcile_disables_declared_false() {
        let registry = vec![
            // codex_local 显式 declared + enabled=false
            AdapterRegistryEntry {
                adapter_type: "codex_local".into(),
                enabled: false,
                runtime_image: None,
                env_keys: None,
                allow_fqdns: None,
                probe_command: None,
                default_env: None,
            },
            // claude_local 显式 declared + enabled=true
            AdapterRegistryEntry {
                adapter_type: "claude_local".into(),
                enabled: true,
                runtime_image: None,
                env_keys: None,
                allow_fqdns: None,
                probe_command: None,
                default_env: None,
            },
        ];
        let k = known(&["claude_local", "codex_local"]);
        let w = InMemoryDisabledSet::new();
        let r = reconcile_adapter_availability(Some(&registry), &k, &w).unwrap();
        assert_eq!(r.enabled, vec!["claude_local".to_string()]);
        assert_eq!(r.disabled, vec!["codex_local".to_string()]);
        assert!(w.is_disabled("codex_local"));
        assert!(!w.is_disabled("claude_local"));
    }

    #[test]
    fn r711_reconcile_unknown_declared_type_throws() {
        let registry = vec![AdapterRegistryEntry {
            adapter_type: "phantom_adapter".into(),
            enabled: true,
            runtime_image: None,
            env_keys: None,
            allow_fqdns: None,
            probe_command: None,
            default_env: None,
        }];
        let k = known(&["claude_local"]);
        let w = InMemoryDisabledSet::new();
        assert!(matches!(
            reconcile_adapter_availability(Some(&registry), &k, &w),
            Err(RegistryBootstrapError::UnknownAdapterTypes(_))
        ));
    }

    // ----- types -----

    #[test]
    fn r711_entry_default_enabled_is_true() {
        let e: AdapterRegistryEntry = serde_json::from_str(r#"{"adapterType":"x"}"#).unwrap();
        assert_eq!(e.enabled, true);
    }

    #[test]
    fn r711_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AdapterRegistryEntry>();
        assert_send_sync::<InMemoryDisabledSet>();
    }
}
