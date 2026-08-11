//! Remote HTTP endpoint URL 校验 + 私有/保留 IP 防护。
//!
//! 对应 Node `server/src/services/remote-http-endpoint-guard.ts`（161 行）1:1 复刻。
//! （原 `pc-remote-http-endpoint-guard` crate 已下沉到 `pc-core`）。


use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use url::Url;

pub const DEFAULT_DNS_TIMEOUT_MS: u64 = 5_000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RemoteHttpEndpointError {
    #[error("Remote MCP connection requires config.url")]
    UrlMissing,
    #[error("Remote MCP connection URL is invalid")]
    UrlInvalid,
    #[error("Remote MCP connection URL must use http or https")]
    UrlProtocolInvalid,
    #[error("Remote MCP connection URL cannot target private or reserved network addresses")]
    PrivateEndpoint,
    #[error("Remote MCP connection hostname could not be resolved")]
    DnsFailed,
    #[error("Remote MCP connection hostname did not resolve")]
    DnsEmpty,
}

impl RemoteHttpEndpointError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UrlMissing => "mcp_remote_url_missing",
            Self::UrlInvalid => "mcp_remote_url_invalid",
            Self::UrlProtocolInvalid => "mcp_remote_url_invalid",
            Self::PrivateEndpoint => "remote_http_private_endpoint",
            Self::DnsFailed | Self::DnsEmpty => "remote_http_dns_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LookupResult {
    pub address: String,
    pub family: i32,
}

#[async_trait]
pub trait RemoteHttpEndpointLookup: Send + Sync {
    async fn lookup(&self, hostname: &str) -> Result<Vec<LookupResult>, String>;
}

#[derive(Clone, Default)]
pub struct RemoteHttpEndpointGuardOptions {
    pub allow_private_network: bool,
    pub dns_timeout_ms: Option<u64>,
    pub lookup: Option<Arc<dyn RemoteHttpEndpointLookup>>,
}

pub fn parse_remote_http_endpoint(value: &str) -> Result<Url, RemoteHttpEndpointError> {
    if value.trim().is_empty() {
        return Err(RemoteHttpEndpointError::UrlMissing);
    }
    let parsed = Url::parse(value).map_err(|_| RemoteHttpEndpointError::UrlInvalid)?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        _ => Err(RemoteHttpEndpointError::UrlProtocolInvalid),
    }
}

pub async fn assert_public_remote_http_endpoint(
    endpoint: &Url,
    options: &RemoteHttpEndpointGuardOptions,
) -> Result<(), RemoteHttpEndpointError> {
    if options.allow_private_network {
        return Ok(());
    }

    let hostname = endpoint
        .host_str()
        .map(|h| h.trim_start_matches('[').trim_end_matches(']').to_lowercase())
        .unwrap_or_default();

    if hostname == "localhost" || hostname.ends_with(".localhost") {
        return Err(RemoteHttpEndpointError::PrivateEndpoint);
    }

    if ip_version(&hostname) != 0 {
        if is_private_or_reserved_ip(&hostname) {
            return Err(RemoteHttpEndpointError::PrivateEndpoint);
        }
        return Ok(());
    }

    let timeout_ms = options.dns_timeout_ms.unwrap_or(DEFAULT_DNS_TIMEOUT_MS);
    let lookup: Arc<dyn RemoteHttpEndpointLookup> = options
        .lookup
        .clone()
        .unwrap_or_else(|| Arc::new(NoopLookup));

    let results = match lookup_with_timeout(&hostname, lookup, timeout_ms).await {
        Ok(r) => r,
        Err(_) => return Err(RemoteHttpEndpointError::DnsFailed),
    };
    if results.is_empty() {
        return Err(RemoteHttpEndpointError::DnsEmpty);
    }
    if results.iter().any(|r| is_private_or_reserved_ip(&r.address)) {
        return Err(RemoteHttpEndpointError::PrivateEndpoint);
    }
    Ok(())
}

struct NoopLookup;

#[async_trait]
impl RemoteHttpEndpointLookup for NoopLookup {
    async fn lookup(&self, _: &str) -> Result<Vec<LookupResult>, String> {
        Ok(vec![])
    }
}

async fn lookup_with_timeout(
    hostname: &str,
    lookup: Arc<dyn RemoteHttpEndpointLookup>,
    timeout_ms: u64,
) -> Result<Vec<LookupResult>, String> {
    tokio::time::timeout(Duration::from_millis(timeout_ms), lookup.lookup(hostname))
        .await
        .map_err(|_| format!("DNS lookup timed out for {hostname}"))?
}

fn ip_version(hostname: &str) -> i32 {
    if hostname.parse::<Ipv4Addr>().is_ok() {
        4
    } else if hostname.parse::<Ipv6Addr>().is_ok() {
        6
    } else {
        0
    }
}

fn is_private_or_reserved_ip(address: &str) -> bool {
    let lower = address.to_lowercase();
    if let Some(rest) = lower.strip_prefix("::ffff:") {
        if rest.contains(':') {
            if let Some(ipv4) = parse_mapped_ipv4_hex(&lower) {
                return is_private_or_reserved_ipv4(&ipv4);
            }
        } else {
            return is_private_or_reserved_ipv4(rest);
        }
    }
    if let Some(ipv4) = parse_mapped_ipv4_hex(&lower) {
        return is_private_or_reserved_ipv4(&ipv4);
    }
    let version = ip_version(&lower);
    if version == 4 {
        return is_private_or_reserved_ipv4(&lower);
    }
    if version == 6 {
        return is_private_or_reserved_ipv6(&lower);
    }
    true
}

fn is_private_or_reserved_ipv4(address: &str) -> bool {
    let Some(octets) = parse_ipv4_address(address) else {
        return true;
    };
    let (a, b, c) = (octets[0], octets[1], octets[2]);
    if a == 0 { return true; }
    if a == 10 { return true; }
    if a == 100 && b >= 64 && b <= 127 { return true; }
    if a == 127 { return true; }
    if a == 169 && b == 254 { return true; }
    if a == 172 && b >= 16 && b <= 31 { return true; }
    if a == 192 && b == 0 && c == 0 { return true; }
    if a == 192 && b == 168 { return true; }
    if a == 192 && b == 0 && c == 2 { return true; }
    if a == 192 && b == 88 && c == 99 { return true; }
    if a == 198 && (b == 18 || b == 19) { return true; }
    if a == 198 && b == 51 && c == 100 { return true; }
    if a == 203 && b == 0 && c == 113 { return true; }
    if a >= 224 { return true; }
    false
}

fn parse_ipv4_address(address: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = address.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (i, part) in parts.iter().enumerate() {
        if !part.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let n: u32 = part.parse().ok()?;
        if n > 255 {
            return None;
        }
        octets[i] = n as u8;
    }
    Some(octets)
}

fn parse_mapped_ipv4_hex(address: &str) -> Option<String> {
    let rest = address.strip_prefix("::ffff:")?;
    if !rest.contains(':') {
        return None;
    }
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let hi = u16::from_str_radix(parts[0], 16).ok()?;
    let lo = u16::from_str_radix(parts[1], 16).ok()?;
    Some(format!(
        "{}.{}.{}.{}",
        (hi >> 8) as u8,
        (hi & 0xff) as u8,
        (lo >> 8) as u8,
        (lo & 0xff) as u8
    ))
}

fn is_private_or_reserved_ipv6(address: &str) -> bool {
    let lower = address.to_lowercase();
    if lower == "::" || lower == "::1" {
        return true;
    }
    if lower.starts_with("fc") || lower.starts_with("fd") {
        return true;
    }
    if lower.starts_with("fe8") || lower.starts_with("fe9")
        || lower.starts_with("fea") || lower.starts_with("feb") {
        return true;
    }
    if lower.starts_with("ff") {
        return true;
    }
    if lower == "100::" || lower.starts_with("100:") {
        return true;
    }
    if lower.starts_with("2001:0:") || lower.starts_with("2001::") {
        return true;
    }
    if lower.starts_with("2001:db8:") || lower == "2001:db8::" {
        return true;
    }
    if lower.starts_with("2001:2:") || lower == "2001:2::" {
        return true;
    }
    if lower.starts_with("2001:2") {
        return true;
    }
    if lower.starts_with("2002:") {
        return true;
    }
    if lower.starts_with("64:ff9b:") {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r720_parse_url_missing() {
        assert_eq!(parse_remote_http_endpoint("").unwrap_err(), RemoteHttpEndpointError::UrlMissing);
        assert_eq!(parse_remote_http_endpoint("   ").unwrap_err(), RemoteHttpEndpointError::UrlMissing);
    }

    #[test]
    fn r720_parse_url_invalid() {
        assert_eq!(parse_remote_http_endpoint("not-a-url").unwrap_err(), RemoteHttpEndpointError::UrlInvalid);
    }

    #[test]
    fn r720_parse_url_wrong_protocol() {
        assert_eq!(parse_remote_http_endpoint("ftp://example.com").unwrap_err(), RemoteHttpEndpointError::UrlProtocolInvalid);
        assert_eq!(parse_remote_http_endpoint("ws://example.com").unwrap_err(), RemoteHttpEndpointError::UrlProtocolInvalid);
    }

    #[test]
    fn r720_parse_url_http_ok() {
        let u = parse_remote_http_endpoint("http://example.com").unwrap();
        assert_eq!(u.scheme(), "http");
    }

    #[test]
    fn r720_parse_url_https_ok() {
        let u = parse_remote_http_endpoint("https://example.com").unwrap();
        assert_eq!(u.scheme(), "https");
    }

    #[test]
    fn r720_ipv4_public() {
        assert!(!is_private_or_reserved_ipv4("8.8.8.8"));
        assert!(!is_private_or_reserved_ipv4("1.1.1.1"));
    }

    #[test]
    fn r720_ipv4_rfc1918() {
        assert!(is_private_or_reserved_ipv4("10.0.0.1"));
        assert!(is_private_or_reserved_ipv4("172.16.0.1"));
        assert!(is_private_or_reserved_ipv4("192.168.1.1"));
    }

    #[test]
    fn r720_ipv4_loopback() {
        assert!(is_private_or_reserved_ipv4("127.0.0.1"));
    }

    #[test]
    fn r720_ipv4_zero() {
        assert!(is_private_or_reserved_ipv4("0.0.0.0"));
    }

    #[test]
    fn r720_ipv4_link_local() {
        assert!(is_private_or_reserved_ipv4("169.254.0.1"));
    }

    #[test]
    fn r720_ipv4_cgn() {
        assert!(is_private_or_reserved_ipv4("100.64.0.1"));
    }

    #[test]
    fn r720_ipv4_documentation() {
        assert!(is_private_or_reserved_ipv4("192.0.2.1"));
        assert!(is_private_or_reserved_ipv4("198.51.100.1"));
        assert!(is_private_or_reserved_ipv4("203.0.113.1"));
    }

    #[test]
    fn r720_ipv4_benchmark() {
        assert!(is_private_or_reserved_ipv4("198.18.0.1"));
    }

    #[test]
    fn r720_ipv4_multicast_reserved() {
        assert!(is_private_or_reserved_ipv4("224.0.0.1"));
        assert!(is_private_or_reserved_ipv4("240.0.0.1"));
        assert!(is_private_or_reserved_ipv4("255.255.255.255"));
    }

    #[test]
    fn r720_ipv4_invalid_format() {
        assert!(is_private_or_reserved_ipv4("1.2.3"));
        assert!(is_private_or_reserved_ipv4("1.2.3.4.5"));
        assert!(is_private_or_reserved_ipv4("1.2.3.300"));
    }

    #[test]
    fn r720_ipv6_public() {
        assert!(!is_private_or_reserved_ipv6("2001:4860:4860::8888"));
    }

    #[test]
    fn r720_ipv6_loopback_unspec() {
        assert!(is_private_or_reserved_ipv6("::"));
        assert!(is_private_or_reserved_ipv6("::1"));
    }

    #[test]
    fn r720_ipv6_unique_local() {
        assert!(is_private_or_reserved_ipv6("fc00::1"));
        assert!(is_private_or_reserved_ipv6("fd00::1"));
    }

    #[test]
    fn r720_ipv6_link_local() {
        assert!(is_private_or_reserved_ipv6("fe80::1"));
        assert!(is_private_or_reserved_ipv6("fea0::1"));
    }

    #[test]
    fn r720_ipv6_multicast() {
        assert!(is_private_or_reserved_ipv6("ff02::1"));
    }

    #[test]
    fn r720_ipv6_discard() {
        assert!(is_private_or_reserved_ipv6("100::"));
        assert!(is_private_or_reserved_ipv6("100:0:0::1"));
    }

    #[test]
    fn r720_ipv6_documentation() {
        assert!(is_private_or_reserved_ipv6("2001:db8::1"));
    }

    #[test]
    fn r720_ipv6_6to4() {
        assert!(is_private_or_reserved_ipv6("2002::1"));
    }

    #[test]
    fn r720_ipv4_mapped_dotted() {
        assert!(is_private_or_reserved_ip("::ffff:10.0.0.1"));
    }

    #[test]
    fn r720_ipv4_mapped_hex_private() {
        // ::ffff:0a00:0001 -> 10.0.0.1
        assert!(is_private_or_reserved_ip("::ffff:0a00:0001"));
    }

    #[test]
    fn r720_ipv4_mapped_hex_public() {
        // ::ffff:0808:0808 -> 8.8.8.8
        assert!(!is_private_or_reserved_ip("::ffff:0808:0808"));
    }

    #[tokio::test]
    async fn r720_assert_allow_private_network_skips() {
        let url = parse_remote_http_endpoint("http://localhost:8080").unwrap();
        let opts = RemoteHttpEndpointGuardOptions { allow_private_network: true, ..Default::default() };
        assert_public_remote_http_endpoint(&url, &opts).await.unwrap();
    }

    #[tokio::test]
    async fn r720_assert_localhost_rejected() {
        let url = parse_remote_http_endpoint("http://localhost").unwrap();
        let opts = RemoteHttpEndpointGuardOptions::default();
        assert_eq!(assert_public_remote_http_endpoint(&url, &opts).await.unwrap_err(), RemoteHttpEndpointError::PrivateEndpoint);
    }

    #[tokio::test]
    async fn r720_assert_dot_localhost_rejected() {
        let url = parse_remote_http_endpoint("http://api.localhost").unwrap();
        let opts = RemoteHttpEndpointGuardOptions::default();
        assert_eq!(assert_public_remote_http_endpoint(&url, &opts).await.unwrap_err(), RemoteHttpEndpointError::PrivateEndpoint);
    }

    #[tokio::test]
    async fn r720_assert_literal_private_ip_rejected() {
        let url = parse_remote_http_endpoint("http://10.0.0.1").unwrap();
        let opts = RemoteHttpEndpointGuardOptions::default();
        assert_eq!(assert_public_remote_http_endpoint(&url, &opts).await.unwrap_err(), RemoteHttpEndpointError::PrivateEndpoint);
    }

    #[tokio::test]
    async fn r720_assert_literal_public_ip_ok() {
        let url = parse_remote_http_endpoint("http://8.8.8.8").unwrap();
        let opts = RemoteHttpEndpointGuardOptions::default();
        assert_public_remote_http_endpoint(&url, &opts).await.unwrap();
    }

    #[tokio::test]
    async fn r720_assert_dns_resolves_to_private_rejected() {
        struct FakeLookup;
        #[async_trait]
        impl RemoteHttpEndpointLookup for FakeLookup {
            async fn lookup(&self, _: &str) -> Result<Vec<LookupResult>, String> {
                Ok(vec![LookupResult { address: "10.0.0.1".into(), family: 4 }])
            }
        }
        let url = parse_remote_http_endpoint("https://example.com").unwrap();
        let opts = RemoteHttpEndpointGuardOptions { lookup: Some(Arc::new(FakeLookup)), ..Default::default() };
        assert_eq!(assert_public_remote_http_endpoint(&url, &opts).await.unwrap_err(), RemoteHttpEndpointError::PrivateEndpoint);
    }

    #[tokio::test]
    async fn r720_assert_dns_resolves_to_public_ok() {
        struct FakeLookup;
        #[async_trait]
        impl RemoteHttpEndpointLookup for FakeLookup {
            async fn lookup(&self, _: &str) -> Result<Vec<LookupResult>, String> {
                Ok(vec![LookupResult { address: "8.8.8.8".into(), family: 4 }])
            }
        }
        let url = parse_remote_http_endpoint("https://example.com").unwrap();
        let opts = RemoteHttpEndpointGuardOptions { lookup: Some(Arc::new(FakeLookup)), ..Default::default() };
        assert_public_remote_http_endpoint(&url, &opts).await.unwrap();
    }

    #[tokio::test]
    async fn r720_assert_dns_empty_rejected() {
        struct FakeLookup;
        #[async_trait]
        impl RemoteHttpEndpointLookup for FakeLookup {
            async fn lookup(&self, _: &str) -> Result<Vec<LookupResult>, String> {
                Ok(vec![])
            }
        }
        let url = parse_remote_http_endpoint("https://example.com").unwrap();
        let opts = RemoteHttpEndpointGuardOptions { lookup: Some(Arc::new(FakeLookup)), ..Default::default() };
        assert_eq!(assert_public_remote_http_endpoint(&url, &opts).await.unwrap_err(), RemoteHttpEndpointError::DnsEmpty);
    }

    #[tokio::test]
    async fn r720_assert_dns_failure_rejected() {
        struct FakeLookup;
        #[async_trait]
        impl RemoteHttpEndpointLookup for FakeLookup {
            async fn lookup(&self, _: &str) -> Result<Vec<LookupResult>, String> {
                Err("dns error".into())
            }
        }
        let url = parse_remote_http_endpoint("https://example.com").unwrap();
        let opts = RemoteHttpEndpointGuardOptions { lookup: Some(Arc::new(FakeLookup)), ..Default::default() };
        assert_eq!(assert_public_remote_http_endpoint(&url, &opts).await.unwrap_err(), RemoteHttpEndpointError::DnsFailed);
    }

    #[tokio::test]
    async fn r720_assert_dns_timeout_rejected() {
        struct SlowLookup;
        #[async_trait]
        impl RemoteHttpEndpointLookup for SlowLookup {
            async fn lookup(&self, _: &str) -> Result<Vec<LookupResult>, String> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(vec![])
            }
        }
        let url = parse_remote_http_endpoint("https://example.com").unwrap();
        let opts = RemoteHttpEndpointGuardOptions { lookup: Some(Arc::new(SlowLookup)), dns_timeout_ms: Some(50), ..Default::default() };
        assert_eq!(assert_public_remote_http_endpoint(&url, &opts).await.unwrap_err(), RemoteHttpEndpointError::DnsFailed);
    }

    #[tokio::test]
    async fn r720_assert_bracket_ipv6_host() {
        let url = Url::parse("http://[::1]/").unwrap();
        let opts = RemoteHttpEndpointGuardOptions::default();
        assert_eq!(assert_public_remote_http_endpoint(&url, &opts).await.unwrap_err(), RemoteHttpEndpointError::PrivateEndpoint);
    }

    #[test]
    fn r720_error_codes_match_node() {
        assert_eq!(RemoteHttpEndpointError::UrlMissing.code(), "mcp_remote_url_missing");
        assert_eq!(RemoteHttpEndpointError::UrlInvalid.code(), "mcp_remote_url_invalid");
        assert_eq!(RemoteHttpEndpointError::UrlProtocolInvalid.code(), "mcp_remote_url_invalid");
        assert_eq!(RemoteHttpEndpointError::PrivateEndpoint.code(), "remote_http_private_endpoint");
        assert_eq!(RemoteHttpEndpointError::DnsFailed.code(), "remote_http_dns_failed");
        assert_eq!(RemoteHttpEndpointError::DnsEmpty.code(), "remote_http_dns_failed");
    }

    #[test]
    fn r720_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RemoteHttpEndpointError>();
        assert_send_sync::<RemoteHttpEndpointGuardOptions>();
        assert_send_sync::<LookupResult>();
    }
}
