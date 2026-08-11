//! OpenClaw Gateway transport security — 对齐 Node
//! `execute.ts::isLoopbackHost` + URL 安全校验 + escape hatch。
//!
//! 与 Hermes-gateway 的 `transport_security` 同款模式：
//! loopback 始终允许；远端需 HTTPS；显式 escape hatch 才允许 http。

#![allow(dead_code)]

use serde_json::Value;

/// 已知 loopback host 列表（与 Node `isLoopbackHost` 等价）。
pub const LOOPBACK_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1", "[::1]", "0.0.0.0"];

/// `isLoopbackHost` —— 严格匹配 Node：大小写不敏感。
pub fn is_loopback_host(hostname: &str) -> bool {
    let h = hostname
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let normalized = h.to_lowercase();
    LOOPBACK_HOSTS.iter().any(|known| *known == normalized)
}

/// Escape hatch —— 显式允许远端 plain http。
pub const ESCAPE_HATCH_KEY: &str = "allowInsecureRemoteHttp";

/// 检查 config 里 escape hatch 是否开启（bool / 别名）。
pub fn allows_insecure_remote_http(config: &Value) -> bool {
    parse_bool_like(config.get(ESCAPE_HATCH_KEY), false)
}

/// 接收任意 `Value` 解析 bool，接受 8 种别名。
pub fn parse_bool_like(v: Option<&Value>, fallback: bool) -> bool {
    let Some(v) = v else {
        return fallback;
    };
    if let Some(b) = v.as_bool() {
        return b;
    }
    if let Some(s) = v.as_str() {
        let normalized = s.trim().to_lowercase();
        return match normalized.as_str() {
            "1" | "true" | "yes" | "on" | "enabled" | "allow" => true,
            "0" | "false" | "no" | "off" | "disabled" | "deny" => false,
            _ => fallback,
        };
    }
    fallback
}

/// 拒绝消息生成器。
pub fn remote_plain_http_denied_message(host: &str) -> String {
    format!(
        "OpenClaw Gateway over plain HTTP to a remote host is denied ({host}). \
         Use https:// or set `{ESCAPE_HATCH_KEY}=true` to override for local testing."
    )
}

/// `validateGatewayUrl` —— 在 Adapter execute 入口调用。
///
/// 校验：
/// 1. URL 合法
/// 2. scheme ∈ {ws, wss, http, https}
/// 3. loopback → 始终允许
/// 4. 远端 http/http(scheme=ws/http) → 当 escape hatch 关闭时拒绝
/// 5. wss:// 远端始终允许
pub fn validate_gateway_url(config: &Value, url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid gatewayUrl: {e}"))?;
    let scheme = parsed.scheme();
    let host = parsed.host_str().unwrap_or("");

    if !matches!(scheme, "ws" | "wss" | "http" | "https") {
        return Err(format!("unsupported scheme: {scheme}"));
    }

    let is_loopback = is_loopback_host(host);
    let is_plain = matches!(scheme, "ws" | "http");
    if !is_loopback && is_plain && !allows_insecure_remote_http(config) {
        return Err(remote_plain_http_denied_message(host));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn loopback_hosts_recognized() {
        for host in [
            "localhost",
            "127.0.0.1",
            "::1",
            "[::1]",
            "0.0.0.0",
            "LOCALHOST",
        ] {
            assert!(is_loopback_host(host), "{host}");
        }
    }

    #[test]
    fn non_loopback_hosts_rejected() {
        for host in ["example.com", "192.168.1.1", "10.0.0.5", "::2", ""] {
            assert!(!is_loopback_host(host), "{host}");
        }
    }

    #[test]
    fn loopback_always_allowed_for_plain_http() {
        let cfg = json!({});
        assert!(validate_gateway_url(&cfg, "ws://localhost:9000/ws").is_ok());
        assert!(validate_gateway_url(&cfg, "http://127.0.0.1:8080/").is_ok());
    }

    #[test]
    fn remote_https_allowed() {
        let cfg = json!({});
        assert!(validate_gateway_url(&cfg, "https://gateway.openclaw.example/v1").is_ok());
        assert!(validate_gateway_url(&cfg, "wss://gateway.openclaw.example/v1").is_ok());
    }

    #[test]
    fn remote_plain_http_denied_without_escape_hatch() {
        let cfg = json!({});
        let err = validate_gateway_url(&cfg, "http://gateway.openclaw.example/v1").unwrap_err();
        assert!(err.contains("denied"));
        assert!(err.contains("gateway.openclaw.example"));
    }

    #[test]
    fn remote_plain_http_allowed_with_escape_hatch() {
        let cfg = json!({ESCAPE_HATCH_KEY: true});
        assert!(validate_gateway_url(&cfg, "http://gateway.openclaw.example/v1").is_ok());
    }

    #[test]
    fn escape_hatch_aliases_recognized() {
        for v in ["true", "1", "yes", "on", "enabled", "allow"] {
            assert!(allows_insecure_remote_http(&json!({ESCAPE_HATCH_KEY: v})));
        }
    }

    #[test]
    fn escape_hatch_disabled_aliases_recognized() {
        for v in ["false", "0", "no", "off", "disabled", "deny"] {
            assert!(!allows_insecure_remote_http(&json!({ESCAPE_HATCH_KEY: v})));
        }
    }

    #[test]
    fn parse_bool_like_returns_fallback_for_unknown() {
        assert!(parse_bool_like(None, true));
        assert!(!parse_bool_like(None, false));
        assert!(parse_bool_like(Some(&json!("garbage")), true));
        assert!(!parse_bool_like(Some(&json!("garbage")), false));
        // unrecognized type returns fallback (test passes true here = expect true)
        assert!(parse_bool_like(Some(&json!([])), true)); // array - fallback true
        assert!(!parse_bool_like(Some(&json!([])), false)); // array - fallback false
    }

    #[test]
    fn parse_bool_like_handles_bool_and_string_mixing() {
        assert!(parse_bool_like(Some(&json!(true)), false));
        assert!(!parse_bool_like(Some(&json!(false)), true));
        assert!(parse_bool_like(Some(&json!("True")), false));
        assert!(!parse_bool_like(Some(&json!("FALSE")), true));
    }

    #[test]
    fn remote_plain_http_denied_message_includes_host_and_hint() {
        let m = remote_plain_http_denied_message("example.com");
        assert!(m.contains("example.com"));
        assert!(m.contains(ESCAPE_HATCH_KEY));
    }

    #[test]
    fn validate_gateway_url_rejects_invalid_url() {
        let cfg = json!({});
        assert!(validate_gateway_url(&cfg, "not a url").is_err());
    }

    #[test]
    fn validate_gateway_url_rejects_unsupported_scheme() {
        let cfg = json!({});
        let err = validate_gateway_url(&cfg, "ftp://localhost/foo").unwrap_err();
        assert!(err.contains("unsupported scheme"));
    }

    #[test]
    fn validate_gateway_url_accepts_loopback_websocket() {
        let cfg = json!({});
        assert!(validate_gateway_url(&cfg, "ws://localhost:9000").is_ok());
        assert!(validate_gateway_url(&cfg, "ws://127.0.0.1:9000").is_ok());
        assert!(validate_gateway_url(&cfg, "wss://localhost:9000").is_ok());
    }

    #[test]
    fn validate_gateway_url_no_host() {
        let cfg = json!({});
        // Some URLs have no host (rare) — defaults to empty string and treat as non-loopback
        assert!(validate_gateway_url(&cfg, "wss:///foo").is_ok()); // actually defaults to empty
    }
}
