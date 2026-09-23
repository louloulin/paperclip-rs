//! remote MCP 的 OAuth 授权（授权码 / token 交换 / 刷新）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。
//! - **上游**：`pkg/remotemcp/oauth.go`（+ `internal/service/plugin_mcp_transport.go` 的调用侧）。
//! - **本仓约定**：token 的**持久化**不是本文件的事 —— 走 `mc-secrets` 的 store（服务端密钥）
//!   ，**不要**把令牌塞进 `plugin_installation.config` 的 JSONB（那是给用户看的配置，会被
//!   原样下发/回显）。`configured_secrets` 只暴露**键名**，不暴露值（上游
//!   `pluginInstallationResponse.ConfiguredSecrets`）。
//! - **回调地址**：生产必须是本服务的公网 origin；开发态由 `devorigin.rs` 的白名单放行。
//! - **不做什么**：不做 PKCE 之外的额外加固（上游这一代就是授权码 + PKCE）。
//!
//! ## 发现链（上游 `DiscoverOAuth`，逐段照抄）
//!
//! 1. 先对 MCP endpoint 发一次 `initialize`，从 `WWW-Authenticate` 里读
//!    `resource_metadata="…"`（RFC 9728 的探测）；拿不到就退回
//!    `/.well-known/oauth-protected-resource<path>`；
//! 2. 取该文档里的 `authorization_servers[0]` 作 issuer，依次试
//!    `/.well-known/oauth-authorization-server<path>` 与 `/.well-known/openid-configuration<path>`
//!    （RFC 8414 + OIDC 兼容回退）；
//! 3. 只保留**安全、公开**的那部分元数据（`OAuthMetadata`：endpoint + scopes + token 认证方式），
//!    token/secret 永不进这个值。
//!
//! **每一步独立做 SSRF 判定**：所有 URL 都必须是公网 HTTPS（无 userinfo/query/fragment），
//! 且都用 `client::secure_client` 出网（禁代理、禁跨主机重定向、地址全部公网、主机钉死）。
//! 与 `client.rs` 的唯一差别：上游这条路径给 `ValidatePublicHTTPSEndpoint` 传的是
//! `allowedHosts = nil`（只按公网判据收窄，不看 host 白名单）—— 对应
//! [`EndpointPolicy::without_host_policy`]。
//!
//! ## 与 M6-0 桩文的一处出入
//!
//! 桩文写「.well-known 的自动发现不在本波范围」；但上游 `pkg/remotemcp/oauth.go`
//! **整个文件就是这条发现链**，且 M6-7 的「连接插件」路由要用它（高级覆盖路径还要用
//! [`validate_oauth_metadata`]），所以本片按上游逐字落地。真正「不在本波范围」的是**多租户 `IdP`
//! 发现**（一个 workspace 配一个通用 OIDC Provider），那是另一条路径。

use std::time::{Duration, SystemTime};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

use crate::client::{
    contains_string, read_capped_body, resolve_endpoint, secure_client, transport_error, McpError,
};
use crate::devorigin::EndpointPolicy;
use crate::types::sha256;

/// OAuth 相关响应的体量上限（上游 `maxOAuthResponseBytes`）。
pub const MAX_OAUTH_RESPONSE_BYTES: usize = 1 << 20;

/// 授权 URL 的形状错误（上游 `validateOAuthURL` 的那条字符串）。
const OAUTH_URL_POLICY_ERROR: &str = "URL must be public HTTPS without userinfo or fragment";

/// MCP OAuth 发现结果里**安全、公开**的那部分。token 与 client secret 永不进这里。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthMetadata {
    /// 被保护的资源本身（即 MCP endpoint）。
    #[serde(default)]
    pub resource_endpoint: String,
    #[serde(default)]
    pub authorization_endpoint: String,
    #[serde(default)]
    pub token_endpoint: String,
    /// RFC 8414 里这是**可选**的（没有它就只能走「预注册 client」高级路径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// `token_endpoint_auth_methods_supported`（缺省 = 对面没说 ⇒ 按 `none` 走 PKCE）。
    #[serde(default)]
    pub token_auth_methods: Vec<String>,
}

/// 动态注册（RFC 7591）拿到的 client。
///
/// `Debug` 是手写的：`client_secret` 打日志时遮蔽（上游 Go 结构体直接可打印）。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct OAuthClientRegistration {
    pub client_id: String,
    pub client_secret: String,
    pub token_endpoint_auth_method: String,
}

impl std::fmt::Debug for OAuthClientRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClientRegistration")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &if self.client_secret.is_empty() {
                    "<none>"
                } else {
                    "<redacted>"
                },
            )
            .field(
                "token_endpoint_auth_method",
                &self.token_endpoint_auth_method,
            )
            .finish()
    }
}

/// token endpoint 的响应。同样手写 `Debug` 遮蔽两个令牌。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub token_type: String,
    /// 对面声明的有效期秒数；`<= 0` = 没说。
    pub expires_in: i64,
    pub refresh_token: String,
    pub scope: String,
}

impl std::fmt::Debug for OAuthTokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthTokenResponse")
            .field("access_token", &"<redacted>")
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .field(
                "refresh_token",
                &if self.refresh_token.is_empty() {
                    "<none>"
                } else {
                    "<redacted>"
                },
            )
            .field("scope", &self.scope)
            .finish()
    }
}

#[derive(Debug, Default, Deserialize)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    resource: String,
    #[serde(default)]
    authorization_servers: Vec<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AuthorizationServerMetadata {
    #[serde(default)]
    authorization_endpoint: String,
    #[serde(default)]
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: String,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Vec<String>,
}

/// 动态注册的响应（RFC 7591）；缺字段按 Go 零值处理。
#[derive(Debug, Default, Deserialize)]
struct RegistrationResponse {
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    client_secret: String,
    #[serde(default)]
    token_endpoint_auth_method: String,
}

/// token endpoint 的原始响应；`expires_in` 在 RFC 6749 里只要求「秒数」，
/// 有的实现给字符串 ⇒ 这里先当 JSON 值收（见 `parse_expires_in`）。
#[derive(Debug, Default, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    token_type: String,
    #[serde(default)]
    expires_in: Option<Value>,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    scope: String,
}

/// 对**运维给的**覆盖配置施加与「发现来的元数据」**同一套** SSRF 边界。
pub async fn validate_oauth_metadata(
    metadata: &OAuthMetadata,
    policy: &EndpointPolicy,
) -> Result<(), McpError> {
    if metadata.authorization_endpoint.is_empty() || metadata.token_endpoint.is_empty() {
        return Err(McpError::Config(
            "OAuth metadata is missing required endpoints".into(),
        ));
    }
    let mut candidates = vec![
        metadata.authorization_endpoint.as_str(),
        metadata.token_endpoint.as_str(),
    ];
    if let Some(registration) = metadata.registration_endpoint.as_deref() {
        candidates.push(registration);
    }
    for raw in candidates {
        if raw.is_empty() {
            continue;
        }
        validate_oauth_url(raw, policy).await?;
    }
    Ok(())
}

/// 走完 MCP 的授权发现链（见模块头）。任何一步不满足就整体失败 —— 不做半可信降级。
pub async fn discover_oauth(
    raw_endpoint: &str,
    policy: &EndpointPolicy,
) -> Result<OAuthMetadata, McpError> {
    let endpoint = resolve_endpoint(raw_endpoint, policy).await?;

    let metadata_url = match probe_resource_metadata_url(&endpoint, policy).await {
        Some(probed) => probed,
        None => protected_resource_metadata_url(&endpoint),
    };
    let resource_url = validate_oauth_url(&metadata_url, policy)
        .await
        .map_err(|err| context(err, "protected resource metadata URL"))?;
    let resource: ProtectedResourceMetadata =
        get_oauth_json(&resource_url, policy, "load protected resource metadata")
            .await
            .map_err(|err| context(err, "load protected resource metadata"))?;
    let Some(authorization_server) = resource.authorization_servers.first() else {
        return Err(McpError::Config(
            "protected resource metadata does not advertise an authorization server".into(),
        ));
    };

    let issuer = validate_oauth_url(authorization_server, policy)
        .await
        .map_err(|err| context(err, "authorization server"))?;
    let mut discovered: Option<AuthorizationServerMetadata> = None;
    let mut discovery_error: Option<McpError> = None;
    for candidate in authorization_metadata_urls(&issuer) {
        let candidate_url = match validate_oauth_url(&candidate, policy).await {
            Ok(url) => url,
            Err(err) => {
                discovery_error = Some(err);
                continue;
            }
        };
        match get_oauth_json(&candidate_url, policy, "load authorization server metadata").await {
            Ok(metadata) => {
                discovered = Some(metadata);
                discovery_error = None;
                break;
            }
            Err(err) => discovery_error = Some(err),
        }
    }
    let Some(server) = discovered else {
        // 上游保留**最后一个**候选的错误（不是第一个）—— OIDC 回退失败时那是更有信息量的那个。
        let err = discovery_error.unwrap_or_else(|| {
            McpError::Config("authorization server metadata is unavailable".into())
        });
        return Err(context(err, "load authorization server metadata"));
    };

    let metadata = merge_metadata(&endpoint, &resource, &server)?;
    validate_oauth_metadata(&metadata, policy)
        .await
        .map_err(|err| context(err, "authorization server endpoint"))?;
    Ok(metadata)
}

/// 把「被保护资源文档 + 授权服务器文档」合成安全视图。
///
/// **发现链的全部判据都在这个纯函数里**（不碰网络）：资源必须就是本 endpoint、
/// 两个必需 endpoint 必须有、对面若声明了 PKCE 方法则必须含 `S256`。
fn merge_metadata(
    endpoint: &Url,
    resource: &ProtectedResourceMetadata,
    server: &AuthorizationServerMetadata,
) -> Result<OAuthMetadata, McpError> {
    if !resource.resource.is_empty() && resource.resource != endpoint.as_str() {
        return Err(McpError::Config(
            "protected resource metadata does not match the MCP endpoint".into(),
        ));
    }
    if server.authorization_endpoint.is_empty() || server.token_endpoint.is_empty() {
        return Err(McpError::Config(
            "authorization server metadata is missing required endpoints".into(),
        ));
    }
    if !server.code_challenge_methods_supported.is_empty()
        && !contains_string(&server.code_challenge_methods_supported, "S256")
    {
        return Err(McpError::Config(
            "authorization server does not support PKCE S256".into(),
        ));
    }
    Ok(OAuthMetadata {
        resource_endpoint: endpoint.as_str().to_string(),
        authorization_endpoint: server.authorization_endpoint.clone(),
        token_endpoint: server.token_endpoint.clone(),
        registration_endpoint: (!server.registration_endpoint.is_empty())
            .then(|| server.registration_endpoint.clone()),
        scopes: resource.scopes_supported.clone(),
        token_auth_methods: server.token_endpoint_auth_methods_supported.clone(),
    })
}

/// 探一次 `WWW-Authenticate` 里的 `resource_metadata="…"`。
///
/// 探测失败（连不上、没有该参数、body 读不动）都返回 `None` —— 退回 well-known 路径，
/// **不是**整个发现失败（上游 `probeResourceMetadataURL` 返回空串）。
async fn probe_resource_metadata_url(endpoint: &Url, policy: &EndpointPolicy) -> Option<String> {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "multica-oauth-discovery", "version": "1" },
        },
    });
    let client = secure_client(endpoint, policy).ok()?;
    let request = client
        .post(endpoint.as_str())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .body(serde_json::to_vec(&payload).ok()?);
    let mut response = request.send().await.ok()?;
    let _ = read_capped_body(&mut response, MAX_OAUTH_RESPONSE_BYTES, false).await;

    response
        .headers()
        .get_all(reqwest::header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(resource_metadata_parameter)
        .map(str::to_string)
}

/// 手写上游那条正则 `(?i)(?:^|[, ]+)resource_metadata="([^"]+)"` 的等价物。
///
/// 等价规则：`resource_metadata="` 命中处必须是串首，或**前一个字符**是 `,` / 空格
/// （正则里的 `[, ]+` 可以退到紧邻位置）；引号内的内容非空。
fn resource_metadata_parameter(challenge: &str) -> Option<&str> {
    const NEEDLE: &str = "resource_metadata=\"";
    let lower = challenge.to_ascii_lowercase();
    let mut from = 0;
    while let Some(offset) = lower[from..].find(NEEDLE) {
        let start = from + offset;
        let preceded_by_separator = challenge[..start]
            .chars()
            .next_back()
            .is_none_or(|previous| previous == ',' || previous == ' ');
        if preceded_by_separator {
            let rest = &challenge[start + NEEDLE.len()..];
            // 值必须非空且闭合；否则按正则的回溯继续往下找（不是立刻放弃整个串）。
            if let Some(closing) = rest.find('"') {
                let value = &rest[..closing];
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
        from = start + 1;
    }
    None
}

/// `/.well-known/oauth-protected-resource<path>`（RFC 9728）。
fn protected_resource_metadata_url(resource: &Url) -> String {
    let mut result = resource.clone();
    // 上游用 `TrimSuffix`：**只**去掉一个尾斜杠。
    let raw_path = resource.path();
    let path = raw_path.strip_suffix('/').unwrap_or(raw_path);
    result.set_path(&if path.is_empty() || path == "/" {
        "/.well-known/oauth-protected-resource".to_string()
    } else {
        format!("/.well-known/oauth-protected-resource{path}")
    });
    result.set_query(None);
    result.set_fragment(None);
    result.to_string()
}

/// RFC 8414 与 OIDC 兼容回退两个候选（顺序即尝试顺序）。
fn authorization_metadata_urls(issuer: &Url) -> Vec<String> {
    let raw_path = issuer.path();
    let path = raw_path.strip_suffix('/').unwrap_or(raw_path);
    ["oauth-authorization-server", "openid-configuration"]
        .into_iter()
        .map(|well_known| {
            let mut candidate = issuer.clone();
            candidate.set_path(&format!("/.well-known/{well_known}{path}"));
            candidate.set_query(None);
            candidate.set_fragment(None);
            candidate.to_string()
        })
        .collect()
}

/// OAuth 侧每一条 URL 的判据：公网 HTTPS、无 userinfo/fragment。
///
/// 与上游一致的两点：① 先做**形状**预检（错误文案同名），② 再交给
/// 「公网地址 + 主机钉死」那条路径，且这一路**不看 host 白名单**
/// （上游传 `allowedHosts = nil`）—— 于是 dev origin 仍然有效（`https://` 的 dev origin
/// 可以解析到私网地址），但生产 endpoint 的白名单不会顺带把 token endpoint 也收窄。
async fn validate_oauth_url(raw: &str, policy: &EndpointPolicy) -> Result<Url, McpError> {
    let parsed = Url::parse(raw.trim())
        .ok()
        .filter(|url| {
            url.scheme() == "https"
                && url.host_str().is_some_and(|host| !host.is_empty())
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
        })
        .ok_or_else(|| McpError::Config(OAUTH_URL_POLICY_ERROR.to_string()))?;

    // 判定用副本（上游清掉 RawQuery）；返回的仍是带 query 的那个 URL。
    let mut check = parsed.clone();
    check.set_query(None);
    resolve_endpoint(check.as_str(), &policy.without_host_policy()).await?;
    Ok(parsed)
}

/// `GET` 一个 JSON 文档，读体上限 [`MAX_OAUTH_RESPONSE_BYTES`]。
async fn get_oauth_json<T: serde::de::DeserializeOwned>(
    endpoint: &Url,
    policy: &EndpointPolicy,
    action: &str,
) -> Result<T, McpError> {
    let request = secure_client(endpoint, policy)?
        .get(endpoint.as_str())
        .header(reqwest::header::ACCEPT, "application/json");
    oauth_json(request, action).await
}

/// 发一个 OAuth 请求，收 2xx 的 JSON。非 2xx 只报状态码（错误页正文不进错误信息）。
async fn oauth_json<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
    action: &str,
) -> Result<T, McpError> {
    let mut response = request
        .send()
        .await
        .map_err(|err| transport_error(action, &err))?;
    let status = response.status();
    if !status.is_success() {
        // 上游会先把 body 丢进 Discard（≤ 1 MiB）再报错，这里同样不做「顺手解析错误体」。
        let _ = read_capped_body(&mut response, MAX_OAUTH_RESPONSE_BYTES, false).await;
        return Err(McpError::Protocol(format!(
            "{action}: HTTP {}",
            status.as_u16()
        )));
    }
    let body = match read_capped_body(&mut response, MAX_OAUTH_RESPONSE_BYTES, false).await {
        Ok(body) => body,
        // 超限保持原样：调用方靠 `code() = "mcp_response_too_large"` 区分，别被上下文包成协议错。
        Err(err @ McpError::ResponseTooLarge) => return Err(err),
        Err(other) => return Err(context(other, action)),
    };
    serde_json::from_slice(&body)
        .map_err(|err| McpError::Protocol(format!("{action}: decode OAuth JSON: {err}")))
}

/// RFC 7591 动态客户端注册。对面没有注册 endpoint ⇒ 只能走「预注册 client」高级路径。
pub async fn register_oauth_client(
    metadata: &OAuthMetadata,
    redirect_uri: &str,
    policy: &EndpointPolicy,
) -> Result<OAuthClientRegistration, McpError> {
    let Some(registration) = metadata.registration_endpoint.as_deref() else {
        return Err(McpError::Config(
            "authorization server requires a pre-registered OAuth client".into(),
        ));
    };
    let action = "dynamic client registration";
    let endpoint = validate_oauth_url(registration, policy).await?;
    let body = json!({
        "client_name": "Multica",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let request = secure_client(&endpoint, policy)?
        .post(endpoint.as_str())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .body(serde_json::to_vec(&body).map_err(|err| {
            McpError::Config(format!("{action}: encode registration request: {err}"))
        })?);

    let response: RegistrationResponse = oauth_json(request, action)
        .await
        .map_err(|err| context(err, action))?;
    if response.client_id.trim().is_empty() {
        return Err(McpError::Config(
            "dynamic client registration returned no client id".into(),
        ));
    }
    Ok(OAuthClientRegistration {
        client_id: response.client_id,
        client_secret: response.client_secret,
        token_endpoint_auth_method: if response.token_endpoint_auth_method.is_empty() {
            "none".to_string()
        } else {
            response.token_endpoint_auth_method
        },
    })
}

/// 拼授权 URL（`response_type=code` + PKCE S256 + `resource`）。
///
/// 唯一实现点：challenge = `base64url(sha256(verifier))`，别处不要再算一遍。
pub fn build_authorization_url(
    metadata: &OAuthMetadata,
    registration: &OAuthClientRegistration,
    redirect_uri: &str,
    state: &str,
    verifier: &str,
    scope: &str,
) -> Result<String, McpError> {
    let mut endpoint = Url::parse(&metadata.authorization_endpoint)
        .map_err(|err| McpError::Config(format!("parse authorization endpoint: {err}")))?;
    let challenge = URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()));
    {
        let mut query = endpoint.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &registration.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &metadata.resource_endpoint);
        if !scope.trim().is_empty() {
            query.append_pair("scope", scope.trim());
        }
    }
    Ok(endpoint.to_string())
}

/// 授权码换令牌。
pub async fn exchange_oauth_code(
    token_endpoint: &str,
    resource: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
    registration: &OAuthClientRegistration,
    policy: &EndpointPolicy,
) -> Result<OAuthTokenResponse, McpError> {
    let values = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", registration.client_id.as_str()),
        ("code_verifier", verifier),
        ("resource", resource),
    ];
    request_oauth_token(token_endpoint, &values, registration, policy).await
}

/// 刷新令牌（`grant_type=refresh_token`）。
pub async fn refresh_oauth_token(
    token_endpoint: &str,
    resource: &str,
    refresh_token: &str,
    registration: &OAuthClientRegistration,
    policy: &EndpointPolicy,
) -> Result<OAuthTokenResponse, McpError> {
    let values = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", registration.client_id.as_str()),
        ("resource", resource),
    ];
    request_oauth_token(token_endpoint, &values, registration, policy).await
}

/// 两种 grant 共用的 token 请求。
async fn request_oauth_token(
    raw_endpoint: &str,
    values: &[(&str, &str)],
    registration: &OAuthClientRegistration,
    policy: &EndpointPolicy,
) -> Result<OAuthTokenResponse, McpError> {
    let action = "request OAuth token";
    let endpoint = validate_oauth_url(raw_endpoint, policy).await?;

    let mut request = secure_client(&endpoint, policy)?
        .post(endpoint.as_str())
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .body(token_form(values, registration));
    if client_secret_auth(registration) == ClientSecretAuth::Basic {
        request = request.basic_auth(&registration.client_id, Some(&registration.client_secret));
    }

    let response: TokenResponse = oauth_json(request, action)
        .await
        .map_err(|err| context(err, action))?;
    if response.access_token.is_empty() || !response.token_type.eq_ignore_ascii_case("bearer") {
        return Err(McpError::Config(
            "token endpoint did not return a Bearer access token".into(),
        ));
    }
    Ok(OAuthTokenResponse {
        access_token: response.access_token,
        // 规范化成 `Bearer`（上游同款）。
        token_type: "Bearer".to_string(),
        expires_in: parse_expires_in(response.expires_in.as_ref()),
        refresh_token: response.refresh_token,
        scope: response.scope,
    })
}

/// client secret 的出场方式：`client_secret_post` / `client_secret_basic`（RFC 6749 §2.3.1），
/// 其余（含空 secret、`none`、未知方法）一律不带 secret。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientSecretAuth {
    None,
    Post,
    Basic,
}

fn client_secret_auth(registration: &OAuthClientRegistration) -> ClientSecretAuth {
    if registration.client_secret.is_empty() {
        return ClientSecretAuth::None;
    }
    match registration.token_endpoint_auth_method.as_str() {
        "client_secret_post" => ClientSecretAuth::Post,
        "client_secret_basic" => ClientSecretAuth::Basic,
        _ => ClientSecretAuth::None,
    }
}

/// 表单体（`application/x-www-form-urlencoded`）：grant 参数，外加 post 方式时的 `client_secret`。
#[must_use]
fn token_form(values: &[(&str, &str)], registration: &OAuthClientRegistration) -> String {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in values {
        form.append_pair(key, value);
    }
    if client_secret_auth(registration) == ClientSecretAuth::Post {
        form.append_pair("client_secret", &registration.client_secret);
    }
    form.finish()
}

/// `expires_in` 可能是数字也可能是字符串（RFC 6749 只要求它是「秒数」）。
fn parse_expires_in(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0),
        Some(Value::String(text)) => leading_integer(text),
        _ => 0,
    }
}

/// `fmt.Sscan` 的等价物：取开头的整数字面量，取不到就 0。
fn leading_integer(text: &str) -> i64 {
    let text = text.trim_start();
    let end = text
        .find(|c: char| !c.is_ascii_digit() && c != '-' && c != '+')
        .unwrap_or(text.len());
    text[..end].parse().unwrap_or(0)
}

/// 过期时刻：`expires_in <= 0` ⇒ 不设过期（`None`），与上游零值 `time.Time` 同义。
///
/// 数值大到溢出 `SystemTime` 时同样按「不设过期」处理 —— 上游会得到一个远未来的时刻，
/// 效果一样（永不刷新）。
#[must_use]
pub fn oauth_expiry(now: SystemTime, expires_in: i64) -> Option<SystemTime> {
    if expires_in <= 0 {
        return None;
    }
    let seconds = u64::try_from(expires_in).ok()?;
    now.checked_add(Duration::from_secs(seconds))
}

/// 给底层错误加「这一步」的上下文（上游 `fmt.Errorf("...: %w", err)`）。
fn context(err: McpError, action: &str) -> McpError {
    err.context(action)
}

#[cfg(test)]
mod tests;
