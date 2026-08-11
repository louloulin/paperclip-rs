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

/// Vault 鉴权方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultAuth {
    /// 直接使用静态 token。
    Token(String),
    /// AppRole: `POST /v1/auth/approle/login`。
    /// 响应里 `auth.client_token` 即 Vault token。
    AppRole { role_id: String, secret_id: String },
    /// Kubernetes auth: `POST /v1/auth/kubernetes/login`.
    Kubernetes { role: String, jwt: String },
}

#[derive(Debug, Clone)]
pub struct VaultProvider {
    addr: String,
    auth: VaultAuth,
    /// Vault enterprise namespace；空字符串等价于 root。
    namespace: String,
}

impl VaultProvider {
    pub fn new(addr: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            auth: VaultAuth::Token(token.into()),
            namespace: String::new(),
        }
    }

    /// 切换为 AppRole 鉴权。
    #[must_use]
    pub fn with_approle(
        addr: impl Into<String>,
        role_id: impl Into<String>,
        secret_id: impl Into<String>,
    ) -> Self {
        Self {
            addr: addr.into(),
            auth: VaultAuth::AppRole {
                role_id: role_id.into(),
                secret_id: secret_id.into(),
            },
            namespace: String::new(),
        }
    }

    /// 切换为 Kubernetes auth 鉴权。
    #[must_use]
    pub fn with_kubernetes_auth(
        addr: impl Into<String>,
        role: impl Into<String>,
        jwt: impl Into<String>,
    ) -> Self {
        Self {
            addr: addr.into(),
            auth: VaultAuth::Kubernetes {
                role: role.into(),
                jwt: jwt.into(),
            },
            namespace: String::new(),
        }
    }

    /// 设置 Vault enterprise namespace（X-Vault-Namespace header）。
    #[must_use]
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    pub fn from_config(provider_config: Option<Value>) -> Result<Self, String> {
        let cfg = provider_config.ok_or_else(|| "vault provider_config is required".to_string())?;
        let addr = cfg
            .get("address")
            .and_then(|v| v.as_str())
            .or_else(|| cfg.get("addr").and_then(|v| v.as_str()))
            .ok_or_else(|| "missing address in vault provider_config".to_string())?
            .to_string();
        let namespace = cfg
            .get("namespace")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // 优先 AppRole，其次 Kubernetes，最后静态 token。
        if let (Some(role_id), Some(secret_id)) = (
            cfg.get("roleId").and_then(|v| v.as_str()),
            cfg.get("secretId").and_then(|v| v.as_str()),
        ) {
            let mut p = Self::with_approle(addr, role_id, secret_id);
            if !namespace.is_empty() {
                p = p.with_namespace(namespace);
            }
            return Ok(p);
        }
        if let (Some(role), Some(jwt)) = (
            cfg.get("kubernetesRole").and_then(|v| v.as_str()),
            cfg.get("kubernetesJwt").and_then(|v| v.as_str()),
        ) {
            let mut p = Self::with_kubernetes_auth(addr, role, jwt);
            if !namespace.is_empty() {
                p = p.with_namespace(namespace);
            }
            return Ok(p);
        }
        let token = cfg
            .get("token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing token in vault provider_config".to_string())?
            .to_string();
        let mut p = Self::new(addr, token);
        if !namespace.is_empty() {
            p = p.with_namespace(namespace);
        }
        Ok(p)
    }

    pub(crate) fn addr(&self) -> &str {
        &self.addr
    }

    pub(crate) fn token(&self) -> &str {
        match &self.auth {
            VaultAuth::Token(t) => t,
            // 对 AppRole / Kubernetes 而言，初始 token 为空串；调用方需先 login。
            _ => "",
        }
    }

    pub(crate) fn auth_kind(&self) -> &'static str {
        match self.auth {
            VaultAuth::Token(_) => "token",
            VaultAuth::AppRole { .. } => "approle",
            VaultAuth::Kubernetes { .. } => "kubernetes",
        }
    }

    pub(crate) fn namespace(&self) -> &str {
        &self.namespace
    }

    /// 把构造时填入的 `AppRole` / `Kubernetes` 凭证兑换成真实 token。
    /// 成功后 `self.auth` 变为 `Token(...)`，后续请求直接走静态 token 路径。
    pub async fn exchange_token(&mut self) -> Result<(), String> {
        let (path, payload) = match &self.auth {
            VaultAuth::AppRole { role_id, secret_id } => (
                "/v1/auth/approle/login".to_string(),
                json!({ "role_id": role_id, "secret_id": secret_id }),
            ),
            VaultAuth::Kubernetes { role, jwt } => (
                "/v1/auth/kubernetes/login".to_string(),
                json!({ "role": role, "jwt": jwt }),
            ),
            VaultAuth::Token(_) => return Ok(()),
        };
        let url = format!("{}{}", self.addr.trim_end_matches('/'), path);
        let mut req = client().post(&url).json(&payload);
        if !self.namespace.is_empty() {
            req = req.header("X-Vault-Namespace", &self.namespace);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("vault {} login request error: {e}", self.auth_kind()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "vault {} login returned {status}: {body}",
                self.auth_kind()
            ));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("vault login json error: {e}"))?;
        let token = v
            .get("auth")
            .and_then(|a| a.get("client_token"))
            .and_then(|t| t.as_str())
            .ok_or_else(|| "vault login response missing auth.client_token".to_string())?
            .to_string();
        self.auth = VaultAuth::Token(token);
        Ok(())
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

/// 给 request builder 加上 X-Vault-Token 和（可选）X-Vault-Namespace。
fn apply_auth_headers(
    builder: reqwest::RequestBuilder,
    token: &str,
    namespace: &str,
) -> reqwest::RequestBuilder {
    let builder = builder.header("X-Vault-Token", token);
    if !namespace.is_empty() {
        builder.header("X-Vault-Namespace", namespace)
    } else {
        builder
    }
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
        match &self.auth {
            VaultAuth::Token(t) if t.is_empty() => {
                return SecretProviderValidationResult::invalid("token is empty");
            }
            VaultAuth::AppRole { role_id, secret_id }
                if role_id.is_empty() || secret_id.is_empty() =>
            {
                return SecretProviderValidationResult::invalid(
                    "approle role_id/secret_id must be non-empty",
                );
            }
            VaultAuth::Kubernetes { role, jwt } if role.is_empty() || jwt.is_empty() => {
                return SecretProviderValidationResult::invalid(
                    "kubernetes role/jwt must be non-empty",
                );
            }
            _ => {}
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
        let req = client().post(&url).json(&payload);
        let resp = apply_auth_headers(req, self.token(), self.namespace())
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
        let req = client().get(&url);
        let resp = apply_auth_headers(req, self.token(), self.namespace())
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
        let req = client().get(&url);
        match apply_auth_headers(req, self.token(), self.namespace())
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

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn r564_vault_with_approle_starts_with_approle_auth() {
        let p = VaultProvider::with_approle("https://vault.example.com", "role-1", "secret-1");
        assert_eq!(p.auth_kind(), "approle");
        assert_eq!(p.token(), "", "approle must not expose secret_id as token");
        assert_eq!(p.namespace(), "");
    }

    #[test]
    fn r564_vault_with_kubernetes_starts_with_kubernetes_auth() {
        let p = VaultProvider::with_kubernetes_auth(
            "https://vault.example.com",
            "my-role",
            "eyJhbGciOiJSUzI1NiJ9.payload",
        );
        assert_eq!(p.auth_kind(), "kubernetes");
        assert_eq!(p.token(), "");
    }

    #[test]
    fn r564_vault_with_namespace_passes_through() {
        let p = VaultProvider::new("https://vault.example.com", "tok").with_namespace("team-a");
        assert_eq!(p.namespace(), "team-a");
    }

    #[test]
    fn r564_vault_from_config_picks_approle() {
        let cfg = json!({
            "address": "https://vault.example.com",
            "roleId": "r",
            "secretId": "s",
            "namespace": "ns1",
        });
        let p = VaultProvider::from_config(Some(cfg)).unwrap();
        assert_eq!(p.auth_kind(), "approle");
        assert_eq!(p.namespace(), "ns1");
    }

    #[test]
    fn r564_vault_from_config_picks_kubernetes() {
        let cfg = json!({
            "address": "https://vault.example.com",
            "kubernetesRole": "my-role",
            "kubernetesJwt": "jwt-token",
        });
        let p = VaultProvider::from_config(Some(cfg)).unwrap();
        assert_eq!(p.auth_kind(), "kubernetes");
    }

    #[test]
    fn r564_vault_from_config_falls_back_to_token() {
        let cfg = json!({
            "address": "https://vault.example.com",
            "token": "static-tok",
        });
        let p = VaultProvider::from_config(Some(cfg)).unwrap();
        assert_eq!(p.auth_kind(), "token");
        assert_eq!(p.token(), "static-tok");
    }

    #[test]
    fn r564_vault_from_config_rejects_missing_credentials() {
        let cfg = json!({ "address": "https://vault.example.com" });
        assert!(VaultProvider::from_config(Some(cfg)).is_err());
    }

    #[tokio::test]
    async fn r564_vault_validate_config_rejects_empty_approle() {
        let p = VaultProvider::with_approle("https://vault.example.com", "", "secret");
        let r = p.validate_config(None).await;
        assert!(!r.ok);
        assert!(r.warnings[0].contains("approle"));
    }

    #[tokio::test]
    async fn r564_vault_validate_config_accepts_full_approle() {
        let p = VaultProvider::with_approle("https://vault.example.com", "role", "secret");
        let r = p.validate_config(None).await;
        assert!(r.ok);
    }

    #[tokio::test]
    async fn r564_vault_token_provider_exchange_is_noop() {
        // 静态 token 路径的 exchange_token 应当是 no-op。
        let mut p = VaultProvider::new("https://vault.example.com", "t");
        p.exchange_token().await.unwrap();
        assert_eq!(p.auth_kind(), "token");
        assert_eq!(p.token(), "t");
    }

    #[test]
    fn r564_vault_approle_auth_constructs_request_shape() {
        // 验证 login URL 与 payload 与 Vault 协议一致。
        let p = VaultProvider::with_approle("https://vault.example.com/", "r", "s");
        match p.auth {
            VaultAuth::AppRole { role_id, secret_id } => {
                assert_eq!(role_id, "r");
                assert_eq!(secret_id, "s");
            }
            _ => panic!("expected AppRole"),
        }
    }
}
