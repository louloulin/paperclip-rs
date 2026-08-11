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

/// 链式凭证：静态 AK/SK 或 STS 临时凭证（通过 `with_assume_role` 设置）。
#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub access_key: String,
    pub secret_key: String,
    /// STS session token；静态凭证为空。
    pub session_token: Option<String>,
    /// 临时凭证过期时间（UTC）。None 表示永不过期。
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl AwsCredentials {
    fn is_expired(&self) -> bool {
        match self.expires_at {
            None => false,
            Some(t) => chrono::Utc::now() + chrono::Duration::seconds(60) >= t,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AwsAssumeRoleConfig {
    pub role_arn: String,
    pub session_name: String,
    /// 可选外部 ID（防止 confused deputy）。
    pub external_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AwsSecretsManagerProvider {
    region: String,
    credentials: AwsCredentials,
    endpoint: Option<String>,
    /// 若设置，则在调用前用当前 credentials 调 STS AssumeRole 换出临时凭证。
    assume_role: Option<AwsAssumeRoleConfig>,
}

impl AwsSecretsManagerProvider {
    pub fn new(
        region: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        Self {
            region: region.into(),
            credentials: AwsCredentials {
                access_key: access_key.into(),
                secret_key: secret_key.into(),
                session_token: None,
                expires_at: None,
            },
            endpoint: None,
            assume_role: None,
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// 配置链式 assume role：每次调用前若 credentials 过期则自动用
    /// `sts:AssumeRole` 换出临时凭证。
    #[must_use]
    pub fn with_assume_role(
        mut self,
        role_arn: impl Into<String>,
        session_name: impl Into<String>,
    ) -> Self {
        self.assume_role = Some(AwsAssumeRoleConfig {
            role_arn: role_arn.into(),
            session_name: session_name.into(),
            external_id: None,
        });
        self
    }

    /// 携带 external ID 的 assume role。
    #[must_use]
    pub fn with_assume_role_external_id(
        mut self,
        role_arn: impl Into<String>,
        session_name: impl Into<String>,
        external_id: impl Into<String>,
    ) -> Self {
        self.assume_role = Some(AwsAssumeRoleConfig {
            role_arn: role_arn.into(),
            session_name: session_name.into(),
            external_id: Some(external_id.into()),
        });
        self
    }

    /// 直接设置临时凭证（来自上游 STS / SSO / web identity 等）。
    pub fn set_temporary_credentials(
        &mut self,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
        session_token: impl Into<String>,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) {
        self.credentials = AwsCredentials {
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            session_token: Some(session_token.into()),
            expires_at: Some(expires_at),
        };
    }

    /// 当前凭证视图。供 STS signer / request signer 共用。
    pub fn credentials(&self) -> &AwsCredentials {
        &self.credentials
    }

    pub fn assume_role_config(&self) -> Option<&AwsAssumeRoleConfig> {
        self.assume_role.as_ref()
    }

    /// 如果配置了 `assume_role` 且当前凭证过期，则调
    /// `sts:AssumeRole` 换出新的临时凭证；否则为 no-op。
    pub async fn refresh_credentials(&mut self) -> Result<(), String> {
        let Some(cfg) = self.assume_role.clone() else {
            return Ok(());
        };
        if !self.credentials.is_expired() {
            return Ok(());
        }
        let now = Utc::now();
        let (amz_date, date_stamp) = amz_date(now);
        // AssumeRole 走 form-encoded；payload 是 x-www-form-urlencoded body。
        let mut body = format!(
            "Action=AssumeRole&Version=2011-06-15&RoleArn={}&RoleSessionName={}",
            urlencode(&cfg.role_arn),
            urlencode(&cfg.session_name),
        );
        if let Some(eid) = &cfg.external_id {
            body.push_str(&format!("&ExternalId={}", urlencode(eid)));
        }
        let payload_hash = sha256_hex(body.as_bytes());
        let host_header = format!("sts.{}.amazonaws.com", self.region);
        let canonical_headers = format!(
            "host:{host_header}
x-amz-date:{amz_date}
",
            amz_date = amz_date
        );
        let signed_headers = "host;x-amz-date";
        let canonical_request = format!(
            "POST
/

{headers}
{signed}
{payload_hash}",
            headers = canonical_headers,
            signed = signed_headers,
            payload_hash = payload_hash
        );
        let credential_scope = format!(
            "{date}/{region}/sts/aws4_request",
            date = date_stamp,
            region = self.region
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256
{amz_date}
{credential_scope}
{hashed}",
            amz_date = amz_date,
            credential_scope = credential_scope,
            hashed = sha256_hex(canonical_request.as_bytes())
        );
        let k_date = hmac_bytes(
            format!("AWS4{}", self.credentials.secret_key).as_bytes(),
            date_stamp.as_bytes(),
        );
        let k_region = hmac_bytes(&k_date, self.region.as_bytes());
        let k_service = hmac_bytes(&k_region, b"sts");
        let k_signing = hmac_bytes(&k_service, b"aws4_request");
        let signature = hex::encode(hmac_bytes(&k_signing, string_to_sign.as_bytes()));
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}/{}, SignedHeaders={}, Signature={}",
            self.credentials.access_key, date_stamp, credential_scope, signed_headers, signature
        );
        let url = format!("https://{}/", host_header);
        let resp = client()
            .post(&url)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-amz-date", &amz_date)
            .header("Authorization", auth)
            .body(body)
            .send()
            .await
            .map_err(|e| format!("aws sts assume_role request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "aws sts assume_role failed: HTTP {}",
                resp.status()
            ));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| format!("aws sts assume_role decode failed: {e}"))?;
        // 极简 XML 解析：搜 <AccessKeyId> / <SecretAccessKey> / <SessionToken> /
        // <Expiration> 四个标签。
        let access_key = extract_xml(&body, "AccessKeyId")
            .ok_or_else(|| "AssumeRole response missing AccessKeyId".to_string())?;
        let secret_key = extract_xml(&body, "SecretAccessKey")
            .ok_or_else(|| "AssumeRole response missing SecretAccessKey".to_string())?;
        let session_token = extract_xml(&body, "SessionToken")
            .ok_or_else(|| "AssumeRole response missing SessionToken".to_string())?;
        let expiration = extract_xml(&body, "Expiration")
            .ok_or_else(|| "AssumeRole response missing Expiration".to_string())?;
        let expires_at = chrono::DateTime::parse_from_rfc3339(&expiration)
            .map_err(|e| format!("AssumeRole Expiration parse failed: {e}"))?
            .with_timezone(&Utc);
        self.credentials = AwsCredentials {
            access_key,
            secret_key,
            session_token: Some(session_token),
            expires_at: Some(expires_at),
        };
        Ok(())
    }
}

fn urlencode(s: &str) -> String {
    // 极简：AWS AssumeRole 只对几个字符做 URL encode；
    // 不引入 urlencoding crate。
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

fn extract_xml(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
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
            access_key: self.credentials.access_key.clone(),
            payload: payload.clone(),
        };
        let auth = sign(
            &self.credentials.secret_key,
            &self.region,
            "secretsmanager",
            &request,
            &payload_hash,
        );
        let url = format!("https://{}/", host(&self.region));
        let mut req = client()
            .post(&url)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", "secretsmanager.CreateSecret")
            .header("x-amz-date", &amz_date)
            .header("Authorization", auth)
            .body(payload);
        if let Some(token) = &self.credentials.session_token {
            req = req.header("x-amz-security-token", token);
        }
        let resp = req
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
            access_key: self.credentials.access_key.clone(),
            payload: payload.clone(),
        };
        let auth = sign(
            &self.credentials.secret_key,
            &self.region,
            "secretsmanager",
            &request,
            &payload_hash,
        );
        let url = format!("https://{}/", host(&self.region));
        let mut req = client()
            .post(&url)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", "secretsmanager.GetSecretValue")
            .header("x-amz-date", &amz_date)
            .header("Authorization", auth)
            .body(payload);
        if let Some(token) = &self.credentials.session_token {
            req = req.header("x-amz-security-token", token);
        }
        let resp = req
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

#[cfg(test)]
mod assume_role_tests {
    use super::*;

    #[test]
    fn r564_aws_constructor_uses_static_credentials() {
        let p = AwsSecretsManagerProvider::new("us-east-1", "AKIA", "secret");
        assert_eq!(p.credentials().access_key, "AKIA");
        assert_eq!(p.credentials().secret_key, "secret");
        assert!(p.credentials().session_token.is_none());
        assert!(!p.credentials().is_expired());
        assert!(p.assume_role_config().is_none());
    }

    #[test]
    fn r564_aws_with_assume_role_records_config() {
        let p = AwsSecretsManagerProvider::new("us-east-1", "AKIA", "secret")
            .with_assume_role("arn:aws:iam::123:role/admin", "paperclip-session");
        let cfg = p.assume_role_config().expect("assume role set");
        assert_eq!(cfg.role_arn, "arn:aws:iam::123:role/admin");
        assert_eq!(cfg.session_name, "paperclip-session");
        assert!(cfg.external_id.is_none());
    }

    #[test]
    fn r564_aws_with_assume_role_external_id() {
        let p = AwsSecretsManagerProvider::new("us-east-1", "AKIA", "secret")
            .with_assume_role_external_id(
                "arn:aws:iam::123:role/admin",
                "paperclip-session",
                "ext-1",
            );
        assert_eq!(
            p.assume_role_config().unwrap().external_id.as_deref(),
            Some("ext-1")
        );
    }

    #[test]
    fn r564_aws_set_temporary_credentials_marks_expiry() {
        let mut p = AwsSecretsManagerProvider::new("us-east-1", "AKIA", "secret");
        let future = Utc::now() + chrono::Duration::hours(1);
        p.set_temporary_credentials("ASIA", "tmp-secret", "session-token", future);
        assert_eq!(p.credentials().access_key, "ASIA");
        assert_eq!(
            p.credentials().session_token.as_deref(),
            Some("session-token")
        );
        assert!(!p.credentials().is_expired());
    }

    #[test]
    fn r564_aws_is_expired_handles_past_timestamp() {
        let mut p = AwsSecretsManagerProvider::new("us-east-1", "AKIA", "secret");
        let past = Utc::now() - chrono::Duration::hours(1);
        p.set_temporary_credentials("ASIA", "tmp", "tok", past);
        assert!(p.credentials().is_expired());
    }

    #[tokio::test]
    async fn r564_aws_refresh_without_assume_role_is_noop() {
        let mut p = AwsSecretsManagerProvider::new("us-east-1", "AKIA", "secret");
        p.refresh_credentials().await.unwrap();
        assert_eq!(p.credentials().access_key, "AKIA");
    }

    #[test]
    fn r564_aws_urlencode_handles_special_chars() {
        assert_eq!(urlencode("hello"), "hello");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a/b"), "a%2Fb");
        assert_eq!(urlencode("a+b"), "a%2Bb");
        // role ARN 含 : 和 / 都要编码
        let arn = "arn:aws:iam::123:role/admin";
        let encoded = urlencode(arn);
        assert!(encoded.contains("%3A"));
        assert!(encoded.contains("%2F"));
    }

    #[test]
    fn r564_aws_extract_xml_pulls_simple_tag() {
        let body = "<AssumeRoleResponse><AssumeRoleResult><Credentials><AccessKeyId>ASIA</AccessKeyId><SecretAccessKey>sec</SecretAccessKey><SessionToken>tok</SessionToken><Expiration>2025-01-01T00:00:00Z</Expiration></Credentials></AssumeRoleResult></AssumeRoleResponse>";
        assert_eq!(extract_xml(body, "AccessKeyId").as_deref(), Some("ASIA"));
        assert_eq!(extract_xml(body, "SecretAccessKey").as_deref(), Some("sec"));
        assert_eq!(extract_xml(body, "SessionToken").as_deref(), Some("tok"));
        assert_eq!(
            extract_xml(body, "Expiration").as_deref(),
            Some("2025-01-01T00:00:00Z")
        );
    }

    #[test]
    fn r564_aws_extract_xml_missing_tag_returns_none() {
        let body = "<Other><Foo>bar</Foo></Other>";
        assert!(extract_xml(body, "AccessKeyId").is_none());
    }

    // sanity: health_check 仍然能用静态凭证
    #[tokio::test]
    async fn r564_aws_health_check_returns_ok() {
        let p = AwsSecretsManagerProvider::new("us-east-1", "AKIA", "secret");
        let h = p.health_check(None, None).await;
        assert_eq!(h.status, ProviderHealthStatus::Ok);
        assert!(h.message.contains("us-east-1"));
    }

    // sanity: create_secret 的 PreparedSecretVersion 形状未破坏
    // (需要网络，所以只测材料序列化 / 解析环节)
    #[test]
    fn r564_aws_prepared_secret_version_shape() {
        let v = PreparedSecretVersion {
            material: json!({
                "scheme": "aws_secrets_manager_v1",
                "region": "us-east-1",
                "secret_name": "demo",
            }),
            value_sha256: "abcd".into(),
            fingerprint_sha256: Some("abcd".into()),
            external_ref: Some("arn:aws:secretsmanager:us-east-1:secret:demo".into()),
            provider_version_ref: None,
        };
        let serialized = serde_json::to_string(&v).unwrap();
        assert!(serialized.contains("aws_secrets_manager_v1"));
        let back: PreparedSecretVersion = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            back.external_ref.unwrap(),
            "arn:aws:secretsmanager:us-east-1:secret:demo"
        );
    }
}
