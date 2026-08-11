//! Hermes gateway 传输安全校验（对齐 Node
//! `packages/adapters/hermes/src/gateway/server/transport-security.ts`）。
//!
//! 核心约束：远端 Hermes gateway 必须用 HTTPS；loopback HTTP 始终允许；
//! 显式 escape hatch `dangerouslyAllowInsecureRemoteHttp=true` 才允许远端
//! 纯 HTTP（dev-only）。

#![allow(dead_code)]

/// 显式 opt-in escape hatch key。
pub const INSECURE_REMOTE_HTTP_ESCAPE_HATCH: &str = "dangerouslyAllowInsecureRemoteHttp";

/// 解析 boolean-like 值（true / "1" / "true" / "yes" / "on" / false / "0" / ...）。
/// 不可识别返回 `None`。
pub fn parse_boolean_like(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) => match s.trim().to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// 主机名是否为 loopback（`localhost` / `127.x.x.x` / `::1` / `[::1]` 形式）。
pub fn is_loopback_hostname(hostname: &str) -> bool {
    let value = hostname
        .to_lowercase()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    value == "localhost"
        || value == "::1"
        || value == "0:0:0:0:0:0:0:1"
        || value == "127.0.0.1"
        || is_loopback_ipv4(&value)
}

/// `127.0.0.0/8`（127.x.x.x）任意段都算 loopback。
fn is_loopback_ipv4(value: &str) -> bool {
    let mut segments = value.split('.');
    let first = match segments.next() {
        Some(s) => s,
        None => return false,
    };
    if first != "127" {
        return false;
    }
    for segment in segments {
        if segment.is_empty() || segment.len() > 3 {
            return false;
        }
        if !segment.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    // 至少要有 4 段
    value.matches('.').count() == 3
}

/// `http:` 且主机不是 loopback → 远端纯 HTTP。
pub fn is_remote_plain_http(url: &url::Url) -> bool {
    url.scheme() == "http" && !is_loopback_hostname(url.host_str().unwrap_or(""))
}

/// 适配器配置是否允许远端纯 HTTP（escape hatch 开启）。
pub fn allows_insecure_remote_http(config: &serde_json::Value) -> bool {
    parse_boolean_like(
        &config
            .get(INSECURE_REMOTE_HTTP_ESCAPE_HATCH)
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    ) == Some(true)
}

/// 校验 apiBaseUrl 是否可接受：loopback HTTP 始终允许；远端必须 HTTPS；
/// 远端 HTTP 仅在 escape hatch 开启时允许。
///
/// 返回 `Ok(())` 或 `Err(message)`。
pub fn validate_api_base_url(config: &serde_json::Value, api_base_url: &str) -> Result<(), String> {
    let url = url::Url::parse(api_base_url).map_err(|e| format!("invalid apiBaseUrl: {e}"))?;
    if is_remote_plain_http(&url) && !allows_insecure_remote_http(config) {
        let hostname = url.host_str().unwrap_or("").to_string();
        return Err(remote_plain_http_denied_message(&hostname));
    }
    Ok(())
}

/// 远端纯 HTTP 被拒绝时的提示文案。
pub fn remote_plain_http_denied_message(hostname: &str) -> String {
    format!(
        "Hermes gateway apiBaseUrl uses remote plain HTTP for \"{hostname}\". \
         Use HTTPS or set {INSECURE_REMOTE_HTTP_ESCAPE_HATCH}=true only for unsafe local development."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn loopback_detection() {
        assert!(is_loopback_hostname("localhost"));
        assert!(is_loopback_hostname("127.0.0.1"));
        assert!(is_loopback_hostname("127.5.10.20"));
        assert!(is_loopback_hostname("::1"));
        assert!(is_loopback_hostname("[::1]"));
        assert!(is_loopback_hostname("0:0:0:0:0:0:0:1"));
        assert!(!is_loopback_hostname("example.com"));
        assert!(!is_loopback_hostname("192.168.1.1"));
        assert!(!is_loopback_hostname("10.0.0.1"));
    }

    #[test]
    fn parse_boolean_handles_aliases() {
        assert_eq!(parse_boolean_like(&json!(true)), Some(true));
        assert_eq!(parse_boolean_like(&json!(false)), Some(false));
        assert_eq!(parse_boolean_like(&json!("true")), Some(true));
        assert_eq!(parse_boolean_like(&json!("YES")), Some(true));
        assert_eq!(parse_boolean_like(&json!("on")), Some(true));
        assert_eq!(parse_boolean_like(&json!("1")), Some(true));
        assert_eq!(parse_boolean_like(&json!("false")), Some(false));
        assert_eq!(parse_boolean_like(&json!("0")), Some(false));
        assert_eq!(parse_boolean_like(&json!("off")), Some(false));
        assert_eq!(parse_boolean_like(&json!("maybe")), None);
        assert_eq!(parse_boolean_like(&json!(42)), None);
        assert_eq!(parse_boolean_like(&Value::Null), None);
    }

    use serde_json::Value;

    #[test]
    fn remote_plain_http_detected() {
        let url = url::Url::parse("http://api.example.com").unwrap();
        assert!(is_remote_plain_http(&url));
        let url = url::Url::parse("https://api.example.com").unwrap();
        assert!(!is_remote_plain_http(&url));
        let url = url::Url::parse("http://127.0.0.1:8642").unwrap();
        assert!(!is_remote_plain_http(&url));
        let url = url::Url::parse("http://localhost:9119").unwrap();
        assert!(!is_remote_plain_http(&url));
    }

    #[test]
    fn escape_hatch_must_be_explicit() {
        let config = json!({});
        assert!(!allows_insecure_remote_http(&config));
        let config = json!({INSECURE_REMOTE_HTTP_ESCAPE_HATCH: false});
        assert!(!allows_insecure_remote_http(&config));
        let config = json!({INSECURE_REMOTE_HTTP_ESCAPE_HATCH: "false"});
        assert!(!allows_insecure_remote_http(&config));
        let config = json!({INSECURE_REMOTE_HTTP_ESCAPE_HATCH: true});
        assert!(allows_insecure_remote_http(&config));
        let config = json!({INSECURE_REMOTE_HTTP_ESCAPE_HATCH: "yes"});
        assert!(allows_insecure_remote_http(&config));
    }

    #[test]
    fn validate_loopback_http_passes() {
        let config = json!({});
        assert!(validate_api_base_url(&config, "http://127.0.0.1:8642").is_ok());
        assert!(validate_api_base_url(&config, "http://localhost:9119").is_ok());
    }

    #[test]
    fn validate_remote_https_passes() {
        let config = json!({});
        assert!(validate_api_base_url(&config, "https://api.hermes.example.com").is_ok());
    }

    #[test]
    fn validate_remote_http_rejected_without_escape_hatch() {
        let config = json!({});
        let err = validate_api_base_url(&config, "http://api.hermes.example.com").unwrap_err();
        assert!(err.contains("remote plain HTTP"));
        assert!(err.contains("api.hermes.example.com"));
    }

    #[test]
    fn validate_remote_http_allowed_with_escape_hatch() {
        let config = json!({INSECURE_REMOTE_HTTP_ESCAPE_HATCH: true});
        assert!(validate_api_base_url(&config, "http://api.hermes.example.com").is_ok());
    }

    #[test]
    fn invalid_url_returns_error() {
        let config = json!({});
        let err = validate_api_base_url(&config, "not a url").unwrap_err();
        assert!(err.contains("invalid apiBaseUrl"));
    }
}
