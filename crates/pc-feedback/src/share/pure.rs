#![forbid(unsafe_code)]

//! Feedback share 业务层的 zero-DB pure helpers。
//!
//! 对应 Node paperclip/server/src/services/feedback-share-client.ts
//! 中不需要走 HTTP 的纯函数部分（validation / canonical）。

use pc_telemetry::feedback_share::FeedbackTraceBundle;

/// 校验 trace bundle 的最小可用性（与 share service.build_object_key 一致）。
///
/// 返回 Ok(()) 当字段齐全且非空；否则返回具体错误信息。
pub fn validate_bundle_for_share(bundle: &FeedbackTraceBundle) -> Result<(), String> {
    if bundle.trace_id.trim().is_empty() {
        return Err("trace_id must not be empty".into());
    }
    if bundle.company_id.trim().is_empty() {
        return Err("company_id must not be empty".into());
    }
    Ok(())
}

/// 校验上传 URL 的最小可用性（trim + 非空）。
pub fn validate_backend_url(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("backend url must not be empty".into());
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err("backend url must be http(s)".into());
    }
    Ok(())
}

/// 从 bundle 中提取用于 object key 的 path 段：company_id/trace_id。
///
/// 返回 trim 后的小写 segments（保持插入顺序）。
pub fn derive_object_key_segments(bundle: &FeedbackTraceBundle) -> Vec<String> {
    vec![
        bundle.company_id.trim().to_lowercase(),
        bundle.trace_id.trim().to_lowercase(),
    ]
}

/// 合并 upload hook event 的 status + message，生成统一的错误描述。
pub fn describe_upload_failure(status: Option<u16>, message: &str) -> String {
    match status {
        Some(s) => format!("HTTP {}: {}", s, message),
        None => message.to_string(),
    }
}

/// 限制 byte_size 上报范围（避免 log 爆炸）。
pub fn clamp_payload_byte_size(byte_size: usize) -> usize {
    byte_size.min(usize::MAX / 2)
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use pc_telemetry::feedback_share::FeedbackTraceBundle;

    #[test]
    fn validate_bundle_accepts_minimal() {
        let bundle = FeedbackTraceBundle::minimal("trace-1", "company-1");
        assert!(validate_bundle_for_share(&bundle).is_ok());
    }

    #[test]
    fn validate_bundle_rejects_empty_trace_id() {
        let bundle = FeedbackTraceBundle::minimal("", "company-1");
        assert!(validate_bundle_for_share(&bundle).is_err());
    }

    #[test]
    fn validate_bundle_rejects_empty_company_id() {
        let bundle = FeedbackTraceBundle::minimal("trace-1", "");
        assert!(validate_bundle_for_share(&bundle).is_err());
    }

    #[test]
    fn validate_backend_url_accepts_https() {
        assert!(validate_backend_url("https://api.example.com").is_ok());
    }

    #[test]
    fn validate_backend_url_rejects_empty() {
        assert!(validate_backend_url("").is_err());
    }

    #[test]
    fn validate_backend_url_rejects_non_http() {
        assert!(validate_backend_url("ftp://example.com").is_err());
    }

    #[test]
    fn derive_object_key_segments_lowercases() {
        let bundle = FeedbackTraceBundle::minimal("TRACE-1", "COMPANY-A");
        let segs = derive_object_key_segments(&bundle);
        assert_eq!(segs, vec!["company-a".to_string(), "trace-1".to_string()]);
    }

    #[test]
    fn derive_object_key_segments_trims() {
        let bundle = FeedbackTraceBundle::minimal("  trace-1  ", "  company-a  ");
        let segs = derive_object_key_segments(&bundle);
        assert_eq!(segs[0], "company-a");
        assert_eq!(segs[1], "trace-1");
    }

    #[test]
    fn describe_upload_failure_includes_status() {
        let s = describe_upload_failure(Some(500), "oops");
        assert!(s.contains("500"));
        assert!(s.contains("oops"));
    }

    #[test]
    fn describe_upload_failure_no_status() {
        let s = describe_upload_failure(None, "connection refused");
        assert_eq!(s, "connection refused");
    }

    #[test]
    fn clamp_payload_byte_size_zero() {
        assert_eq!(clamp_payload_byte_size(0), 0);
    }

    #[test]
    fn clamp_payload_byte_size_normal() {
        assert_eq!(clamp_payload_byte_size(1024), 1024);
    }

    #[test]
    fn r755_share_pure_clamp_payload_byte_size_caps_huge_value() {
        // 任意极大值都必须被钳制到 usize::MAX/2
        assert_eq!(clamp_payload_byte_size(usize::MAX), usize::MAX / 2);
        assert_eq!(clamp_payload_byte_size(usize::MAX - 1), usize::MAX / 2);
    }

    #[test]
    fn r755_share_pure_describe_upload_failure_with_zero_status() {
        let s = describe_upload_failure(Some(0), "unreachable");
        assert!(s.contains("HTTP 0"));
        assert!(s.contains("unreachable"));
    }

    #[test]
    fn r755_share_pure_validate_backend_url_trims_whitespace() {
        assert!(validate_backend_url("   ").is_err());
        assert!(validate_backend_url("\thttps://api.example.com\n").is_ok());
    }
}
