//! Collectors —— 从 adapter config 提取 secret_ref / user_secret_ref bindings。
//!
//! 与 Node `collectSecretRefs` / `collectUserSecretRefs` 1:1 对齐。

use serde_json::{Map, Value};

use crate::types::{SecretProjectionClass, SecretRef, SecretVersionSelector, UserSecretRef};

// ============================================================================
// Helpers
// ============================================================================

fn as_record(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value.and_then(|v| v.as_object())
}

/// 解析 `secretVersionSelectorSchema`：`"latest"` 或正整数。
///
/// 解析失败时回退到 `Latest`（与 Node `binding.version ?? "latest"` 1:1 对齐）。
fn parse_version_selector(value: Option<&Value>) -> SecretVersionSelector {
    match value {
        Some(Value::String(s)) if s == "latest" => SecretVersionSelector::Latest,
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                if i > 0 {
                    SecretVersionSelector::Version(i)
                } else {
                    SecretVersionSelector::Latest
                }
            } else {
                SecretVersionSelector::Latest
            }
        }
        _ => SecretVersionSelector::Latest,
    }
}

/// 解析 `projectionClass` 字段：`"unclassified"` / `"class_3_static_lease"`。
///
/// 未知值返回 `None`（保留 Node 端 zod enum 校验失败时静默跳过的语义）。
fn parse_projection_class(value: Option<&Value>) -> Option<SecretProjectionClass> {
    match value.and_then(|v| v.as_str()) {
        Some("unclassified") => Some(SecretProjectionClass::Unclassified),
        Some("class_3_static_lease") => Some(SecretProjectionClass::Class3StaticLease),
        _ => None,
    }
}

fn parse_projection_allowlist_key(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// 校验一个 binding 是否为合法的 `secret_ref`。
///
/// Node 端用 `envBindingSchema.safeParse(rawBinding)` 校验，再判断 `binding.type === "secret_ref"`。
/// 本实现直接检查 JSON 字段，避免引入 zod 类依赖：
/// - `type` 必须为字面量 `"secret_ref"`
/// - `secretId` 必须为非空字符串
fn is_secret_ref(binding: &Value) -> bool {
    let obj = match binding.as_object() {
        Some(o) => o,
        None => return false,
    };
    if obj.get("type").and_then(|v| v.as_str()) != Some("secret_ref") {
        return false;
    }
    match obj.get("secretId").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => true,
        _ => false,
    }
}

/// 校验一个 binding 是否为合法的 `user_secret_ref`。
fn is_user_secret_ref(binding: &Value) -> bool {
    let obj = match binding.as_object() {
        Some(o) => o,
        None => return false,
    };
    if obj.get("type").and_then(|v| v.as_str()) != Some("user_secret_ref") {
        return false;
    }
    match obj.get("key").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => true,
        _ => false,
    }
}

/// 从合法 `secret_ref` binding 构造 [`SecretRef`]。
fn build_secret_ref(config_path: &str, binding: &Map<String, Value>) -> Option<SecretRef> {
    let secret_id = binding.get("secretId")?.as_str()?.to_string();
    Some(SecretRef {
        secret_id,
        config_path: config_path.to_string(),
        version_selector: parse_version_selector(binding.get("version")),
        projection_class: parse_projection_class(binding.get("projectionClass")),
        projection_allowlist_key: parse_projection_allowlist_key(
            binding.get("projectionAllowlistKey"),
        ),
    })
}

/// 从合法 `user_secret_ref` binding 构造 [`UserSecretRef`]。
fn build_user_secret_ref(
    config_path: &str,
    env_key: &str,
    binding: &Map<String, Value>,
) -> Option<UserSecretRef> {
    let definition_key = binding.get("key")?.as_str()?.to_string();
    Some(UserSecretRef {
        definition_key,
        config_path: config_path.to_string(),
        env_key: env_key.to_string(),
        version_selector: parse_version_selector(binding.get("version")),
        required: binding
            .get("required")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        allow_missing_override: binding
            .get("allowMissingOverride")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

// ============================================================================
// Collectors
// ============================================================================

/// 提取所有 `secret_ref` bindings（与 Node `collectSecretRefs` 1:1 对齐）。
///
/// ## 扫描顺序
///
/// 1. 先扫 `config.env.<key>` 下的 bindings（configPath 格式：`env.<key>`）。
/// 2. 再扫 `config.*`（除 `env` 之外）的顶层字段（configPath 格式：`<key>`）。
///
/// ## 解析规则
///
/// - binding 必须是合法对象且 `type === "secret_ref"` 且 `secretId` 非空。
/// - 非合法 binding 静默跳过（与 Node zod safeParse 失败等价）。
pub fn collect_secret_refs(adapter_config: &Value) -> Vec<SecretRef> {
    let mut refs: Vec<SecretRef> = Vec::new();
    let config = match as_record(Some(adapter_config)) {
        Some(c) => c,
        None => return refs,
    };

    // 1. env.<key>
    if let Some(env_value) = as_record(config.get("env")) {
        for (key, raw_binding) in env_value {
            if !is_secret_ref(raw_binding) {
                continue;
            }
            if let Some(obj) = raw_binding.as_object() {
                if let Some(r) = build_secret_ref(&format!("env.{key}"), obj) {
                    refs.push(r);
                }
            }
        }
    }

    // 2. 顶层 <key>（除 env 外）
    for (key, raw_binding) in config {
        if key == "env" {
            continue;
        }
        if !is_secret_ref(raw_binding) {
            continue;
        }
        if let Some(obj) = raw_binding.as_object() {
            if let Some(r) = build_secret_ref(key, obj) {
                refs.push(r);
            }
        }
    }

    refs
}

/// 提取所有 `user_secret_ref` bindings（与 Node `collectUserSecretRefs` 1:1 对齐）。
///
/// 扫描顺序与 [`collect_secret_refs`] 一致；configPath / envKey 按规则生成。
pub fn collect_user_secret_refs(adapter_config: &Value) -> Vec<UserSecretRef> {
    let mut refs: Vec<UserSecretRef> = Vec::new();
    let config = match as_record(Some(adapter_config)) {
        Some(c) => c,
        None => return refs,
    };

    // 1. env.<key>
    if let Some(env_value) = as_record(config.get("env")) {
        for (key, raw_binding) in env_value {
            if !is_user_secret_ref(raw_binding) {
                continue;
            }
            if let Some(obj) = raw_binding.as_object() {
                if let Some(r) = build_user_secret_ref(&format!("env.{key}"), key, obj) {
                    refs.push(r);
                }
            }
        }
    }

    // 2. 顶层 <key>（除 env 外）
    for (key, raw_binding) in config {
        if key == "env" {
            continue;
        }
        if !is_user_secret_ref(raw_binding) {
            continue;
        }
        if let Some(obj) = raw_binding.as_object() {
            if let Some(r) = build_user_secret_ref(key, key, obj) {
                refs.push(r);
            }
        }
    }

    refs
}
