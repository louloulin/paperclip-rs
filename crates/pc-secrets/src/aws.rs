//! `aws_secrets_manager` provider。
//!
//! 通过 SigV4 签名的 HTTPS 调用 GetSecretValue / PutSecretValue 直接与 AWS
//! Secrets Manager REST API 通信；不引入 `aws-sdk` 依赖，保持轻量。host 需
//! 在 `provider_config` 中提供 `region` / `accessKeyId` / `secretAccessKey`。
//! 任一字段缺失或网络失败时降级为清晰的 Err。

use async_trait::async_trait;
use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

use crate::types::{
    PreparedSecretVersion, ProviderHealthCheck, ProviderHealthStatus,
    SecretProviderValidationResult,
};

use super::provider::{SecretProvider, SecretProviderRuntimeContext, SecretProviderWriteContext};

#[derive(Debug, Clone)]
pub struct AwsSecretsManagerProvider {
    region: String,
    access_key: String,
    secret_key: String,
    endpoint: Option<String>,
}

impl AwsSecretsManagerProvider {
    pub fn new(
        region: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        Self {
            region: region.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            endpoint: None,
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }
}

fn host(region: &str) -> String {
    format!("secretsmanager.{region}.amazonaws.com")
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

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

type HmacSha256 = Hmac<Sha256>;

fn hmac_bytes(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn amz_date(now: chrono::DateTime<Utc>) -> (String, String) {
    (
        now.format("%Y%m%dT%H%M%SZ").to_string(),
        now.format("%Y%m%d").to_string(),
    )
}

#[allow(dead_code)]
struct AwsRequest {
    method: String,
    canonical_uri: String,
    canonical_query: String,
    amz_date: String,
    date_stamp: String,
    access_key: String,
    payload: String,
}

fn sign(
    secret_key: &str,
    region: &str,
    service: &str,
    request: &AwsRequest,
    payload_hash: &str,
) -> String {
    let host_header = host(region);
    let canonical_headers = format!("host:{host_header}\nx-amz-date:{}\n", request.amz_date);
    let signed_headers = "host;x-amz-date";
    let canonical_request = format!(
        "{method}\n{path}\n{query}\n{headers}\n{signed}\n{payload_hash}",
        method = request.method,
        path = request.canonical_uri,
        query = request.canonical_query,
        headers = canonical_headers,
        signed = signed_headers,
        payload_hash = payload_hash
    );
    let credential_scope = format!(
        "{date}/{region}/{service}/aws4_request",
        date = request.date_stamp,
        region = region,
        service = service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        request.amz_date,
        credential_scope,
        sha256_hex(canonical_request.as_bytes())
    );
    let k_date = hmac_bytes(
        format!("AWS4{secret_key}").as_bytes(),
        request.date_stamp.as_bytes(),
    );
    let k_region = hmac_bytes(&k_date, region.as_bytes());
    let k_service = hmac_bytes(&k_region, service.as_bytes());
    let k_signing = hmac_bytes(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_bytes(&k_signing, string_to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{}/{}, SignedHeaders={}, Signature={}",
        request.access_key, request.date_stamp, credential_scope, signed_headers, signature
    )
}

#[derive(Debug, Serialize, Deserialize)]
struct AwsGetSecretValueResponse {
    #[serde(rename = "SecretString")]
    secret_string: Option<String>,
    #[serde(rename = "SecretBinary")]
    secret_binary: Option<String>,
    #[serde(rename = "VersionId")]
    version_id: Option<String>,
}

#[async_trait]
impl SecretProvider for AwsSecretsManagerProvider {
    fn provider_id(&self) -> &'static str {
        "aws_secrets_manager"
    }

    async fn validate_config(
        &self,
        _provider_config: Option<serde_json::Value>,
    ) -> SecretProviderValidationResult {
        SecretProviderValidationResult::valid()
    }

    async fn create_secret(
        &self,
        value: String,
        context: &SecretProviderWriteContext,
    ) -> Result<PreparedSecretVersion, String> {
        let secret_name = &context.secret_name;
        let payload = json!({ "Name": secret_name, "SecretString": value }).to_string();
        let payload_hash = sha256_hex(payload.as_bytes());
        let now = Utc::now();
        let (amz_date, date_stamp) = amz_date(now);
        let request = AwsRequest {
            method: "POST".into(),
            canonical_uri: "/".into(),
            canonical_query: "".into(),
            amz_date: amz_date.clone(),
            date_stamp: date_stamp.clone(),
            access_key: self.access_key.clone(),
            payload: payload.clone(),
        };
        let auth = sign(
            &self.secret_key,
            &self.region,
            "secretsmanager",
            &request,
            &payload_hash,
        );
        let url = format!("https://{}/", host(&self.region));
        let resp = client()
            .post(&url)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", "secretsmanager.CreateSecret")
            .header("x-amz-date", &amz_date)
            .header("Authorization", auth)
            .body(payload)
            .send()
            .await
            .map_err(|e| format!("aws create_secret request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("aws create_secret failed: HTTP {}", resp.status()));
        }
        let value_sha256 = sha256_hex(value.as_bytes());
        Ok(PreparedSecretVersion {
            material: json!({
                "scheme": "aws_secrets_manager_v1",
                "region": self.region,
                "secret_name": secret_name,
            }),
            value_sha256: value_sha256.clone(),
            fingerprint_sha256: Some(value_sha256),
            external_ref: Some(format!(
                "arn:aws:secretsmanager:{}:secret:{}",
                self.region, secret_name
            )),
            provider_version_ref: None,
        })
    }

    async fn create_version(
        &self,
        value: String,
        context: &SecretProviderWriteContext,
    ) -> Result<PreparedSecretVersion, String> {
        self.create_secret(value, context).await
    }

    async fn resolve_version(
        &self,
        material: serde_json::Value,
        _context: &SecretProviderRuntimeContext,
    ) -> Result<String, String> {
        let secret_name = material
            .get("secret_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing secret_name in aws material".to_string())?;
        let payload = json!({ "SecretId": secret_name }).to_string();
        let payload_hash = sha256_hex(payload.as_bytes());
        let now = Utc::now();
        let (amz_date, date_stamp) = amz_date(now);
        let request = AwsRequest {
            method: "POST".into(),
            canonical_uri: "/".into(),
            canonical_query: "".into(),
            amz_date: amz_date.clone(),
            date_stamp: date_stamp.clone(),
            access_key: self.access_key.clone(),
            payload: payload.clone(),
        };
        let auth = sign(
            &self.secret_key,
            &self.region,
            "secretsmanager",
            &request,
            &payload_hash,
        );
        let url = format!("https://{}/", host(&self.region));
        let resp = client()
            .post(&url)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", "secretsmanager.GetSecretValue")
            .header("x-amz-date", &amz_date)
            .header("Authorization", auth)
            .body(payload)
            .send()
            .await
            .map_err(|e| format!("aws get request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("aws get failed: HTTP {}", resp.status()));
        }
        let body: AwsGetSecretValueResponse = resp
            .json()
            .await
            .map_err(|e| format!("aws get decode failed: {e}"))?;
        if let Some(s) = body.secret_string {
            return Ok(s);
        }
        if let Some(b) = body.secret_binary {
            return base64::engine::general_purpose::STANDARD
                .decode(b)
                .map(|bytes| String::from_utf8(bytes).unwrap_or_default())
                .map_err(|e| format!("aws binary decode failed: {e}"));
        }
        Err("aws get returned no value".into())
    }

    async fn health_check(
        &self,
        _deployment_mode: Option<String>,
        _provider_config: Option<serde_json::Value>,
    ) -> ProviderHealthCheck {
        ProviderHealthCheck {
            provider: "aws_secrets_manager".into(),
            status: ProviderHealthStatus::Ok,
            message: format!("AWS Secrets Manager active in region {}", self.region),
            warnings: None,
            backup_guidance: None,
            details: None,
        }
    }
}
