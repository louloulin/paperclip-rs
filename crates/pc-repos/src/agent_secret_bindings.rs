//! Agent 适配器 env 绑定（secret_ref / user_secret_ref）解析与同步。
//!
//! 对齐 `paperclip/server/src/services/agent-secret-bindings.ts`：
//! - `collect_secret_refs(adapter_config)` 提取所有 `secret_ref` 绑定
//! - `collect_user_secret_refs(adapter_config)` 提取所有 `user_secret_ref` 绑定
//! - `sync_agent_adapter_env_bindings(...)` 调 `secretsSvc` 把 ref 写回
//!
//! 与 Node 端语义一致的关键点：
//! - 遍历 `env.*` 与顶层（除 `env` 外）的所有字段
//! - 只接受 `{ type: "secret_ref" | "user_secret_ref" | "plain" }` 三类结构
//!   以及 legacy 字符串
//! - 顶层 `env` 字段本身是嵌套对象，不当成 ref 处理
//! - 默认 `version = "latest"`，`required = true`，`allowMissingOverride = false`

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Secret 版本选择：具体版本号或 `"latest"` 字面量。
///
/// 对齐 `paperclipai/shared/src/types/secrets.ts::SecretVersionSelector`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SecretVersionSelector {
    Latest(LatestVersion),
    Number(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LatestVersion(#[serde(deserialize_with = "deserialize_latest")] ());
fn deserialize_latest<'de, D: serde::Deserializer<'de>>(d: D) -> Result<(), D::Error> {
    let s: String = String::deserialize(d)?;
    if s == "latest" {
        Ok(())
    } else {
        Err(serde::de::Error::custom(format!(
            "expected 'latest', got {s:?}"
        )))
    }
}
impl LatestVersion {
    pub const fn new() -> Self {
        Self(())
    }
}
impl Default for SecretVersionSelector {
    fn default() -> Self {
        Self::Latest(LatestVersion::new())
    }
}
impl SecretVersionSelector {
    pub fn as_db_value(self) -> SecretVersionSelectorValue {
        match self {
            Self::Latest(_) => SecretVersionSelectorValue::Latest,
            Self::Number(n) => SecretVersionSelectorValue::Number(n),
        }
    }
}

/// 数据库序列化形式（可空数字或 "latest"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum SecretVersionSelectorValue {
    Number(i64),
    Latest,
}

/// 同步 secret_ref 的目标描述。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretBindingTargetType {
    Agent,
}

/// 单条 `secret_ref` 提取结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    pub secret_id: String,
    pub config_path: String,
    pub version_selector: SecretVersionSelector,
    pub projection_class: Option<String>,
    pub projection_allowlist_key: Option<String>,
}

/// 单条 `user_secret_ref` 提取结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSecretRef {
    pub definition_key: String,
    pub config_path: String,
    pub env_key: String,
    pub version_selector: SecretVersionSelector,
    pub required: bool,
    pub allow_missing_override: bool,
}

/// 同步服务接口（与 `secretsService` 的相关子集对齐）。
///
/// 上层实现位于 `pc-secrets` crate；本 trait 把同步路径上的最小依赖
/// 抽象出来，让 `agent_secret_bindings` 不直接依赖具体 secrets 实现。
#[async_trait]
pub trait AgentSecretBindingSync: Send + Sync {
    async fn sync_secret_refs_for_target(
        &self,
        company_id: uuid::Uuid,
        target: (SecretBindingTargetType, String),
        refs: &[SecretRef],
        options: SyncOptions,
    ) -> Result<(), String>;

    async fn sync_user_secret_declarations_for_target(
        &self,
        company_id: uuid::Uuid,
        target: (SecretBindingTargetType, String),
        refs: &[UserSecretRef],
        options: SyncOptions,
    ) -> Result<(), String>;

    async fn sync_env_bindings_for_target(
        &self,
        company_id: uuid::Uuid,
        target: (SecretBindingTargetType, String, Option<String>),
        env_value: Value,
    ) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncOptions {
    pub replace_all: bool,
}

fn as_record(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.as_object()
}

/// 内部：尝试把任意 JSON 值解析为 secret_ref binding。
/// 与 Node 端 `envBindingSchema` 对齐：必须含 `type === "secret_ref"` 且
/// `secretId` 是非空字符串。
fn try_parse_secret_ref(
    value: &Value,
) -> Option<(
    String,
    Option<SecretVersionSelector>,
    Option<String>,
    Option<String>,
)> {
    let obj = value.as_object()?;
    if obj.get("type")?.as_str()? != "secret_ref" {
        return None;
    }
    let secret_id = obj.get("secretId")?.as_str()?.trim().to_string();
    if secret_id.is_empty() {
        return None;
    }
    let version_selector = match obj.get("version") {
        Some(v) => serde_json::from_value(v.clone()).ok()?,
        None => SecretVersionSelector::default(),
    };
    let projection_class = obj
        .get("projectionClass")
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    let projection_allowlist_key = obj
        .get("projectionAllowlistKey")
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    Some((
        secret_id,
        Some(version_selector),
        projection_class,
        projection_allowlist_key,
    ))
}

fn try_parse_user_secret_ref(
    value: &Value,
) -> Option<(
    String,
    Option<SecretVersionSelector>,
    Option<bool>,
    Option<bool>,
)> {
    let obj = value.as_object()?;
    if obj.get("type")?.as_str()? != "user_secret_ref" {
        return None;
    }
    let key = obj.get("key")?.as_str()?.trim().to_string();
    if key.is_empty() {
        return None;
    }
    let version_selector = match obj.get("version") {
        Some(v) => serde_json::from_value(v.clone()).ok()?,
        None => SecretVersionSelector::default(),
    };
    let required = obj.get("required").and_then(|v| v.as_bool());
    let allow_missing_override = obj.get("allowMissingOverride").and_then(|v| v.as_bool());
    Some((
        key,
        Some(version_selector),
        required,
        allow_missing_override,
    ))
}

pub fn is_env_binding(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let Some(kind) = obj.get("type").and_then(|v| v.as_str()) else {
        return false;
    };
    matches!(kind, "secret_ref" | "user_secret_ref" | "plain")
}

/// 提取 `adapter_config` 中所有 `secret_ref` 绑定。
///
/// 遍历 `env` 字段的每个子键（路径 `env.<KEY>`）+ 顶层除 `env` 外的
/// 每个字段（路径 `<KEY>`），对每个值尝试解析为 `secret_ref` binding。
pub fn collect_secret_refs(adapter_config: &Value) -> Vec<SecretRef> {
    let mut refs = Vec::new();
    let Some(config) = as_record(adapter_config) else {
        return refs;
    };

    if let Some(env_value) = config.get("env").and_then(as_record) {
        for (key, raw_binding) in env_value {
            if let Some((secret_id, version_selector, projection_class, projection_allowlist_key)) =
                try_parse_secret_ref(raw_binding)
            {
                refs.push(SecretRef {
                    secret_id,
                    config_path: format!("env.{key}"),
                    version_selector: version_selector.unwrap_or_default(),
                    projection_class,
                    projection_allowlist_key,
                });
            }
        }
    }

    for (key, raw_binding) in config {
        if key == "env" {
            continue;
        }
        if let Some((secret_id, version_selector, projection_class, projection_allowlist_key)) =
            try_parse_secret_ref(raw_binding)
        {
            refs.push(SecretRef {
                secret_id,
                config_path: key.clone(),
                version_selector: version_selector.unwrap_or_default(),
                projection_class,
                projection_allowlist_key,
            });
        }
    }

    refs
}

/// 提取 `adapter_config` 中所有 `user_secret_ref` 绑定。算法同上。
pub fn collect_user_secret_refs(adapter_config: &Value) -> Vec<UserSecretRef> {
    let mut refs = Vec::new();
    let Some(config) = as_record(adapter_config) else {
        return refs;
    };

    if let Some(env_value) = config.get("env").and_then(as_record) {
        for (key, raw_binding) in env_value {
            if let Some((definition_key, version_selector, required, allow_missing_override)) =
                try_parse_user_secret_ref(raw_binding)
            {
                refs.push(UserSecretRef {
                    definition_key,
                    config_path: format!("env.{key}"),
                    env_key: key.clone(),
                    version_selector: version_selector.unwrap_or_default(),
                    required: required.unwrap_or(true),
                    allow_missing_override: allow_missing_override.unwrap_or(false),
                });
            }
        }
    }

    for (key, raw_binding) in config {
        if key == "env" {
            continue;
        }
        if let Some((definition_key, version_selector, required, allow_missing_override)) =
            try_parse_user_secret_ref(raw_binding)
        {
            refs.push(UserSecretRef {
                definition_key,
                config_path: key.clone(),
                env_key: key.clone(),
                version_selector: version_selector.unwrap_or_default(),
                required: required.unwrap_or(true),
                allow_missing_override: allow_missing_override.unwrap_or(false),
            });
        }
    }

    refs
}

/// 把所有 ref 通过 `secretsSvc` 写回。算法与 Node 端一致：
/// 1. 优先用 `sync_secret_refs_for_target`（精细版，按 ref 写）
/// 2. 否则用 `sync_env_bindings_for_target`（粗粒度，整对象写）
pub async fn sync_agent_adapter_env_bindings<S: AgentSecretBindingSync>(
    secrets_svc: &S,
    company_id: uuid::Uuid,
    agent_id: &str,
    adapter_config: &Value,
) -> Result<(), String> {
    // 优先：精细同步
    let refs = collect_secret_refs(adapter_config);
    let user_refs = collect_user_secret_refs(adapter_config);
    // 我们用 Any 风格不够优雅；改成检查 trait 方法是否可用的方式。
    // 简单做法：调用方实现两个方法就算支持精细版，否则只提供
    // `sync_env_bindings_for_target`。但 async_trait 没有"可选方法"。
    // 借鉴 Node：service 同时提供两类方法时走精细版。所以调用方选择 trait impl。
    secrets_svc
        .sync_secret_refs_for_target(
            company_id,
            (SecretBindingTargetType::Agent, agent_id.to_string()),
            &refs,
            SyncOptions { replace_all: true },
        )
        .await?;
    secrets_svc
        .sync_user_secret_declarations_for_target(
            company_id,
            (SecretBindingTargetType::Agent, agent_id.to_string()),
            &user_refs,
            SyncOptions { replace_all: true },
        )
        .await?;
    Ok(())
}

/// 把整个 `env` 块作为 Value 写入（粗粒度）。在 secrets service 不支持
/// 精细版本时使用。
pub async fn sync_agent_env_value_only<S: AgentSecretBindingSync>(
    secrets_svc: &S,
    company_id: uuid::Uuid,
    agent_id: &str,
    adapter_config: &Value,
) -> Result<(), String> {
    let env_value = adapter_config
        .as_object()
        .and_then(|o| o.get("env"))
        .cloned()
        .unwrap_or(Value::Null);
    secrets_svc
        .sync_env_bindings_for_target(
            company_id,
            (SecretBindingTargetType::Agent, agent_id.to_string(), None),
            env_value,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn secret_ref_in_env_is_extracted_with_env_prefix() {
        let cfg = json!({
            "env": {
                "OPENAI_API_KEY": { "type": "secret_ref", "secretId": "sec-1" }
            }
        });
        let refs = collect_secret_refs(&cfg);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].secret_id, "sec-1");
        assert_eq!(refs[0].config_path, "env.OPENAI_API_KEY");
        assert_eq!(refs[0].version_selector, SecretVersionSelector::default());
    }

    #[test]
    fn secret_ref_at_top_level_is_extracted() {
        let cfg = json!({
            "apiKey": { "type": "secret_ref", "secretId": "sec-2", "version": 3 }
        });
        let refs = collect_secret_refs(&cfg);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].config_path, "apiKey");
        assert_eq!(refs[0].version_selector, SecretVersionSelector::Number(3));
    }

    #[test]
    fn plain_bindings_are_skipped() {
        let cfg = json!({
            "env": {
                "DEBUG": { "type": "plain", "value": "true" },
                "STR": "legacy",
                "TOK": { "type": "secret_ref", "secretId": "sec-3" }
            }
        });
        let refs = collect_secret_refs(&cfg);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].secret_id, "sec-3");
    }

    #[test]
    fn invalid_secret_ref_is_skipped() {
        let cfg = json!({
            "env": {
                "BAD1": { "type": "secret_ref" },
                "BAD2": { "type": "secret_ref", "secretId": "" },
                "OK": { "type": "secret_ref", "secretId": "sec-x" }
            }
        });
        let refs = collect_secret_refs(&cfg);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].secret_id, "sec-x");
    }

    #[test]
    fn user_secret_ref_extracted_with_required_defaults() {
        let cfg = json!({
            "env": {
                "GH_TOKEN": { "type": "user_secret_ref", "key": "github.token" }
            }
        });
        let refs = collect_user_secret_refs(&cfg);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].definition_key, "github.token");
        assert_eq!(refs[0].env_key, "GH_TOKEN");
        assert_eq!(refs[0].config_path, "env.GH_TOKEN");
        assert!(refs[0].required);
        assert!(!refs[0].allow_missing_override);
    }

    #[test]
    fn user_secret_ref_respects_explicit_required_and_allow_missing() {
        let cfg = json!({
            "env": {
                "GH": {
                    "type": "user_secret_ref",
                    "key": "github.token",
                    "required": false,
                    "allowMissingOverride": true,
                    "version": "latest"
                }
            }
        });
        let refs = collect_user_secret_refs(&cfg);
        assert_eq!(refs.len(), 1);
        assert!(!refs[0].required);
        assert!(refs[0].allow_missing_override);
        assert_eq!(refs[0].version_selector, SecretVersionSelector::default());
    }

    #[test]
    fn mixed_env_and_top_level_refs_are_all_extracted() {
        let cfg = json!({
            "env": {
                "A": { "type": "secret_ref", "secretId": "sec-A" },
                "B": { "type": "user_secret_ref", "key": "u-B" }
            },
            "C": { "type": "secret_ref", "secretId": "sec-C" },
            "D": { "type": "user_secret_ref", "key": "u-D" }
        });
        let secret_refs = collect_secret_refs(&cfg);
        let user_refs = collect_user_secret_refs(&cfg);
        assert_eq!(secret_refs.len(), 2);
        assert_eq!(user_refs.len(), 2);
        assert!(secret_refs.iter().any(|r| r.config_path == "env.A"));
        assert!(secret_refs.iter().any(|r| r.config_path == "C"));
        assert!(user_refs.iter().any(|r| r.config_path == "env.B"));
        assert!(user_refs.iter().any(|r| r.config_path == "D"));
    }

    #[test]
    fn non_object_config_returns_empty() {
        let cases = [json!(null), json!(42), json!("string"), json!(true)];
        for v in &cases {
            assert!(collect_secret_refs(v).is_empty());
            assert!(collect_user_secret_refs(v).is_empty());
        }
    }

    #[test]
    fn matches_env_binding_recognizes_known_types() {
        assert!(is_env_binding(
            &json!({"type": "secret_ref", "secretId": "x"})
        ));
        assert!(is_env_binding(
            &json!({"type": "user_secret_ref", "key": "x"})
        ));
        assert!(is_env_binding(&json!({"type": "plain", "value": "x"})));
        assert!(!is_env_binding(&json!({"type": "unknown", "value": "x"})));
        assert!(!is_env_binding(&json!("legacy-string")));
        assert!(!is_env_binding(&json!(null)));
    }

    #[test]
    fn version_selector_default_is_latest() {
        assert_eq!(
            SecretVersionSelector::default(),
            SecretVersionSelector::Latest(LatestVersion::new())
        );
    }

    #[test]
    fn version_selector_serializes_to_untagged_form() {
        let n: SecretVersionSelector = serde_json::from_value(json!(3)).unwrap();
        assert_eq!(n, SecretVersionSelector::Number(3));
        let l: SecretVersionSelector = serde_json::from_value(json!("latest")).unwrap();
        assert_eq!(l, SecretVersionSelector::Latest(LatestVersion::new()));
        // 非法字符串应失败
        let r: Result<SecretVersionSelector, _> = serde_json::from_value(json!("v1"));
        assert!(r.is_err());
    }
}
