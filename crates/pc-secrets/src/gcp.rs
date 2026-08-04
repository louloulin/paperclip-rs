//! `gcp_secret_manager` provider。
//!
//! 通过 HTTPS 直接调用 Google Secret Manager REST API（v1）：
//! `https://secretmanager.googleapis.com/v1/projects/{project}/secrets/{secret}/versions`。
//!
//! 鉴权：使用 `provider_config` 中的 `accessToken`（短期 OAuth2 access token），
//! 或者通过 metadata server 隐式获取（暂未实现，需要时扩展）。
//!
//! 任何缺失字段或网络失败降级为清晰 Err。

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
pub struct GcpSecretManagerProvider {
    project_id: String,
    access_token: String,
    endpoint: Option<String>,
}

impl GcpSecretManagerProvider {
    pub fn new(project_id: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            access_token: access_token.into(),
            endpoint: None,
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    pub fn from_config(provider_config: Option<Value>) -> Result<Self, String> {
        let cfg = provider_config
            .ok_or_else(|| "gcp_secret_manager provider_config is required".to_string())?;
        let project_id = cfg
            .get("projectId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing projectId in gcp provider_config".to_string())?
            .to_string();
        let access_token = cfg
            .get("accessToken")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing accessToken in gcp provider_config".to_string())?
            .to_string();
        let mut p = Self::new(project_id, access_token);
        if let Some(endpoint) = cfg.get("endpoint").and_then(|v| v.as_str()) {
            p = p.with_endpoint(endpoint);
        }
        Ok(p)
    }

    fn base_url(&self) -> String {
        self.endpoint
            .clone()
            .unwrap_or_else(|| "https://secretmanager.googleapis.com".into())
    }

    pub(crate) fn project(&self) -> &str {
        &self.project_id
    }

    pub(crate) fn token(&self) -> &str {
        &self.access_token
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

/// 对 name 做最严格的清洗：只允许字母数字 + `_` + `-` + `.` + `/` + `:`。
/// 避免 secret name 里出现控制字符 / SQL 注入字符。
fn sanitize_name(name: &str) -> Result<String, String> {
    if name.is_empty() || name.len() > 255 {
        return Err("gcp secret name must be 1..=255 chars".into());
    }
    if name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':'))
    {
        Ok(name.to_string())
    } else {
        Err("gcp secret name contains forbidden chars".into())
    }
}

fn payload_base64(value: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
}

#[async_trait]
impl SecretProvider for GcpSecretManagerProvider {
    fn provider_id(&self) -> &'static str {
        "gcp_secret_manager"
    }

    async fn validate_config(
        &self,
        _provider_config: Option<Value>,
    ) -> SecretProviderValidationResult {
        if self.project().is_empty() {
            return SecretProviderValidationResult::invalid("projectId is empty");
        }
        if self.token().is_empty() {
            return SecretProviderValidationResult::invalid("accessToken is empty");
        }
        SecretProviderValidationResult::valid()
    }

    async fn create_secret(
        &self,
        value: String,
        context: &SecretProviderWriteContext,
    ) -> Result<PreparedSecretVersion, String> {
        let secret_name = sanitize_name(&context.secret_name)?;
        let url = format!(
            "{}/v1/projects/{}/secrets/{}/addVersion",
            self.base_url(),
            self.project(),
            secret_name
        );
        let payload = json!({
            "payload": payload_base64(&value),
        });
        let resp = client()
            .post(&url)
            .bearer_auth(self.token())
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("gcp addVersion request error: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("gcp addVersion returned {status}: {body}"));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("gcp addVersion json error: {e}"))?;
        let version_name = v
            .get("name")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "gcp addVersion response missing name".to_string())?
            .to_string();
        Ok(PreparedSecretVersion {
            material: json!({
                "scheme": "gcp_secret_manager_v1",
                "project_id": self.project(),
                "resource_name": version_name,
                "ciphertext_base64": payload_base64(&value),
            }),
            value_sha256: crate::hmac_sha256(b"", value.as_bytes()),
            fingerprint_sha256: Some(version_name.to_string()),
            external_ref: None,
            provider_version_ref: Some(context.version.to_string()),
        })
    }

    async fn create_version(
        &self,
        value: String,
        context: &SecretProviderWriteContext,
    ) -> Result<PreparedSecretVersion, String> {
        // 同 create_secret：GCP 没有分离 create vs rotate，都走 addVersion
        self.create_secret(value, context).await
    }

    async fn resolve_version(
        &self,
        material: Value,
        _context: &SecretProviderRuntimeContext,
    ) -> Result<String, String> {
        let resource_name = material
            .get("resource_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "gcp material missing resource_name".to_string())?;
        let url = format!("{}/v1/{}:access", self.base_url(), resource_name);
        let resp = client()
            .get(&url)
            .bearer_auth(self.token())
            .send()
            .await
            .map_err(|e| format!("gcp access request error: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("gcp access returned {status}: {body}"));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("gcp access json error: {e}"))?;
        let payload_b64 = v
            .get("payload")
            .and_then(|p| p.get("data"))
            .and_then(|d| d.as_str())
            .ok_or_else(|| "gcp access response missing payload.data".to_string())?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload_b64)
            .map_err(|e| format!("gcp payload base64 decode error: {e}"))?;
        String::from_utf8(bytes).map_err(|e| format!("gcp payload utf8 error: {e}"))
    }

    async fn health_check(
        &self,
        _deployment_mode: Option<String>,
        _provider_config: Option<Value>,
    ) -> ProviderHealthCheck {
        // 用 list secrets 试探 API 权限。
        let url = format!(
            "{}/v1/projects/{}/secrets?pageSize=1",
            self.base_url(),
            self.project()
        );
        match client().get(&url).bearer_auth(self.token()).send().await {
            Ok(r) if r.status().is_success() => ProviderHealthCheck::ok(
                "gcp_secret_manager",
                "Connected to Google Secret Manager successfully.".to_string(),
            ),
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                let msg = format!("GCP API returned {status}");
                if status.as_u16() == 401 || status.as_u16() == 403 {
                    ProviderHealthCheck {
                        provider: "gcp_secret_manager".to_string(),
                        status: ProviderHealthStatus::Error,
                        message: format!("{msg}: {body}"),
                        warnings: None,
                        backup_guidance: None,
                        details: None,
                    }
                    .with_warnings(vec!["Check accessToken and project IAM bindings.".into()])
                } else {
                    ProviderHealthCheck {
                        provider: "gcp_secret_manager".to_string(),
                        status: ProviderHealthStatus::Warn,
                        message: format!("{msg}: {body}"),
                        warnings: None,
                        backup_guidance: None,
                        details: None,
                    }
                    .with_warnings(vec![
                        "Provider reachable but returned a non-success status.".into(),
                    ])
                }
            }
            Err(e) => ProviderHealthCheck {
                provider: "gcp_secret_manager".to_string(),
                status: ProviderHealthStatus::Warn,
                message: format!("GCP Secret Manager unreachable: {e}"),
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
pub(crate) fn sanitize_name_for_test(name: &str) -> Result<String, String> {
    sanitize_name(name)
}
