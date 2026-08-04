use serde::{Deserialize, Serialize};

/// 本地加密材料（AES-256-GCM v1 scheme）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalEncryptedMaterial {
    pub scheme: String,
    pub iv: String,
    pub tag: String,
    pub ciphertext: String,
}

impl Default for LocalEncryptedMaterial {
    fn default() -> Self {
        Self {
            scheme: "local_encrypted_v1".into(),
            iv: String::new(),
            tag: String::new(),
            ciphertext: String::new(),
        }
    }
}

/// 已持久化（或已准备持久化）的秘密版本材料。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSecretVersionMaterial {
    #[serde(flatten)]
    pub inner: serde_json::Value,
}

/// 待持久化的完整秘密版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedSecretVersion {
    pub material: serde_json::Value,
    pub value_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint_sha256: Option<String>,
    pub external_ref: Option<String>,
    pub provider_version_ref: Option<String>,
}

/// 提供方健康状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderHealthStatus {
    Ok,
    Warn,
    Error,
}

/// 提供方健康检查结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHealthCheck {
    pub provider: String,
    pub status: ProviderHealthStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_guidance: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ProviderHealthCheck {
    /// 构造一个 OK 状态。
    #[must_use]
    pub fn ok(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            status: ProviderHealthStatus::Ok,
            message: message.into(),
            warnings: None,
            backup_guidance: None,
            details: None,
        }
    }

    /// 附加 warning。
    pub fn with_warnings(mut self, warnings: Vec<String>) -> Self {
        self.warnings = if warnings.is_empty() { None } else { Some(warnings) };
        self
    }
}

/// 配置校验结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretProviderValidationResult {
    pub ok: bool,
    pub warnings: Vec<String>,
}

impl SecretProviderValidationResult {
    #[must_use]
    pub fn valid() -> Self {
        Self {
            ok: true,
            warnings: Vec::new(),
        }
    }

    /// 构造一个 invalid 结果，可附带原因。
    #[must_use]
    pub fn invalid(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            warnings: vec![reason.into()],
        }
    }

    pub fn warn(&mut self, warning: String) {
        self.warnings.push(warning);
    }

    pub fn invalidate(&mut self) {
        self.ok = false;
    }
}
