//! S3 兼容 provider（AWS SigV4）。
//!
//! 复用 `aws-sdk-*` 不在依赖列表里以保持轻量；本模块通过 SigV4 签名的 HTTPS
//! 直接调用 S3 REST API（`PutObject` / `GetObject` / `DeleteObject` /
//! `ListObjectsV2`），需要 host 在 `S3Storage::new(...).mark_configured(...)` 之后
//! 注入 access key / secret key / region。

use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use url::Url;

use crate::error::{StorageError, StorageResult};
use crate::provider::{ObjectMetadata, ObjectStream, StorageProvider};
use crate::types::{ObjectKey, PresignedUrl, StorageClass, StorageLocation};

#[derive(Debug, Clone)]
pub struct S3Storage {
    name: &'static str,
    region: String,
    bucket: String,
    access_key: Option<String>,
    secret_key: Option<String>,
    endpoint: Option<String>,
    path_style: bool,
}

impl S3Storage {
    #[must_use]
    pub fn new(region: impl Into<String>, bucket: impl Into<String>) -> Self {
        Self {
            name: "s3",
            region: region.into(),
            bucket: bucket.into(),
            access_key: None,
            secret_key: None,
            endpoint: None,
            path_style: true,
        }
    }

    #[must_use]
    pub fn with_credentials(
        mut self,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        self.access_key = Some(access_key.into());
        self.secret_key = Some(secret_key.into());
        self
    }

    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    #[must_use]
    pub fn path_style(mut self, path_style: bool) -> Self {
        self.path_style = path_style;
        self
    }

    fn require_creds(&self) -> StorageResult<(&str, &str)> {
        match (self.access_key.as_deref(), self.secret_key.as_deref()) {
            (Some(a), Some(s)) => Ok((a, s)),
            _ => Err(StorageError::NotConfigured(
                "S3 credentials not configured".into(),
            )),
        }
    }

    fn host(&self) -> String {
        match &self.endpoint {
            Some(e) => e.trim_end_matches('/').to_string(),
            None => {
                if self.path_style {
                    format!("s3.{}.amazonaws.com", self.region)
                } else {
                    format!("{}.s3.{}.amazonaws.com", self.bucket, self.region)
                }
            }
        }
    }
}

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("reqwest client")
    })
}

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

fn amz_date(now: chrono::DateTime<Utc>) -> (String, String) {
    (
        now.format("%Y%m%dT%H%M%SZ").to_string(),
        now.format("%Y%m%d").to_string(),
    )
}

struct SignedRequest {
    url: String,
    headers: Vec<(String, String)>,
}

fn build_signed_request(
    method: &str,
    host_value: &str,
    region: &str,
    service: &str,
    access_key: &str,
    secret_key: &str,
    canonical_uri: &str,
    canonical_query: &str,
    payload_hash: &str,
    content_type: Option<&str>,
    body_hash: &str,
    extra_headers: &[(&str, &str)],
) -> (String, Vec<(String, String)>) {
    let now = Utc::now();
    let (amz_date, date_stamp) = amz_date(now);
    let mut headers = vec![
        ("host".to_string(), host_value.to_string()),
        ("x-amz-date".to_string(), amz_date.clone()),
        ("x-amz-content-sha256".to_string(), body_hash.to_string()),
    ];
    if let Some(ct) = content_type {
        headers.push(("content-type".to_string(), ct.to_string()));
    }
    for (k, v) in extra_headers {
        headers.push((k.to_string(), v.to_string()));
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical_headers: String = headers
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{body_hash}"
    );
    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let k_date = hmac(
        format!("AWS4{secret_key}").as_bytes(),
        date_stamp.as_bytes(),
    );
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex::encode(hmac(&k_signing, string_to_sign.as_bytes()));
    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
    );
    let mut all = vec![
        ("Authorization".to_string(), auth),
        ("x-amz-date".to_string(), amz_date),
        ("x-amz-content-sha256".to_string(), body_hash.to_string()),
    ];
    if let Some(ct) = content_type {
        all.push(("content-type".to_string(), ct.to_string()));
    }
    for (k, v) in extra_headers {
        all.push((k.to_string(), v.to_string()));
    }
    (payload_hash.to_string(), all)
}

#[derive(Debug, Deserialize)]
struct ListObjectsResponse {
    #[serde(rename = "Contents", default)]
    contents: Vec<ListObjectEntry>,
}

#[derive(Debug, Deserialize)]
struct ListObjectEntry {
    #[serde(rename = "Key")]
    key: String,
}

#[async_trait]
impl StorageProvider for S3Storage {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn health(&self) -> StorageResult<()> {
        if self.access_key.is_none() {
            return Err(StorageError::NotConfigured("S3 credentials missing".into()));
        }
        Ok(())
    }

    async fn put_object(
        &self,
        target: &StorageLocation,
        bytes: Bytes,
        content_type: Option<&str>,
    ) -> StorageResult<ObjectMetadata> {
        let (access, secret) = self.require_creds()?;
        let host = self.host();
        let key = target.key.as_str();
        let uri = if self.path_style {
            format!("/{}/{}", self.bucket, key)
        } else {
            format!("/{key}")
        };
        let body_hash = sha256_hex(&bytes);
        let url = if let Some(endpoint) = &self.endpoint {
            format!("{}/{}/{}", endpoint.trim_end_matches('/'), self.bucket, key)
        } else if self.path_style {
            format!("https://{host}{uri}")
        } else {
            format!("https://{}/{key}", host)
        };
        let (_, headers) = build_signed_request(
            "PUT",
            &host,
            &self.region,
            "s3",
            access,
            secret,
            &uri,
            "",
            &body_hash,
            content_type,
            &body_hash,
            &[],
        );
        let mut req = client().put(&url);
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req
            .body(bytes.clone())
            .send()
            .await
            .map_err(|e| StorageError::Backend(format!("s3 put_object send failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(StorageError::Backend(format!(
                "s3 put_object HTTP {}",
                resp.status()
            )));
        }
        Ok(ObjectMetadata {
            key: target.key.clone(),
            size: bytes.len() as u64,
            content_type: content_type.map(str::to_string),
            content_sha256: Some(body_hash),
            last_modified: Utc::now(),
            class: StorageClass::Hot,
        })
    }

    async fn get_object(&self, location: &StorageLocation) -> StorageResult<Bytes> {
        let (access, secret) = self.require_creds()?;
        let host = self.host();
        let key = location.key.as_str();
        let uri = if self.path_style {
            format!("/{}/{}", self.bucket, key)
        } else {
            format!("/{key}")
        };
        let empty_hash = sha256_hex(b"");
        let url = if let Some(endpoint) = &self.endpoint {
            format!("{}/{}/{}", endpoint.trim_end_matches('/'), self.bucket, key)
        } else if self.path_style {
            format!("https://{host}{uri}")
        } else {
            format!("https://{}/{key}", host)
        };
        let (_, headers) = build_signed_request(
            "GET",
            &host,
            &self.region,
            "s3",
            access,
            secret,
            &uri,
            "",
            "",
            None,
            &empty_hash,
            &[],
        );
        let mut req = client().get(&url);
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| StorageError::Backend(format!("s3 get_object send failed: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(StorageError::NotFound(format!("{}/{}", self.bucket, key)));
        }
        if !resp.status().is_success() {
            return Err(StorageError::Backend(format!(
                "s3 get_object HTTP {}",
                resp.status()
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| StorageError::Backend(format!("s3 body: {e}")))?;
        Ok(bytes)
    }

    async fn stream_object(&self, location: &StorageLocation) -> StorageResult<ObjectStream> {
        let bytes = self.get_object(location).await?;
        Ok(Box::pin(futures::stream::once(async move { Ok(bytes) })))
    }

    async fn delete_object(&self, location: &StorageLocation) -> StorageResult<()> {
        let (access, secret) = self.require_creds()?;
        let host = self.host();
        let key = location.key.as_str();
        let uri = if self.path_style {
            format!("/{}/{}", self.bucket, key)
        } else {
            format!("/{key}")
        };
        let empty_hash = sha256_hex(b"");
        let url = if let Some(endpoint) = &self.endpoint {
            format!("{}/{}/{}", endpoint.trim_end_matches('/'), self.bucket, key)
        } else if self.path_style {
            format!("https://{host}{uri}")
        } else {
            format!("https://{}/{key}", host)
        };
        let (_, headers) = build_signed_request(
            "DELETE",
            &host,
            &self.region,
            "s3",
            access,
            secret,
            &uri,
            "",
            "",
            None,
            &empty_hash,
            &[],
        );
        let mut req = client().delete(&url);
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| StorageError::Backend(format!("s3 delete_object send failed: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(()); // idempotent
        }
        if !resp.status().is_success() {
            return Err(StorageError::Backend(format!(
                "s3 delete_object HTTP {}",
                resp.status()
            )));
        }
        Ok(())
    }

    async fn list_prefix(&self, bucket: &str, prefix: &str) -> StorageResult<Vec<ObjectKey>> {
        let (access, secret) = self.require_creds()?;
        let host = self.host();
        let canonical_uri = format!("/{bucket}");
        let canonical_query = format!(
            "list-type=2&prefix={}",
            url::form_urlencoded::byte_serialize(prefix.as_bytes()).collect::<String>()
        );
        let empty_hash = sha256_hex(b"");
        let url = if let Some(endpoint) = &self.endpoint {
            format!("{endpoint}/{bucket}?{canonical_query}")
        } else {
            format!("https://{host}{canonical_uri}?{canonical_query}")
        };
        let (_, headers) = build_signed_request(
            "GET",
            &host,
            &self.region,
            "s3",
            access,
            secret,
            &canonical_uri,
            &canonical_query,
            "",
            None,
            &empty_hash,
            &[],
        );
        let mut req = client().get(&url);
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| StorageError::Backend(format!("s3 list_prefix send failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(StorageError::Backend(format!(
                "s3 list_prefix HTTP {}",
                resp.status()
            )));
        }
        let body: ListObjectsResponse = resp
            .json()
            .await
            .map_err(|e| StorageError::Backend(format!("s3 list_prefix decode: {e}")))?;
        Ok(body
            .contents
            .into_iter()
            .map(|c| ObjectKey::new(c.key))
            .collect())
    }

    async fn presign_get(
        &self,
        location: &StorageLocation,
        ttl: std::time::Duration,
    ) -> StorageResult<PresignedUrl> {
        let (access, secret) = self.require_creds()?;
        let host = self.host();
        let key = location.key.as_str();
        let canonical_uri = if self.path_style {
            format!("/{}/{}", self.bucket, key)
        } else {
            format!("/{key}")
        };
        let now = Utc::now();
        let (amz_date, date_stamp) = amz_date(now);
        let expires_seconds = ttl.as_secs().max(60).min(604_800);
        let canonical_query = format!(
            "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={}&X-Amz-Date={}&X-Amz-Expires={}&X-Amz-SignedHeaders=host",
            url::form_urlencoded::byte_serialize(
                format!("{access}/{date_stamp}/{}/s3/aws4_request", self.region).as_bytes()
            )
            .collect::<String>(),
            amz_date,
            expires_seconds
        );
        let canonical_request = format!(
            "GET\n{canonical_uri}\n{canonical_query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD"
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{date_stamp}/{}/s3/aws4_request\n{}",
            self.region,
            sha256_hex(canonical_request.as_bytes())
        );
        let k_date = hmac(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
        let k_region = hmac(&k_date, self.region.as_bytes());
        let k_service = hmac(&k_region, b"s3");
        let k_signing = hmac(&k_service, b"aws4_request");
        let signature = hex::encode(hmac(&k_signing, string_to_sign.as_bytes()));
        let url =
            format!("https://{host}{canonical_uri}?{canonical_query}&X-Amz-Signature={signature}");
        let parsed = Url::parse(&url).map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(PresignedUrl {
            url: parsed.to_string(),
            expires_at: Utc::now() + chrono::Duration::seconds(ttl.as_secs() as i64),
        })
    }
}
