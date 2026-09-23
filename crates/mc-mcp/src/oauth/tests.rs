//! `oauth.rs` 的回归。
//!
//! 两类：**纯函数**（发现链的判据、PKCE、表单编码、过期时刻）与**传输**（走
//! `client::tests` 那份裸 HTTP 夹具，验 2xx/非 2xx/超限/重定向四种收尾）。
//!
//! 发现链的端到端跑不起来（本片不能加 `tokio-rustls` 之类的 TLS 测试依赖，而 OAuth 的
//! 每条 URL 都强制 `https`）—— 所以把**判据**全部下沉进 `merge_metadata` /
//! `validate_oauth_url` 的纯/半纯层，网络层只留 `get_oauth_json` / `oauth_json` 这一层。

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use url::Url;

use super::{
    authorization_metadata_urls, build_authorization_url, client_secret_auth, leading_integer,
    merge_metadata, oauth_expiry, parse_expires_in, protected_resource_metadata_url,
    register_oauth_client, resource_metadata_parameter, token_form, validate_oauth_metadata,
    AuthorizationServerMetadata, ClientSecretAuth, OAuthClientRegistration, OAuthMetadata,
    ProtectedResourceMetadata, OAUTH_URL_POLICY_ERROR,
};
use crate::client::tests::{spawn_fixture, FixtureResponse};
use crate::devorigin::EndpointPolicy;

/// RFC 7636 附录 B 的样例 code verifier / challenge。
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn resource(resource: &str, servers: &[&str]) -> ProtectedResourceMetadata {
    ProtectedResourceMetadata {
        resource: resource.to_string(),
        authorization_servers: servers.iter().map(|s| (*s).to_string()).collect(),
        scopes_supported: vec!["mcp.read".to_string(), "mcp.write".to_string()],
    }
}

fn server(authorization: &str, token: &str) -> AuthorizationServerMetadata {
    AuthorizationServerMetadata {
        authorization_endpoint: authorization.to_string(),
        token_endpoint: token.to_string(),
        registration_endpoint: "https://as.example/register".to_string(),
        code_challenge_methods_supported: vec!["S256".to_string()],
        token_endpoint_auth_methods_supported: vec!["none".to_string()],
    }
}

fn endpoint() -> Url {
    Url::parse("https://mcp.example/mcp").expect("endpoint")
}

fn metadata() -> OAuthMetadata {
    OAuthMetadata {
        resource_endpoint: "https://mcp.example/mcp".to_string(),
        authorization_endpoint: "https://as.example/authorize".to_string(),
        token_endpoint: "https://as.example/token".to_string(),
        registration_endpoint: Some("https://as.example/register".to_string()),
        scopes: vec!["mcp.read".to_string()],
        token_auth_methods: vec!["none".to_string()],
    }
}

// ---------------------------------------------------------------- 探测头

#[test]
fn resource_metadata_parameter_mirrors_the_upstream_regex() {
    // `Bearer` 之后的空格与 `,` 都算分隔符（`[, ]+`）。
    assert_eq!(
        resource_metadata_parameter(r#"Bearer resource_metadata="https://as.example/rm""#),
        Some("https://as.example/rm")
    );
    assert_eq!(
        resource_metadata_parameter(r#"Bearer, resource_metadata="https://as.example/rm""#),
        Some("https://as.example/rm")
    );
    // 大小写不敏感（`(?i)`）。
    assert_eq!(
        resource_metadata_parameter(r#"RESOURCE_METADATA="https://as.example/rm""#),
        Some("https://as.example/rm")
    );
    // 前一个字符必须是串首 / `,` / 空格 —— `xresource_metadata=` 不是这个参数。
    assert_eq!(
        resource_metadata_parameter(r#"xresource_metadata="https://evil.example""#),
        None
    );
    // 空值与未闭合都不算命中。
    assert_eq!(resource_metadata_parameter(r#"resource_metadata="""#), None);
    assert_eq!(
        resource_metadata_parameter(r#"resource_metadata="unterminated"#),
        None
    );
    assert_eq!(
        resource_metadata_parameter("Bearer error=invalid_token"),
        None
    );
    // 第一个（坏）候选不能吃掉后面那个好候选。
    assert_eq!(
        resource_metadata_parameter(r#"resource_metadata="" , resource_metadata="https://ok""#),
        Some("https://ok")
    );
}

// ---------------------------------------------------------------- URL 形状

#[test]
fn well_known_urls_follow_rfc_8414_and_9728() {
    let cases = [
        (
            "https://mcp.example/mcp",
            "https://mcp.example/.well-known/oauth-protected-resource/mcp",
        ),
        (
            "https://mcp.example/",
            "https://mcp.example/.well-known/oauth-protected-resource",
        ),
        (
            "https://mcp.example",
            "https://mcp.example/.well-known/oauth-protected-resource",
        ),
        (
            "https://mcp.example/a/b/",
            "https://mcp.example/.well-known/oauth-protected-resource/a/b",
        ),
        (
            "https://mcp.example/mcp?x=1#frag",
            "https://mcp.example/.well-known/oauth-protected-resource/mcp",
        ),
    ];
    for (input, want) in cases {
        let parsed = Url::parse(input).expect(input);
        assert_eq!(protected_resource_metadata_url(&parsed), want, "{input}");
    }
}

#[test]
fn authorization_metadata_urls_try_rfc_8414_before_oidc_fallback() {
    let issuer = Url::parse("https://as.example/tenant/").expect("issuer");
    assert_eq!(
        authorization_metadata_urls(&issuer),
        vec![
            "https://as.example/.well-known/oauth-authorization-server/tenant".to_string(),
            "https://as.example/.well-known/openid-configuration/tenant".to_string(),
        ]
    );
}

// ---------------------------------------------------------------- 发现链判据

#[test]
fn merge_metadata_pins_the_rules_of_the_discovery_chain() {
    let merged = merge_metadata(
        &endpoint(),
        &resource("https://mcp.example/mcp", &["https://as.example"]),
        &server("https://as.example/authorize", "https://as.example/token"),
    )
    .expect("happy path");
    assert_eq!(merged.resource_endpoint, "https://mcp.example/mcp");
    assert_eq!(
        merged.authorization_endpoint,
        "https://as.example/authorize"
    );
    assert_eq!(merged.token_endpoint, "https://as.example/token");
    assert_eq!(
        merged.registration_endpoint.as_deref(),
        Some("https://as.example/register")
    );
    assert_eq!(merged.scopes, vec!["mcp.read", "mcp.write"]);
    assert_eq!(merged.token_auth_methods, vec!["none"]);

    // 资源文档声明的 `resource` 必须就是本 endpoint（防「拿别人的元数据换掉 endpoint」）。
    let mismatch = merge_metadata(
        &endpoint(),
        &resource("https://other.example/mcp", &["https://as.example"]),
        &server("https://as.example/authorize", "https://as.example/token"),
    );
    assert!(
        matches!(mismatch, Err(crate::client::McpError::Config(message))
        if message == "protected resource metadata does not match the MCP endpoint")
    );
    // 空 `resource` 字段不算不匹配（上游 `if metadata.Resource != "" && …`）。
    assert!(merge_metadata(
        &endpoint(),
        &resource("", &["https://as.example"]),
        &server("https://as.example/authorize", "https://as.example/token"),
    )
    .is_ok());

    for (authorization, token) in [
        ("", "https://as.example/token"),
        ("https://as.example/authorize", ""),
    ] {
        let missing = merge_metadata(
            &endpoint(),
            &resource("", &[]),
            &server(authorization, token),
        );
        assert!(
            matches!(missing, Err(crate::client::McpError::Config(message))
            if message == "authorization server metadata is missing required endpoints")
        );
    }

    // 对面若声明了支持的 PKCE 方法，必须含 S256；没说（空）则放行。
    let mut plain = server("https://as.example/authorize", "https://as.example/token");
    plain.code_challenge_methods_supported = vec!["plain".to_string()];
    let unsupported = merge_metadata(&endpoint(), &resource("", &[]), &plain);
    assert!(
        matches!(unsupported, Err(crate::client::McpError::Config(message))
        if message == "authorization server does not support PKCE S256")
    );
    plain.code_challenge_methods_supported = Vec::new();
    assert!(merge_metadata(&endpoint(), &resource("", &[]), &plain).is_ok());
}

#[tokio::test]
async fn validate_oauth_metadata_rejects_missing_or_unsafe_endpoints() {
    let policy = EndpointPolicy::default();

    let mut missing = metadata();
    missing.token_endpoint = String::new();
    assert!(matches!(
        validate_oauth_metadata(&missing, &policy).await,
        Err(crate::client::McpError::Config(message))
            if message == "OAuth metadata is missing required endpoints"
    ));

    // 形状预检：非 https 直接拒（且发生在解析 DNS **之前**，所以这条断言不出网）。
    let mut insecure = metadata();
    insecure.authorization_endpoint = "http://as.example/authorize".to_string();
    insecure.token_endpoint = "http://as.example/token".to_string();
    assert!(matches!(
        validate_oauth_metadata(&insecure, &policy).await,
        Err(crate::client::McpError::Config(message)) if message == OAUTH_URL_POLICY_ERROR
    ));

    let mut userinfo = metadata();
    userinfo.authorization_endpoint = "https://user@as.example/authorize".to_string();
    assert!(matches!(
        validate_oauth_metadata(&userinfo, &policy).await,
        Err(crate::client::McpError::Config(_))
    ));
}

#[tokio::test]
async fn discover_oauth_rejects_a_bad_endpoint_before_any_request() {
    let err = super::discover_oauth("not a url", &EndpointPolicy::default())
        .await
        .expect_err("must reject");
    assert!(
        matches!(err, crate::client::McpError::EndpointRejected(message)
        if message.starts_with("parse endpoint:"))
    );
}

// ---------------------------------------------------------------- 授权 URL / PKCE

#[test]
fn build_authorization_url_uses_the_rfc_7636_vector_and_keeps_existing_query() {
    let mut metadata = metadata();
    metadata.authorization_endpoint = "https://as.example/authorize?tenant=t1".to_string();
    let registration = OAuthClientRegistration {
        client_id: "client-1".to_string(),
        client_secret: String::new(),
        token_endpoint_auth_method: "none".to_string(),
    };
    let url = build_authorization_url(
        &metadata,
        &registration,
        "https://app.example/oauth/callback",
        "state-1",
        PKCE_VERIFIER,
        "  mcp.read mcp.write  ",
    )
    .expect("build");
    let parsed = Url::parse(&url).expect("parse");
    let pairs: BTreeMap<String, String> = parsed.query_pairs().into_owned().collect();
    assert_eq!(pairs.get("tenant").map(String::as_str), Some("t1"));
    assert_eq!(pairs.get("response_type").map(String::as_str), Some("code"));
    assert_eq!(pairs.get("client_id").map(String::as_str), Some("client-1"));
    assert_eq!(
        pairs.get("redirect_uri").map(String::as_str),
        Some("https://app.example/oauth/callback")
    );
    assert_eq!(pairs.get("state").map(String::as_str), Some("state-1"));
    // PKCE S256 = base64url(sha256(verifier))，用 RFC 7636 附录 B 的向量钉住。
    assert_eq!(
        pairs.get("code_challenge").map(String::as_str),
        Some(PKCE_CHALLENGE)
    );
    assert_eq!(
        pairs.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        pairs.get("resource").map(String::as_str),
        Some("https://mcp.example/mcp")
    );
    assert_eq!(
        pairs.get("scope").map(String::as_str),
        Some("mcp.read mcp.write"),
        "scope 两侧空白会被裁掉"
    );

    // 空 scope ⇒ 不带该参数（上游 `if scope != ""`）。
    let without_scope = build_authorization_url(
        &metadata,
        &registration,
        "https://app.example/oauth/callback",
        "state-1",
        PKCE_VERIFIER,
        "   ",
    )
    .expect("build");
    assert!(!without_scope.contains("scope="));

    // 相对 URL 拼不出来（上游 `url.Parse` 报错）。
    let mut relative = metadata;
    relative.authorization_endpoint = "/authorize".to_string();
    assert!(build_authorization_url(
        &relative,
        &registration,
        "https://app.example/oauth/callback",
        "s",
        PKCE_VERIFIER,
        ""
    )
    .is_err());
}

// ---------------------------------------------------------------- token 请求

#[test]
fn client_secret_placement_follows_rfc_6749() {
    let cases = [
        ("", "client_secret_post", ClientSecretAuth::None),
        ("s3cr3t", "client_secret_post", ClientSecretAuth::Post),
        ("s3cr3t", "client_secret_basic", ClientSecretAuth::Basic),
        ("s3cr3t", "none", ClientSecretAuth::None),
        ("s3cr3t", "private_key_jwt", ClientSecretAuth::None),
    ];
    for (secret, method, want) in cases {
        let registration = OAuthClientRegistration {
            client_id: "client-1".to_string(),
            client_secret: secret.to_string(),
            token_endpoint_auth_method: method.to_string(),
        };
        assert_eq!(client_secret_auth(&registration), want, "{method}/{secret}");
    }
}

#[test]
fn token_form_is_url_encoded_and_carries_the_secret_only_for_post() {
    let values = [("grant_type", "authorization_code"), ("code", "a b+c/1")];
    let post = OAuthClientRegistration {
        client_id: "client-1".to_string(),
        client_secret: "s3cr3t".to_string(),
        token_endpoint_auth_method: "client_secret_post".to_string(),
    };
    let encoded = token_form(&values, &post);
    let pairs: BTreeMap<String, String> = url::form_urlencoded::parse(encoded.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(
        pairs.get("grant_type").map(String::as_str),
        Some("authorization_code")
    );
    assert_eq!(pairs.get("code").map(String::as_str), Some("a b+c/1"));
    assert_eq!(
        pairs.get("client_secret").map(String::as_str),
        Some("s3cr3t")
    );

    let basic = OAuthClientRegistration {
        token_endpoint_auth_method: "client_secret_basic".to_string(),
        ..post.clone()
    };
    assert!(!token_form(&values, &basic).contains("client_secret="));
    let none = OAuthClientRegistration {
        client_secret: String::new(),
        token_endpoint_auth_method: "client_secret_post".to_string(),
        ..post
    };
    assert!(!token_form(&values, &none).contains("client_secret="));
}

#[test]
fn register_oauth_client_needs_a_registration_endpoint() {
    let mut metadata = metadata();
    metadata.registration_endpoint = None;
    let err = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(register_oauth_client(
            &metadata,
            "https://app.example/oauth/callback",
            &EndpointPolicy::default(),
        ))
        .expect_err("must reject");
    assert!(matches!(err, crate::client::McpError::Config(message)
        if message == "authorization server requires a pre-registered OAuth client"));
}

// ---------------------------------------------------------------- expires_in / 过期

#[test]
fn expires_in_accepts_numbers_and_strings_like_the_go_decoder() {
    assert_eq!(parse_expires_in(Some(&json!(3600))), 3600);
    assert_eq!(parse_expires_in(Some(&json!("3600"))), 3600);
    assert_eq!(parse_expires_in(Some(&json!("3600s"))), 3600);
    assert_eq!(parse_expires_in(Some(&json!(null))), 0);
    assert_eq!(parse_expires_in(None), 0);
    // Go 把 `3600.0` 解进 int64 会失败 ⇒ 落到 0（本仓同样）。
    assert_eq!(parse_expires_in(Some(&json!(3600.0))), 0);
    assert_eq!(leading_integer(" 42 "), 42);
    assert_eq!(leading_integer("-3"), -3);
    assert_eq!(leading_integer("abc"), 0);
}

#[test]
fn oauth_expiry_matches_the_go_zero_time_semantics() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert_eq!(oauth_expiry(now, 0), None);
    assert_eq!(oauth_expiry(now, -5), None);
    assert_eq!(
        oauth_expiry(now, 3600),
        Some(now + Duration::from_secs(3600))
    );
    // 大到溢出 ⇒ 按「不设过期」处理（上游会得到远未来时刻，效果相同）。
    assert_eq!(oauth_expiry(now, i64::MAX), None);
}

// ---------------------------------------------------------------- 日志遮蔽

#[test]
fn debug_never_prints_secrets_or_tokens() {
    let registration = OAuthClientRegistration {
        client_id: "client-1".to_string(),
        client_secret: "s3cr3t-value".to_string(),
        token_endpoint_auth_method: "client_secret_basic".to_string(),
    };
    let printed = format!("{registration:?}");
    assert!(printed.contains("client-1"));
    assert!(printed.contains("<redacted>"));
    assert!(!printed.contains("s3cr3t-value"));

    let token = super::OAuthTokenResponse {
        access_token: "at-value".to_string(),
        token_type: "Bearer".to_string(),
        expires_in: 3600,
        refresh_token: "rt-value".to_string(),
        scope: "mcp.read".to_string(),
    };
    let printed = format!("{token:?}");
    assert!(printed.contains("Bearer") && printed.contains("3600"));
    assert!(!printed.contains("at-value") && !printed.contains("rt-value"));
    // `expires_in <= 0` / 无 refresh token 时也不泄漏（形状不同，值同样遮蔽）。
    let no_refresh = super::OAuthTokenResponse {
        access_token: "at-value".to_string(),
        refresh_token: String::new(),
        ..token
    };
    assert!(!format!("{no_refresh:?}").contains("at-value"));
}

// ---------------------------------------------------------------- 传输层

// 夹具每接受**一条连接**就把它上面的响应按序发完 ⇒ 多次调用必须共用同一个 client
// （`get_oauth_json` 每次新建 client，只适合「一次调用一个夹具」）。
fn oauth_get(client: &reqwest::Client, url: &Url) -> reqwest::RequestBuilder {
    client
        .get(url.as_str())
        .header(reqwest::header::ACCEPT, "application/json")
}

#[tokio::test]
async fn oauth_json_accepts_2xx_and_reports_non_2xx_with_the_status() {
    let fixture = spawn_fixture(vec![
        FixtureResponse::json(r#"{"ok": true}"#),
        FixtureResponse::status(500, r#"{"error":"boom"}"#),
    ])
    .await;
    let url = Url::parse(&fixture.endpoint()).expect("url");
    let policy = fixture.dev_policy();
    let client = crate::client::secure_client(&url, &policy).expect("client");

    let loaded: Value = super::oauth_json(oauth_get(&client, &url), "load metadata")
        .await
        .expect("2xx");
    assert_eq!(loaded["ok"], json!(true));

    let err = super::oauth_json::<Value>(oauth_get(&client, &url), "load metadata")
        .await
        .expect_err("non-2xx");
    assert!(
        matches!(&err, crate::client::McpError::Protocol(message)
            if message == "load metadata: HTTP 500"),
        "对面状态码进错误，错误页正文不进：{err:?}"
    );
}

#[tokio::test]
async fn oauth_json_fails_closed_on_an_oversized_body() {
    let oversize = "x".repeat(super::MAX_OAUTH_RESPONSE_BYTES + 1);
    let fixture = spawn_fixture(vec![FixtureResponse::status(200, oversize)]).await;
    let url = Url::parse(&fixture.endpoint()).expect("url");

    let err = super::get_oauth_json::<Value>(&url, &fixture.dev_policy(), "load metadata")
        .await
        .expect_err("oversize");
    assert_eq!(err, crate::client::McpError::ResponseTooLarge);
}

#[tokio::test]
async fn oauth_json_does_not_follow_redirects() {
    let fixture = spawn_fixture(vec![
        FixtureResponse::status(302, "").with_header("Location", "https://evil.example/meta")
    ])
    .await;
    let url = Url::parse(&fixture.endpoint()).expect("url");

    let err = super::get_oauth_json::<Value>(&url, &fixture.dev_policy(), "load metadata")
        .await
        .expect_err("redirect");
    assert!(
        matches!(&err, crate::client::McpError::Transport(message)
            if message.contains("redirects are not allowed")),
        "重定向必须变成传输错误：{err:?}"
    );
    // 断言的是**没有**追随：夹具只写过一条响应，追随了就会读到空/超时而不是这条错误。
    assert_eq!(fixture.requests().len(), 1);
}
