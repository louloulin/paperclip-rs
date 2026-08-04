//! `vault` provider — HashiCorp Vault KV v2.
//!
//! 通过 HTTPS 调用 Vault HTTP API：
//! `PUT  {addr}/v1/secret/data/{path}` — 写
//! `GET  {addr}/v1/secret/data/{path}` — 读
//!
//! 鉴权：`X-Vault-Token` header（来自 `provider_config.token`，或环境 `VAULT_TOKEN`）。

use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::OnceLock;

use crate::types::{
    PreparedSecretVersion, ProviderHealthCheck, ProviderHealthStatus,
    SecretProviderValidationResult,
};

use super::provider::{SecretProvider, SecretProviderRuntimeContext, SecretProviderWriteContext};

#[derive(Debug, Clone)]
pub struct VaultProvider {
    addr: String,
    token: String,
}

impl VaultProvider {
    pub fn new(addr: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            token: token.into(),
        }
    }

    pub fn from_config(provider_config: Option<Value>) -> Result<Self, String> {
        let cfg = provider_config.ok_or_else(|| "vault provider_config is required".to_string())?;
        let addr = cfg
            .get("address")
            .and_then(|v| v.as_str())
            .or_else(|| cfg.get("addr").and_then(|v| v.as_str()))
            .ok_or_else(|| "missing address in vault provider_config".to_string())?
            .to_string();
        let token = cfg
            .get("token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing token in vault provider_config".to_string())?
            .to_string();
        Ok(Self::new(addr, token))
    }

    pub(crate) fn addr(&self) -> &str {
        &self.addr
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }
}

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client")
    })
}

/// path 限制：只允许 ASCII 字母数字 + `_` + `-` + `.` + `/`，长度 ≤ 1024。
fn sanitize_path(path: &str) -> Result<String, String> {
    if path.is_empty() || path.len() > 1024 {
        return Err("vault secret path must be 1..=1024 chars".into());
    }
    if path.starts_with('/') {
        return Err("vault secret path must not start with '/'".into());
    }
    if !path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
    {
        return Err("vault secret path contains forbidden chars".into());
    }
    Ok(path.to_string())
}

#[async_trait]
impl SecretProvider for VaultProvider {
    fn provider_id(&self) -> &'static str {
        "vault"
    }

    async fn validate_config(
        &self,
        _provider_config: Option<Value>,
    ) -> SecretProviderValidationResult {
        if self.addr().is_empty() {
            return SecretProviderValidationResult::invalid("address is empty");
        }
        if self.token().is_empty() {
            return SecretProviderValidationResult::invalid("token is empty");
        }
        SecretProviderValidationResult::valid()
    }

    async fn create_secret(
        &self,
        value: String,
        context: &SecretProviderWriteContext,
    ) -> Result<PreparedSecretVersion, String> {
        let path = sanitize_path(&context.secret_key)?;
        let url = format!(
            "{}/v1/secret/data/{}",
            self.addr().trim_end_matches('/'),
            path
        );
        let payload = json!({
            "data": {
                "value": &value,
            },
            "options": {
                "cas": 0,
            },
        });
        let resp = client()
            .post(&url)
            .header("X-Vault-Token", self.token())
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("vault write request error: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("vault write returned {status}: {body}"));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("vault write json error: {e}"))?;
        let version = v
            .get("data")
            .and_then(|d| d.get("version"))
            .and_then(|n| n.as_i64())
            .unwrap_or(1);
        let ciphertext_b64 = base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
        Ok(PreparedSecretVersion {
            material: json!({
                "scheme": "vault_kv_v2",
                "address": self.addr(),
                "path": path,
                "version": version,
                "ciphertext_base64": ciphertext_b64,
            }),
            value_sha256: crate::hmac_sha256(b"", value.as_bytes()),
            fingerprint_sha256: Some(format!("{path}@v{version}")),
            external_ref: None,
            provider_version_ref: Some(context.version.to_string()),
        })
    }

    async fn create_version(
        &self,
        value: String,
        context: &SecretProviderWriteContext,
    ) -> Result<PreparedSecretVersion, String> {
        // Vault KV v2 是写一次递增 version，与 create_secret 等价
        self.create_secret(value, context).await
    }

    async fn resolve_version(
        &self,
        material: Value,
        _context: &SecretProviderRuntimeContext,
    ) -> Result<String, String> {
        let path = material
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "vault material missing path".to_string())?;
        let url = format!(
            "{}/v1/secret/data/{}",
            self.addr().trim_end_matches('/'),
            path
        );
        let resp = client()
            .get(&url)
            .header("X-Vault-Token", self.token())
            .send()
            .await
            .map_err(|e| format!("vault read request error: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("vault read returned {status}: {body}"));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("vault read json error: {e}"))?;
        let value = v
            .get("data")
            .and_then(|d| d.get("data"))
            .and_then(|d| d.get("value"))
            .and_then(|x| x.as_str())
            .ok_or_else(|| "vault read response missing data.data.value".to_string())?;
        Ok(value.to_string())
    }

    async fn health_check(
        &self,
        _deployment_mode: Option<String>,
        _provider_config: Option<Value>,
    ) -> ProviderHealthCheck {
        // Vault health endpoint: GET /v1/sys/health
        let url = format!("{}/v1/sys/health", self.addr().trim_end_matches('/'));
        match client()
            .get(&url)
            .header("X-Vault-Token", self.token())
            .send()
            .await
        {
            Ok(r) => {
                let s = r.status();
                if s.is_success() {
                    ProviderHealthCheck::ok(
                        "vault".to_string(),
                        "Connected to HashiCorp Vault successfully.".to_string(),
                    )
                } else if s.as_u16() == 403 {
                    ProviderHealthCheck {
                        provider: "vault".to_string(),
                        status: ProviderHealthStatus::Error,
                        message: "Vault rejected the token (403).".to_string(),
                        warnings: None,
                        backup_guidance: None,
                        details: None,
                    }
                    .with_warnings(vec![
                        "Check X-Vault-Token validity and policy permissions.".into(),
                    ])
                } else if s.as_u16() == 429 {
                    ProviderHealthCheck {
                        provider: "vault".to_string(),
                        status: ProviderHealthStatus::Warn,
                        message: "Vault is sealed or in standby (429).".to_string(),
                        warnings: None,
                        backup_guidance: None,
                        details: None,
                    }
                    .with_warnings(vec!["Unseal the Vault or pick an active node.".into()])
                } else {
                    ProviderHealthCheck {
                        provider: "vault".to_string(),
                        status: ProviderHealthStatus::Warn,
                        message: format!("Vault returned {s}"),
                        warnings: None,
                        backup_guidance: None,
                        details: None,
                    }
                    .with_warnings(vec!["Provider reachable but returned non-success.".into()])
                }
            }
            Err(e) => ProviderHealthCheck {
                provider: "vault".to_string(),
                status: ProviderHealthStatus::Warn,
                message: format!("Vault unreachable: {e}"),
                warnings: None,
                backup_guidance: None,
                details: None,
            }
            .with_warnings(vec![
                "Network or DNS error; provider operations will fail.".into(),
            ]),
        }
    }
}

#[cfg(test)]
pub(crate) fn sanitize_path_for_test(path: &str) -> Result<String, String> {
    sanitize_path(path)
}
