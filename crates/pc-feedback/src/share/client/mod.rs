#![forbid(unsafe_code)]
//! `pc-feedback-share-client` —— feedback trace 上传客户端。
//!
//! 对应 Node `server/src/services/feedback-share-client.ts`（59 行）。
//!
//! 设计目标：1:1 复刻
//! - `buildFeedbackShareObjectKey` —— S3 风格对象 key
//! - `createFeedbackTraceShareClientFromConfig` —— HTTP 上传器（注入式）
//! - 默认 backend URL: `https://telemetry.paperclip.ing`
//! - 上传格式：`gzip+base64+json`（gzip 压缩 body → base64 编码 → 包成 `{ encoding, payload }` JSON）

use std::sync::Arc;

use base64::Engine;
use chrono::{DateTime, Utc};
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

/// 默认 backend URL —— 与 Node `DEFAULT_FEEDBACK_EXPORT_BACKEND_URL` 1:1。
pub const DEFAULT_FEEDBACK_EXPORT_BACKEND_URL: &str = "https://telemetry.paperclip.ing";

//! Feedback trace HTTP upload client.
//!
//! 内部实现，提供 `FeedbackTraceShareClient` trait 与 HTTP 实现。
//! 公共 API 通过 [`super`] 平铺 re-export；本子模块路径仍可用以兼容。

pub use pc_telemetry::feedback_share::FeedbackTraceBundle;

/// 上传结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackTraceShareUploadResult {
    pub object_key: String,
}

/// 上传错误。
#[derive(Debug, thiserror::Error)]
pub enum FeedbackTraceShareError {
    #[error("HTTP {0}: {1}")]
    Http(u16, String),
    #[error("encoding error: {0}")]
    Encoding(String),
    #[error("serde error: {0}")]
    Serde(String),
}

/// 注入式 HTTP fetcher trait（与 Node `fetch` 等价）。
#[async_trait::async_trait]
pub trait FeedbackHttpFetcher: Send + Sync {
    async fn post(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> Result<HttpResponse, FeedbackTraceShareError>;
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// `buildFeedbackShareObjectKey` —— 与 Node 1:1 对齐。
pub fn build_feedback_share_object_key(
    bundle: &FeedbackTraceBundle,
    exported_at: DateTime<Utc>,
) -> String {
    let year = exported_at.format("%Y").to_string();
    let month = format!("{:02}", exported_at.format("%m").to_string());
    let day = format!("{:02}", exported_at.format("%d").to_string());
    let id = bundle.export_id.clone().unwrap_or_else(|| bundle.trace_id.clone());
    format!(
        "feedback-traces/{}/{}/{}/{}/{}.json",
        bundle.company_id, year, month, day, id
    )
}

/// gzip + base64 编码请求体。
pub fn encode_feedback_body(request_body: &str) -> Result<String, FeedbackTraceShareError> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    use std::io::Write;
    encoder
        .write_all(request_body.as_bytes())
        .map_err(|e| FeedbackTraceShareError::Encoding(e.to_string()))?;
    let compressed = encoder
        .finish()
        .map_err(|e| FeedbackTraceShareError::Encoding(e.to_string()))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&compressed))
}

/// Feedback trace share client 配置。
#[derive(Debug, Clone)]
pub struct FeedbackTraceShareConfig {
    pub feedback_export_backend_url: Option<String>,
    pub feedback_export_backend_token: Option<String>,
}

/// Feedback trace share client —— 注入式。
#[derive(Clone)]
pub struct FeedbackTraceShareClient {
    fetcher: Arc<dyn FeedbackHttpFetcher>,
    base_url: String,
    token: Option<String>,
}

impl FeedbackTraceShareClient {
    /// 与 Node `createFeedbackTraceShareClientFromConfig(config)` 1:1 对齐。
    pub fn from_config(
        fetcher: Arc<dyn FeedbackHttpFetcher>,
        config: FeedbackTraceShareConfig,
    ) -> Self {
        let base_url = config
            .feedback_export_backend_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_FEEDBACK_EXPORT_BACKEND_URL)
            .to_string();
        let token = config
            .feedback_export_backend_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Self {
            fetcher,
            base_url,
            token,
        }
    }

    /// 上传一个 feedback trace bundle，返回 server 回的 objectKey（或 fallback）。
    pub async fn upload_trace_bundle(
        &self,
        bundle: &FeedbackTraceBundle,
    ) -> Result<FeedbackTraceShareUploadResult, FeedbackTraceShareError> {
        let exported_at = Utc::now();
        let object_key = build_feedback_share_object_key(bundle, exported_at);

        let request_body = serde_json::to_string(&serde_json::json!({
            "objectKey": object_key,
            "exportedAt": exported_at.to_rfc3339(),
            "bundle": bundle,
        }))
        .map_err(|e| FeedbackTraceShareError::Serde(e.to_string()))?;

        let encoded = encode_feedback_body(&request_body)?;
        let body = serde_json::to_string(&serde_json::json!({
            "encoding": "gzip+base64+json",
            "payload": encoded,
        }))
        .map_err(|e| FeedbackTraceShareError::Serde(e.to_string()))?;

        let endpoint = format!("{}/feedback-traces", self.base_url.trim_end_matches('/'));

        let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
        if let Some(token) = &self.token {
            headers.push(("authorization".to_string(), format!("Bearer {token}")));
        }

        let response = self.fetcher.post(&endpoint, headers, body).await?;
        if !(200..300).contains(&response.status) {
            let detail = response.body.trim().to_string();
            return Err(FeedbackTraceShareError::Http(
                response.status,
                if detail.is_empty() {
                    format!("HTTP {}", response.status)
                } else {
                    detail
                },
            ));
        }

        let payload: Option<serde_json::Value> = serde_json::from_str(&response.body).ok();
        let server_key = payload
            .as_ref()
            .and_then(|v| v.get("objectKey"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Ok(FeedbackTraceShareUploadResult {
            object_key: server_key.unwrap_or(object_key),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn r698_default_backend_url() {
        assert_eq!(DEFAULT_FEEDBACK_EXPORT_BACKEND_URL, "https://telemetry.paperclip.ing");
    }

    #[test]
    fn r698_build_object_key_format() {
        let bundle = FeedbackTraceBundle {
            company_id: "co-1".into(),
            export_id: Some("exp-7".into()),
            trace_id: "trace-x".into(),
            extra: Default::default(),
        };
        // 2024-05-12T...
        let exported_at: DateTime<Utc> =
            DateTime::parse_from_rfc3339("2024-05-12T03:04:05+00:00")
                .unwrap()
                .with_timezone(&Utc);
        let key = build_feedback_share_object_key(&bundle, exported_at);
        assert_eq!(key, "feedback-traces/co-1/2024/05/12/exp-7.json");
    }

    #[test]
    fn r698_object_key_falls_back_to_trace_id() {
        let bundle = FeedbackTraceBundle {
            company_id: "co-1".into(),
            export_id: None,
            trace_id: "trace-99".into(),
            extra: Default::default(),
        };
        let exported_at: DateTime<Utc> =
            DateTime::parse_from_rfc3339("2024-01-02T00:00:00+00:00")
                .unwrap()
                .with_timezone(&Utc);
        let key = build_feedback_share_object_key(&bundle, exported_at);
        assert_eq!(key, "feedback-traces/co-1/2024/01/02/trace-99.json");
    }

    #[test]
    fn r698_object_key_pads_month_day() {
        let bundle = FeedbackTraceBundle {
            company_id: "co".into(),
            export_id: Some("e".into()),
            trace_id: "t".into(),
            extra: Default::default(),
        };
        let exported_at: DateTime<Utc> =
            DateTime::parse_from_rfc3339("2024-03-04T00:00:00+00:00")
                .unwrap()
                .with_timezone(&Utc);
        let key = build_feedback_share_object_key(&bundle, exported_at);
        assert_eq!(key, "feedback-traces/co/2024/03/04/e.json");
    }

    #[test]
    fn r698_encode_body_round_trip() {
        let body = "{\"hello\":\"world\"}";
        let encoded = encode_feedback_body(body).unwrap();
        // 验证能解回来
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(&encoded).unwrap();
        let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
        let mut out = String::new();
        use std::io::Read;
        decoder.read_to_string(&mut out).unwrap();
        assert_eq!(out, body);
    }

    struct MockFetcher {
        captured: Mutex<Option<(String, Vec<(String, String)>, String)>>,
        response: HttpResponse,
    }

    #[async_trait::async_trait]
    impl FeedbackHttpFetcher for MockFetcher {
        async fn post(
            &self,
            url: &str,
            headers: Vec<(String, String)>,
            body: String,
        ) -> Result<HttpResponse, FeedbackTraceShareError> {
            *self.captured.lock().unwrap() = Some((url.to_string(), headers, body));
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn r698_upload_sends_gzip_base64_json() {
        let fetcher = Arc::new(MockFetcher {
            captured: Mutex::new(None),
            response: HttpResponse {
                status: 200,
                body: r#"{"objectKey":"server-returned-key"}"#.to_string(),
            },
        });
        let client = FeedbackTraceShareClient::from_config(
            fetcher.clone(),
            FeedbackTraceShareConfig {
                feedback_export_backend_url: Some("https://example.com".into()),
                feedback_export_backend_token: Some("token123".into()),
            },
        );
        let bundle = FeedbackTraceBundle {
            company_id: "co".into(),
            export_id: Some("e".into()),
            trace_id: "t".into(),
            extra: Default::default(),
        };
        let r = client.upload_trace_bundle(&bundle).await.unwrap();
        assert_eq!(r.object_key, "server-returned-key");

        let captured = fetcher.captured.lock().unwrap().clone().unwrap();
        assert_eq!(captured.0, "https://example.com/feedback-traces");
        // Authorization header present
        assert!(captured.1.iter().any(|(k, v)| k == "authorization" && v == "Bearer token123"));
        assert!(captured.1.iter().any(|(k, v)| k == "content-type" && v == "application/json"));
        // Body 是 JSON { encoding, payload }
        let parsed: serde_json::Value = serde_json::from_str(&captured.2).unwrap();
        assert_eq!(parsed["encoding"], "gzip+base64+json");
        assert!(parsed["payload"].is_string());
    }

    #[tokio::test]
    async fn r698_upload_falls_back_to_local_object_key() {
        let fetcher = Arc::new(MockFetcher {
            captured: Mutex::new(None),
            response: HttpResponse { status: 200, body: "{}".to_string() },
        });
        let client = FeedbackTraceShareClient::from_config(
            fetcher,
            FeedbackTraceShareConfig {
                feedback_export_backend_url: None,
                feedback_export_backend_token: None,
            },
        );
        let bundle = FeedbackTraceBundle {
            company_id: "co".into(),
            export_id: Some("e".into()),
            trace_id: "t".into(),
            extra: Default::default(),
        };
        let r = client.upload_trace_bundle(&bundle).await.unwrap();
        // 服务端没返回 objectKey → 用本地计算
        assert!(r.object_key.starts_with("feedback-traces/co/"));
    }

    #[tokio::test]
    async fn r698_upload_http_error_returns_error() {
        let fetcher = Arc::new(MockFetcher {
            captured: Mutex::new(None),
            response: HttpResponse {
                status: 500,
                body: "internal error".to_string(),
            },
        });
        let client = FeedbackTraceShareClient::from_config(
            fetcher,
            FeedbackTraceShareConfig {
                feedback_export_backend_url: Some("https://example.com".into()),
                feedback_export_backend_token: None,
            },
        );
        let bundle = FeedbackTraceBundle {
            company_id: "co".into(),
            export_id: Some("e".into()),
            trace_id: "t".into(),
            extra: Default::default(),
        };
        let err = client.upload_trace_bundle(&bundle).await.unwrap_err();
        match err {
            FeedbackTraceShareError::Http(500, detail) => {
                assert!(detail.contains("internal error"));
            }
            other => panic!("expected Http(500, _), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn r698_upload_no_token_no_authorization_header() {
        let fetcher = Arc::new(MockFetcher {
            captured: Mutex::new(None),
            response: HttpResponse {
                status: 200,
                body: r#"{"objectKey":"k"}"#.to_string(),
            },
        });
        let client = FeedbackTraceShareClient::from_config(
            fetcher.clone(),
            FeedbackTraceShareConfig {
                feedback_export_backend_url: None,
                feedback_export_backend_token: None,
            },
        );
        let bundle = FeedbackTraceBundle {
            company_id: "co".into(),
            export_id: Some("e".into()),
            trace_id: "t".into(),
            extra: Default::default(),
        };
        client.upload_trace_bundle(&bundle).await.unwrap();
        let captured = fetcher.captured.lock().unwrap().clone().unwrap();
        // 无 token → 无 Authorization header
        assert!(!captured.1.iter().any(|(k, _)| k == "authorization"));
    }

    #[test]
    fn r698_token_trim_empty_treated_as_no_token() {
        let fetcher: Arc<dyn FeedbackHttpFetcher> = Arc::new(MockFetcher {
            captured: Mutex::new(None),
            response: HttpResponse { status: 200, body: "{}".to_string() },
        });
        let client = FeedbackTraceShareClient::from_config(
            fetcher,
            FeedbackTraceShareConfig {
                feedback_export_backend_url: Some("https://example.com".into()),
                feedback_export_backend_token: Some("   ".into()),
            },
        );
        // 验证 token 是 None
        assert!(client.token.is_none());
    }
}
