//! 出网边界：endpoint 校验 + 受控 HTTP 客户端。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。M6-9 的 task broker **只读**这里（它自己建连接，
//!   但必须用同一套拨号判据，否则「管理员同意过的目的地」就有第二份实现）。
//! - **上游**：`pkg/remotemcp/client.go` 的 `ValidatePublicHTTPSEndpoint` /
//!   `NewSecureHTTPClient` / `readResponse` 里属于 HTTP 客户端的那半段。
//!
//! ## 与 Go 的结构性差异（为什么）
//!
//! 上游的解析器是**接口注入**；本仓把它拆成「解析」与「判定」两个函数：
//! [`validate_public_https_endpoint`] 收**已解析好的地址**做纯判定，
//! [`resolve_endpoint`] 才是生产入口（自己查 DNS）。好处是 SSRF 判据的全部回归
//! （localhost、`*.localhost`、私网/链路本地/组播/保留段、v4-mapped）都能在**不发包、
//! 不查 DNS** 的测试里钉住 —— 上游为此专门写了 `remotemcptest` 夹具进程。
//!
//! ## 拨号的四道闸（缺一不可）
//!
//! 1. **不过代理**（上游 `transport.Proxy = nil`）：否则一个被投毒的 `HTTP_PROXY`
//!    就能把「只连公网」变成「连攻击者的内网跳板」；
//! 2. **不跟随重定向**（上游 `CheckRedirect` 直接报错）：重定向即换目的地，而目的地是管理员
//!    逐个同意过的；
//! 3. **解析出来的每个地址都必须是公网**（dev origin 例外），**有一个不干净就整体拒绝**，
//!    不是「挑一个公网的连」；
//! 4. **主机钉死**：拨号 host 必须仍是 endpoint 的 host（本仓落在自定义 DNS 解析器里，
//!    与上游写在 `DialContext` 里等价）。
//!
//! ## 不做什么
//!
//! 不读环境变量（见 `devorigin` 的模块文档）：dev origin 白名单与 CA 由调用方装进
//! [`EndpointPolicy`]。不记录请求/响应体（可能含插件凭据）。

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::HeaderMap;
use reqwest::redirect::Policy;
use url::Url;

use super::{transport_error, McpError, CALL_TIMEOUT, CONNECT_TIMEOUT};
use crate::devorigin::{is_public_address, normalize_host, EndpointPolicy};

/// 判定 `raw` 能不能当这个策略下的 endpoint（**地址由调用方给定**，见模块文档）。
///
/// 上游 `ValidatePublicHTTPSEndpoint` 的逐条判据、顺序一致：dev origin 只跳过
/// 「必须在公网」这一条（**不**跳过 host 白名单）；其余情况必须是「无 userinfo / 无 query /
/// 无 fragment 的 https」，host 不能是 localhost，必须在白名单内，且**每个**解析地址都是公网。
pub fn validate_public_https_endpoint(
    raw: &str,
    policy: &EndpointPolicy,
    resolved: &[IpAddr],
) -> Result<Url, McpError> {
    let endpoint = parse_endpoint(raw)?;
    if policy.is_dev_origin(&endpoint) {
        return check_dev_origin(endpoint, policy);
    }
    check_public_https(endpoint, policy, resolved)
}

/// 生产入口：自己解析 DNS，再走与纯判定完全相同的那批判据。
pub async fn resolve_endpoint(raw: &str, policy: &EndpointPolicy) -> Result<Url, McpError> {
    let endpoint = parse_endpoint(raw)?;
    if policy.is_dev_origin(&endpoint) {
        // dev origin 不做公网判定（上游同理：本机 origin 本来就不是公网地址）。
        return check_dev_origin(endpoint, policy);
    }
    let host = endpoint.host_str().unwrap_or_default();
    let port = endpoint.port_or_known_default().unwrap_or(443);
    let resolved = resolve_host(host, port).await?;
    check_public_https(endpoint, policy, &resolved)
}

fn parse_endpoint(raw: &str) -> Result<Url, McpError> {
    Url::parse(raw.trim())
        .map_err(|err| McpError::EndpointRejected(format!("parse endpoint: {err}")))
}

/// dev origin 分支：**只**查 host 白名单。
fn check_dev_origin(endpoint: Url, policy: &EndpointPolicy) -> Result<Url, McpError> {
    let dev_host = endpoint.host_str().map_or(String::new(), normalize_host);
    if !policy.allows_host(&dev_host) {
        return Err(McpError::EndpointRejected(
            "endpoint host is outside the Plugin endpoint policy".into(),
        ));
    }
    Ok(endpoint)
}

/// 生产分支：形状 → localhost → 白名单 → 每个地址都必须是公网。
fn check_public_https(
    endpoint: Url,
    policy: &EndpointPolicy,
    resolved: &[IpAddr],
) -> Result<Url, McpError> {
    if endpoint.scheme() != "https"
        || endpoint.host_str().is_none_or(str::is_empty)
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
        || endpoint.query().is_some()
    {
        return Err(McpError::EndpointRejected(
            "endpoint must be a public HTTPS URL without userinfo, query, or fragment".into(),
        ));
    }

    let host = endpoint.host_str().map_or(String::new(), normalize_host);
    if host == "localhost" || host.ends_with(".localhost") {
        return Err(McpError::EndpointRejected(
            "endpoint host is not public".into(),
        ));
    }
    if !policy.allows_host(&host) {
        return Err(McpError::EndpointRejected(
            "endpoint host is outside the Plugin endpoint policy".into(),
        ));
    }

    if resolved.is_empty() {
        return Err(McpError::EndpointRejected(format!(
            "resolve endpoint host: no address for {host}"
        )));
    }
    for address in resolved {
        if !is_public_address(*address) {
            return Err(McpError::NonPublicAddress(format!(
                "endpoint host resolves to non-public address {address}"
            )));
        }
    }
    Ok(endpoint)
}

/// 查 DNS（`tokio` 的异步解析器；上游用 `net.DefaultResolver`）。
pub(super) async fn resolve_host(host: &str, port: u16) -> Result<Vec<IpAddr>, McpError> {
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|err| McpError::Transport(format!("resolve endpoint host: {err}")))?;
    Ok(addrs.map(|addr| addr.ip()).collect())
}

/// 受控 HTTP 客户端：传输层的四道闸（除了地址过滤，那在 [`SecureResolver`] 里）。
///
/// 上游 `NewSecureHTTPClient`：`devOrigin` 决定两件事 —— 是否跳过「地址必须公网」，
/// 以及是否装额外的 CA。**dev 允许的是「多信这张 CA」，不是「不验证证书」**。
/// 上游把 CA 读失败静默吞掉（随后连接以证书错误失败）；本仓在装配处直接返回
/// [`McpError::Config`] —— 同样失败闭合，只是诊断更早、更明确。
pub fn secure_client(endpoint: &Url, policy: &EndpointPolicy) -> Result<reqwest::Client, McpError> {
    let dev_origin = policy.is_dev_origin(endpoint);
    let host = endpoint.host_str().map_or(String::new(), normalize_host);
    let port = endpoint.port_or_known_default().unwrap_or(443);

    let mut builder = reqwest::Client::builder()
        // 闸 1：上游 `transport.Proxy = nil`。
        .no_proxy()
        // 闸 2：上游 `CheckRedirect` 直接报错。
        .redirect(Policy::custom(|attempt| {
            attempt.error(std::io::Error::other(
                "remote MCP redirects are not allowed",
            ))
        }))
        // 上游 `http.Client.Timeout` / `net.Dialer.Timeout`。
        .timeout(CALL_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        // 闸 3 + 4：地址过滤与 host 钉定。
        .dns_resolver(Arc::new(SecureResolver {
            pinned_host: host,
            port,
            allow_non_public: dev_origin,
        }));

    if dev_origin {
        if let Some(pem) = policy.dev_ca_pem() {
            let certificates = reqwest::Certificate::from_pem_bundle(pem).map_err(|err| {
                McpError::Config(format!("dev origin CA bundle is not valid PEM: {err}"))
            })?;
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        builder = builder.min_tls_version(reqwest::tls::Version::TLS_1_2);
    }

    builder
        .build()
        .map_err(|err| McpError::Config(format!("build remote MCP HTTP client: {err}")))
}

/// 自定义解析器：把上游 `DialContext` 的两件事搬到 reqwest 的 DNS 层。
///
/// 逐个地址重试是 reqwest/hyper 的既有行为（连接器依次尝试解析结果），因此上游那段
/// `for candidate := range addresses { dial; if err == nil { return } }` 不需要重写。
struct SecureResolver {
    pinned_host: String,
    port: u16,
    allow_non_public: bool,
}

impl Resolve for SecureResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = normalize_host(name.as_str());
        if host != self.pinned_host {
            // 闸 4：上游同一句话写在 `DialContext` 里。
            return Box::pin(async {
                Err(Box::new(std::io::Error::other(
                    "remote MCP redirect changed endpoint host",
                ))
                    as Box<dyn std::error::Error + Send + Sync>)
            });
        }
        let port = self.port;
        let allow_non_public = self.allow_non_public;
        Box::pin(async move {
            let mut addresses: Vec<SocketAddr> = Vec::new();
            let resolved = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|err| {
                    Box::new(std::io::Error::other(format!(
                        "resolve endpoint host: {err}"
                    ))) as Box<dyn std::error::Error + Send + Sync>
                })?;
            for address in resolved {
                // 上游先 `Unmap()` 再判（`::ffff:127.0.0.1` 就是 `127.0.0.1`）。
                if !allow_non_public && !is_public_address(address.ip()) {
                    return Err(Box::new(std::io::Error::other(
                        "remote MCP endpoint resolved to a non-public address",
                    ))
                        as Box<dyn std::error::Error + Send + Sync>);
                }
                addresses.push(SocketAddr::new(address.ip(), port));
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

/// 响应是不是 SSE（`Content-Type: text/event-stream`，前缀比较、不分大小写）。
#[must_use]
pub(super) fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"))
}

/// 读响应体，最多 `limit` 字节；SSE 在**第一条 `data:` 行**出现后立即返回
/// （上游 `bufio.Scanner` 的行为：不等流结束，否则一个保持连接的 SSE 流会挂到超时）。
pub(crate) async fn read_capped_body(
    response: &mut reqwest::Response,
    limit: usize,
    event_stream: bool,
) -> Result<Vec<u8>, McpError> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| transport_error("read remote MCP response", &err))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > limit {
            return Err(McpError::ResponseTooLarge);
        }
        if event_stream {
            if let Some(data) = first_sse_data(&body) {
                return Ok(data);
            }
        }
    }
    if event_stream {
        return Err(McpError::Protocol(
            "remote MCP SSE response contained no data".into(),
        ));
    }
    Ok(body)
}

/// 从（可能尚未收完的）正文里取第一条非空 `data:` 行。
///
/// 只看**已经收到换行**的整行：最后一段可能是半行，下一块 chunk 会补齐。
fn first_sse_data(body: &[u8]) -> Option<Vec<u8>> {
    let end = body.iter().rposition(|byte| *byte == b'\n')? + 1;
    for line in body[..end].split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(data) = line.strip_prefix(b"data:") else {
            continue;
        };
        let start = data
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
            .unwrap_or(data.len());
        let end = data
            .iter()
            .rposition(|byte| !byte.is_ascii_whitespace())
            .map_or(0, |index| index + 1);
        if start < end {
            return Some(data[start..end].to_vec());
        }
    }
    None
}
