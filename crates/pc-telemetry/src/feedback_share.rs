//! Feedback trace 共享客户端（对齐 Node `server/src/services/feedback-share-client.ts`）。
//!
//! 职责：把 trace bundle 用 `gzip+base64+json` 编码后 POST 到远程 feedback 后端。
//! 不持有任何业务状态；只对 `Config.feedbackExportBackendUrl` / `feedbackExportBackendToken` 两个字段有依赖。

use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;

/// 默认 feedback 后端地址（与 Node `DEFAULT_FEEDBACK_EXPORT_BACKEND_URL` 1:1 对齐）。
pub const DEFAULT_FEEDBACK_EXPORT_BACKEND_URL: &str = "https://telemetry.paperclip.ing";

/// payload 编码格式（与 Node `encoding: "gzip+base64+json"` 1:1 对齐）。
pub const FEEDBACK_SHARE_ENCODING: &str = "gzip+base64+json";

/// HTTP 请求超时（Node 没有显式超时，复刻时按合理默认 30s 兜底）。
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Feedback trace bundle 形状（对齐 Node `@paperclipai/shared` 的 `FeedbackTraceBundle`）。
///
/// 注：pc-telemetry 不依赖 pc-core/sharp，所以这里独立定义一个轻量结构体；
/// 仅保留 Node 端真正使用的字段以避免跨 crate 强耦合。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackTraceBundle {
    pub trace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export_id: Option<String>,
    pub company_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_status: Option<String>,
    #[serde(default)]
    pub notes: Vec<JsonValue>,
    #[serde(default)]
    pub envelope: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paperclip_run: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_adapter_trace: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normalized_adapter_trace: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privacy: Option<JsonValue>,
    #[serde(default)]
    pub integrity: JsonValue,
    #[serde(default)]
    pub files: Vec<JsonValue>,
}

impl FeedbackTraceBundle {
    /// 构造一个最小可用的测试 bundle，便于单测与下游 fixture 复用。
    pub fn minimal(trace_id: impl Into<String>, company_id: impl Into<String>) -> Self {
        Self {
            trace_id: trace_id.into(),
            export_id: None,
            company_id: company_id.into(),
            issue_id: None,
            issue_identifier: None,
            adapter_type: None,
            capture_status: None,
            notes: Vec::new(),
            envelope: JsonValue::Object(Default::default()),
            surface: None,
            paperclip_run: None,
            raw_adapter_trace: None,
            normalized_adapter_trace: None,
            privacy: None,
            integrity: JsonValue::Object(Default::default()),
            files: Vec::new(),
        }
    }
}

/// Feedback trace 上传结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadTraceBundleResponse {
    pub object_key: String,
}

/// Feedback trace 上传错误。
#[derive(Debug, Error)]
pub enum FeedbackTraceShareError {
    #[error("feedback trace upload failed with HTTP {status}: {body}")]
    Http { status: u16, body: String },

    #[error("feedback trace response was not valid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),

    #[error("feedback trace request failed: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("feedback trace payload gzip encoding failed: {0}")]
    Gzip(std::io::Error),

    #[error("feedback trace base64 encoding failed: {0}")]
    Base64(base64::EncodeSliceError),

    #[error("feedback trace bundle serialization failed: {0}")]
    Serialize(serde_json::Error),
}

/// Feedback trace 共享客户端抽象。
#[async_trait::async_trait]
pub trait FeedbackTraceShareClient: Send + Sync {
    async fn upload_trace_bundle(
        &self,
        bundle: &FeedbackTraceBundle,
    ) -> Result<UploadTraceBundleResponse, FeedbackTraceShareError>;
}

/// 客户端配置（与 Node `Config.feedbackExportBackendUrl` / `feedbackExportBackendToken` 对齐）。
#[derive(Debug, Clone, Default)]
pub struct FeedbackShareConfig {
    pub backend_url: Option<String>,
    pub backend_token: Option<String>,
}

impl FeedbackShareConfig {
    pub fn new(backend_url: Option<String>, backend_token: Option<String>) -> Self {
        Self {
            backend_url,
            backend_token,
        }
    }
}

/// 基于 reqwest 的 HTTP 实现。
#[derive(Debug, Clone)]
pub struct HttpFeedbackTraceShareClient {
    endpoint: String,
    bearer_token: Option<String>,
    http: reqwest::Client,
}

impl HttpFeedbackTraceShareClient {
    pub fn new(config: &FeedbackShareConfig) -> Self {
        let base_url = config
            .backend_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_FEEDBACK_EXPORT_BACKEND_URL);
        let endpoint = append_path(base_url, "/feedback-traces");
        let bearer_token = config
            .backend_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("reqwest client build should not fail");
        Self {
            endpoint,
            bearer_token,
            http,
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn bearer_token(&self) -> Option<&str> {
        self.bearer_token.as_deref()
    }
}

/// 工厂函数（与 Node `createFeedbackTraceShareClientFromConfig` 1:1 对齐）。
pub fn create_feedback_trace_share_client_from_config(
    config: &FeedbackShareConfig,
) -> HttpFeedbackTraceShareClient {
    HttpFeedbackTraceShareClient::new(config)
}

/// 构造 feedback trace 对象键（与 Node `buildFeedbackShareObjectKey` 1:1 对齐）。
///
/// 格式：`feedback-traces/{companyId}/{YYYY}/{MM}/{DD}/{exportId ?? traceId}.json`，
/// 月、日采用 UTC 且两位补零。
pub fn build_feedback_share_object_key(
    bundle: &FeedbackTraceBundle,
    exported_at: DateTime<Utc>,
) -> String {
    let year = exported_at.format("%Y").to_string();
    let month = exported_at.format("%m").to_string();
    let day = exported_at.format("%d").to_string();
    let id = bundle
        .export_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&bundle.trace_id);
    format!(
        "feedback-traces/{}/{}/{}/{}/{}.json",
        bundle.company_id, year, month, day, id
    )
}

/// 对 `{objectKey, exportedAt, bundle}` 做 gzip+base64 编码（与 Node `gzipSync(...).toString("base64")` 1:1 对齐）。
///
/// 返回 (encoding_label, base64_payload)。
pub fn encode_feedback_share_payload(
    object_key: &str,
    exported_at: DateTime<Utc>,
    bundle: &FeedbackTraceBundle,
) -> Result<(String, String), FeedbackTraceShareError> {
    let inner = serde_json::json!({
        "objectKey": object_key,
        "exportedAt": exported_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "bundle": bundle,
    });
    let inner_bytes = serde_json::to_vec(&inner).map_err(FeedbackTraceShareError::Serialize)?;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    std::io::Write::write_all(&mut encoder, &inner_bytes).map_err(FeedbackTraceShareError::Gzip)?;
    let gz = encoder.finish().map_err(FeedbackTraceShareError::Gzip)?;

    let mut buf = String::with_capacity(gz.len() * 2);
    base64::engine::general_purpose::STANDARD.encode_string(&gz, &mut buf);
    Ok((FEEDBACK_SHARE_ENCODING.to_string(), buf))
}

/// 解码（用于测试：gunzipSync + base64）。
pub fn decode_feedback_share_payload(
    encoding: &str,
    payload: &str,
) -> Result<Vec<u8>, FeedbackTraceShareError> {
    if encoding != FEEDBACK_SHARE_ENCODING {
        return Err(FeedbackTraceShareError::Http {
            status: 0,
            body: format!("unsupported encoding: {encoding}"),
        });
    }
    let gz = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|e| FeedbackTraceShareError::Http {
            status: 0,
            body: format!("base64 decode failed: {e}"),
        })?;
    let mut decoder = flate2::read::GzDecoder::new(&gz[..]);
    let mut out = Vec::with_capacity(gz.len() * 4);
    std::io::Read::read_to_end(&mut decoder, &mut out).map_err(FeedbackTraceShareError::Gzip)?;
    Ok(out)
}

/// 把 `/feedback-traces` 拼到 base url 后面（与 Node `new URL("/feedback-traces", baseUrl)` 1:1 对齐）。
fn append_path(base_url: &str, path: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    format!("{trimmed}{path}")
}

#[async_trait::async_trait]
impl FeedbackTraceShareClient for HttpFeedbackTraceShareClient {
    async fn upload_trace_bundle(
        &self,
        bundle: &FeedbackTraceBundle,
    ) -> Result<UploadTraceBundleResponse, FeedbackTraceShareError> {
        let exported_at = Utc::now();
        let object_key = build_feedback_share_object_key(bundle, exported_at);
        let (_encoding, payload) = encode_feedback_share_payload(&object_key, exported_at, bundle)?;
        let body = serde_json::json!({
            "encoding": FEEDBACK_SHARE_ENCODING,
            "payload": payload,
        });

        let mut req = self
            .http
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .json(&body);
        if let Some(token) = &self.bearer_token {
            req = req.bearer_auth(token);
        }
        let response = req.send().await?;

        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            let trimmed = detail.trim();
            let body_msg = if trimmed.is_empty() {
                format!("Feedback trace upload failed with HTTP {}", status.as_u16())
            } else {
                trimmed.to_string()
            };
            return Err(FeedbackTraceShareError::Http {
                status: status.as_u16(),
                body: body_msg,
            });
        }

        let parsed: Option<UploadTraceBundleResponse> = response.json().await.ok();
        let final_key = parsed
            .as_ref()
            .map(|r| r.object_key.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or(object_key);

        Ok(UploadTraceBundleResponse {
            object_key: final_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_bundle() -> FeedbackTraceBundle {
        FeedbackTraceBundle::minimal("trace-1", "company-1").with_export_id("export-1")
    }

    trait BundleExt {
        fn with_export_id(self, export_id: &str) -> Self;
    }
    impl BundleExt for FeedbackTraceBundle {
        fn with_export_id(mut self, export_id: &str) -> Self {
            self.export_id = Some(export_id.to_string());
            self
        }
    }

    #[test]
    fn build_object_key_uses_utc_date_segments() {
        let bundle = sample_bundle();
        let ts = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        assert_eq!(
            build_feedback_share_object_key(&bundle, ts),
            "feedback-traces/company-1/2026/05/07/export-1.json"
        );
    }

    #[test]
    fn build_object_key_falls_back_to_trace_id_when_export_id_missing() {
        let mut bundle = sample_bundle();
        bundle.export_id = None;
        let ts = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        assert_eq!(
            build_feedback_share_object_key(&bundle, ts),
            "feedback-traces/company-1/2026/01/02/trace-1.json"
        );
    }

    #[test]
    fn build_object_key_falls_back_when_export_id_empty_string() {
        let mut bundle = sample_bundle();
        bundle.export_id = Some(String::new());
        let ts = Utc.with_ymd_and_hms(2026, 12, 31, 23, 59, 59).unwrap();
        assert_eq!(
            build_feedback_share_object_key(&bundle, ts),
            "feedback-traces/company-1/2026/12/31/trace-1.json"
        );
    }

    #[test]
    fn encode_payload_round_trips_via_decode() {
        let bundle = sample_bundle();
        let ts = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let object_key = build_feedback_share_object_key(&bundle, ts);
        let (encoding, payload) = encode_feedback_share_payload(&object_key, ts, &bundle).unwrap();
        assert_eq!(encoding, "gzip+base64+json");
        let decoded = decode_feedback_share_payload(&encoding, &payload).unwrap();
        let json: JsonValue = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(json["objectKey"], object_key);
        assert_eq!(json["exportedAt"], "2026-05-07T12:00:00.000Z");
        assert_eq!(json["bundle"]["traceId"], "trace-1");
        assert_eq!(json["bundle"]["exportId"], "export-1");
        assert_eq!(json["bundle"]["companyId"], "company-1");
    }

    #[test]
    fn client_uses_default_url_when_unset() {
        let client = HttpFeedbackTraceShareClient::new(&FeedbackShareConfig::default());
        assert_eq!(
            client.endpoint(),
            "https://telemetry.paperclip.ing/feedback-traces"
        );
        assert_eq!(client.bearer_token(), None);
    }

    #[test]
    fn client_trims_and_overrides_url() {
        let cfg = FeedbackShareConfig::new(
            Some(" https://telemetry.example.com/ ".to_string()),
            Some(" secret-token ".to_string()),
        );
        let client = HttpFeedbackTraceShareClient::new(&cfg);
        assert_eq!(
            client.endpoint(),
            "https://telemetry.example.com/feedback-traces"
        );
        assert_eq!(client.bearer_token(), Some("secret-token"));
    }

    #[test]
    fn client_drops_empty_token() {
        let cfg = FeedbackShareConfig::new(Some("https://x".into()), Some("   ".into()));
        let client = HttpFeedbackTraceShareClient::new(&cfg);
        assert_eq!(client.bearer_token(), None);
    }

    #[test]
    fn append_path_strips_trailing_slash() {
        assert_eq!(
            append_path("https://telemetry.paperclip.ing/", "/feedback-traces"),
            "https://telemetry.paperclip.ing/feedback-traces"
        );
        assert_eq!(
            append_path("https://telemetry.paperclip.ing", "/feedback-traces"),
            "https://telemetry.paperclip.ing/feedback-traces"
        );
    }

    #[test]
    fn decode_rejects_unknown_encoding() {
        let err = decode_feedback_share_payload("plain", "abc").unwrap_err();
        match err {
            FeedbackTraceShareError::Http { body, .. } => {
                assert!(body.contains("unsupported encoding"));
            }
            _ => panic!("expected Http error, got {err:?}"),
        }
    }

    #[test]
    fn factory_returns_http_client() {
        let client =
            create_feedback_trace_share_client_from_config(&FeedbackShareConfig::default());
        assert_eq!(
            client.endpoint(),
            "https://telemetry.paperclip.ing/feedback-traces"
        );
    }

    // 异步集成测试：本地起一个 TCP echo 服务器，校验 POST body / headers / 响应解析。
    #[tokio::test]
    async fn upload_sends_gzip_base64_payload_and_parses_response_object_key() {
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        use tokio::sync::Mutex;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let captured: Arc<Mutex<Option<(String, String, String)>>> = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();

        let server = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                let mut lines = raw.split("\r\n");
                let request_line = lines.next().unwrap_or("").to_string();
                let mut headers = String::new();
                for line in lines.by_ref() {
                    if line.is_empty() {
                        break;
                    }
                    headers.push_str(line);
                    headers.push('\n');
                }
                let body_start = raw.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
                let body = raw[body_start..].to_string();
                *captured_clone.lock().await = Some((request_line, headers, body));

                let response_body = r#"{"objectKey":"feedback-traces/server-override.json"}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let cfg = FeedbackShareConfig::new(
            Some(format!("http://{addr}")),
            Some("test-token".to_string()),
        );
        let client = HttpFeedbackTraceShareClient::new(&cfg);
        let bundle = sample_bundle();
        let result = client.upload_trace_bundle(&bundle).await.unwrap();
        assert_eq!(result.object_key, "feedback-traces/server-override.json");

        let guard = captured.lock().await.clone();
        let (request_line, headers, body) = guard.expect("server captured request");
        assert!(request_line.starts_with("POST /feedback-traces HTTP/1.1"));
        assert!(headers.contains("content-type: application/json"));
        assert!(headers.contains("authorization: Bearer test-token"));

        let parsed: JsonValue = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["encoding"], "gzip+base64+json");
        let decoded = decode_feedback_share_payload(
            parsed["encoding"].as_str().unwrap(),
            parsed["payload"].as_str().unwrap(),
        )
        .unwrap();
        let inner: JsonValue = serde_json::from_slice(&decoded).unwrap();
        assert!(inner["objectKey"]
            .as_str()
            .unwrap()
            .contains("feedback-traces/company-1/"));
        assert!(inner["objectKey"]
            .as_str()
            .unwrap()
            .ends_with("/export-1.json"));
        assert_eq!(
            inner["bundle"]["envelope"],
            JsonValue::Object(Default::default())
        );

        let _ = server.await;
    }

    #[tokio::test]
    async fn upload_returns_local_object_key_when_response_missing_object_key() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 16 * 1024];
                let _ = sock.read(&mut buf).await;
                let body = r#"{}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let cfg = FeedbackShareConfig::new(Some(format!("http://{addr}")), None);
        let client = HttpFeedbackTraceShareClient::new(&cfg);
        let bundle = sample_bundle();
        let result = client.upload_trace_bundle(&bundle).await.unwrap();
        assert!(result.object_key.contains("feedback-traces/company-1/"));
        assert!(result.object_key.ends_with("/export-1.json"));

        let _ = server.await;
    }

    #[tokio::test]
    async fn upload_returns_http_error_when_status_not_ok() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 16 * 1024];
                let _ = sock.read(&mut buf).await;
                let body = "upstream rejected";
                let resp = format!(
                    "HTTP/1.1 502 Bad Gateway\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let cfg = FeedbackShareConfig::new(Some(format!("http://{addr}")), None);
        let client = HttpFeedbackTraceShareClient::new(&cfg);
        let bundle = sample_bundle();
        let err = client.upload_trace_bundle(&bundle).await.unwrap_err();
        match err {
            FeedbackTraceShareError::Http { status, body } => {
                assert_eq!(status, 502);
                assert_eq!(body, "upstream rejected");
            }
            other => panic!("expected Http error, got {other:?}"),
        }

        let _ = server.await;
    }

    #[tokio::test]
    async fn upload_returns_generic_message_when_body_empty() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 16 * 1024];
                let _ = sock.read(&mut buf).await;
                let resp = "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let cfg = FeedbackShareConfig::new(Some(format!("http://{addr}")), None);
        let client = HttpFeedbackTraceShareClient::new(&cfg);
        let bundle = sample_bundle();
        let err = client.upload_trace_bundle(&bundle).await.unwrap_err();
        match err {
            FeedbackTraceShareError::Http { status, body } => {
                assert_eq!(status, 503);
                assert_eq!(body, "Feedback trace upload failed with HTTP 503");
            }
            other => panic!("expected Http error, got {other:?}"),
        }

        let _ = server.await;
    }
}
