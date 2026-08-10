//! Types —— Agent secret binding DTOs.
//!
//! 与 Node `server/src/services/agent-secret-bindings.ts` 1:1 对齐。

use serde::{Deserialize, Serialize};

// ============================================================================
// Enums
// ============================================================================

/// Secret version selector（与 Node `secretVersionSelectorSchema` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretVersionSelector {
    /// 最新版本（Node `"latest"`）。
    Latest,
    /// 指定版本号。
    Version(i64),
}

impl SecretVersionSelector {
    /// 默认值（与 Node `version ?? "latest"` 1:1 对齐）。
    pub fn latest() -> Self {
        Self::Latest
    }

    pub fn as_str(&self) -> String {
        match self {
            Self::Latest => "latest".to_string(),
            Self::Version(v) => v.to_string(),
        }
    }
}

/// Secret projection class（与 Node `SECRET_PROJECTION_CLASSES` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretProjectionClass {
    Unclassified,
    #[serde(rename = "class_3_static_lease")]
    Class3StaticLease,
}

impl SecretProjectionClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unclassified => "unclassified",
            Self::Class3StaticLease => "class_3_static_lease",
        }
    }
}

// ============================================================================
// Refs
// ============================================================================

/// `secret_ref` binding（与 Node `SecretRef` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretRef {
    pub secret_id: String,
    pub config_path: String,
    pub version_selector: SecretVersionSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_class: Option<SecretProjectionClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_allowlist_key: Option<String>,
}

/// `user_secret_ref` binding（与 Node `UserSecretRef` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserSecretRef {
    pub definition_key: String,
    pub config_path: String,
    pub env_key: String,
    pub version_selector: SecretVersionSelector,
    pub required: bool,
    pub allow_missing_override: bool,
}

// ============================================================================
// Errors
// ============================================================================

/// Sync service error。
#[derive(Debug, thiserror::Error)]
pub enum SecretBindingError {
    #[error("secrets service error: {0}")]
    Service(String),
}

pub type SecretBindingResult<T> = Result<T, SecretBindingError>;

// ============================================================================
// Sync service trait
// ============================================================================

/// 注入的 secrets 同步服务（与 Node `AgentSecretBindingSyncService` 1:1 对齐）。
///
/// 本 trait 仅暴露 secret_refs / user_secret_declarations / env_bindings 三类同步能力。
/// 当 `sync_secret_refs` 为 None 时，`sync_agent_adapter_env_bindings` 会自动 fallback 到
/// `sync_env_bindings`（与 Node 端 `if (input.secretsSvc.syncSecretRefsForTarget) ...` 分支 1:1 对齐）。
#[async_trait::async_trait]
pub trait SecretBindingSync: Send + Sync {
    /// 同步 secret_ref bindings。
    async fn sync_secret_refs(
        &self,
        company_id: &str,
        target: BindingTarget<'_>,
        refs: &[SecretRef],
    ) -> SecretBindingResult<()>;

    /// 同步 user_secret_ref declarations。
    async fn sync_user_secret_declarations(
        &self,
        company_id: &str,
        target: BindingTarget<'_>,
        refs: &[UserSecretRef],
    ) -> SecretBindingResult<()>;

    /// 同步 env bindings（fallback 路径）。
    async fn sync_env_bindings(
        &self,
        company_id: &str,
        target: BindingTarget<'_>,
        env_value: serde_json::Value,
    ) -> SecretBindingResult<()>;
}

/// Binding target。
#[derive(Debug, Clone, Copy)]
pub struct BindingTarget<'a> {
    pub target_type: BindingTargetType,
    pub target_id: &'a str,
}

impl<'a> BindingTarget<'a> {
    pub fn agent(target_id: &'a str) -> Self {
        Self {
            target_type: BindingTargetType::Agent,
            target_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingTargetType {
    Agent,
}
