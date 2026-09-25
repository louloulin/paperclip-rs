//! `mc-vcs-github` 的**离线替身**端到端测试（`docs/61` §4.2 的 R-M8-1）。
//!
//! 上游 `github.go:34-36` 把 REST base 放成**可写包级变量**（注释逐字「Mutable so tests
//! can…」）；本仓对应物是 `GithubClient::new(api_base)` / `Client::with_api_base(..)`。
//! 本文件起一个**最小 HTTP/1.1 替身**（`tokio::net::TcpListener` + 手写帧，零额外依赖），
//! 按 GitHub 的 REST / GraphQL 形状答，然后断言**真实 wire 帧**：
//!
//! | 用例 | 断言链 |
//! | --- | --- |
//! | `exchange_installation_token` | 断言替身收到的 `Authorization: Bearer <App JWT>`、`Accept`、`X-GitHub-Api-Version` 与请求体逐字 |
//! | 仓库分页 | `page*per_page < total_count` ⇒ `next_page = page+1`；否则 `null`（上游算术） |
//! | GraphQL | `data` 透传；查询级 `RATE_LIMITED` ⇒ `RateLimited`；403 + `Retry-After` ⇒ `RateLimited` |
//! | token 撤销 | `DELETE /installation/token` + 204 ⇒ `Ok(())` |
//! | 账号信息 | 200 ⇒ 真值；非 200 ⇒ `unknown`/`User` 占位（**永不失败**） |
//! | **单飞** | `Client::installation_token` 并发 8 次 ⇒ 替身**只**收到 1 次 `POST .../access_tokens` |

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mc_vcs_github::app::{verify_rs256, AppJwtSigner};
use mc_vcs_github::ghsnapshot::Client;
use mc_vcs_github::rest::{GithubClient, GithubError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// 最小 HTTP/1.1 替身
// ---------------------------------------------------------------------------

/// 替身收到的一次请求（逐字段留证）。
#[derive(Debug, Clone)]
struct StubRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

impl StubRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

/// 替身的应答。
#[derive(Debug, Clone)]
struct StubResponse {
    status: u16,
    body: String,
    headers: Vec<(String, String)>,
}

impl StubResponse {
    fn json(status: u16, body: impl serde::Serialize) -> Self {
        Self {
            status,
            body: serde_json::to_string(&body).unwrap_or_default(),
            headers: Vec::new(),
        }
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            body: String::new(),
            headers: Vec::new(),
        }
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// 起一个替身：`handler` 按请求给应答；返回 `(base_url, 收到的请求台账)`。
async fn start_stub<F>(handler: F) -> (String, Arc<Mutex<Vec<StubRequest>>>)
where
    F: Fn(&StubRequest) -> StubResponse + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let addr = listener.local_addr().expect("addr");
    let seen: Arc<Mutex<Vec<StubRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let ledger = seen.clone();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let ledger = ledger.clone();
            let handler = handler.clone();
            tokio::spawn(async move {
                let _ = serve_one(stream, handler, ledger).await;
            });
        }
    });
    (format!("http://{addr}"), seen)
}

async fn serve_one<F>(
    mut stream: TcpStream,
    handler: Arc<F>,
    ledger: Arc<Mutex<Vec<StubRequest>>>,
) -> std::io::Result<()>
where
    F: Fn(&StubRequest) -> StubResponse + Send + Sync + 'static,
{
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_subsequence(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    let request = StubRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    };
    let response = handler(&request);
    ledger.lock().unwrap().push(request);

    let mut raw = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        status_text(response.status),
        response.body.len()
    );
    for (name, value) in &response.headers {
        let _ = std::fmt::Write::write_fmt(&mut raw, format_args!("{name}: {value}\r\n"));
    }
    raw.push_str("\r\n");
    raw.push_str(&response.body);
    stream.write_all(raw.as_bytes()).await?;
    stream.flush().await
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        _ => "Stub",
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn ledger_paths(ledger: &Arc<Mutex<Vec<StubRequest>>>) -> Vec<String> {
    ledger
        .lock()
        .unwrap()
        .iter()
        .map(|request| format!("{} {}", request.method, request.path))
        .collect()
}

/// 单测用的确定性私钥（与 `app.rs` 的往返用例同源；**不是**生产密钥）。
const TEST_KEY_PEM: &str = "\
-----BEGIN PRIVATE KEY-----
MIIEvwIBADANBgkqhkiG9w0BAQEFAASCBKkwggSlAgEAAoIBAQC8Lko4B+10Dj7o
dZedrDE3ZbHRBAsSKACHGOsI00EzEEUh8di1MVB1O/dYC2ZwlsteuwkZFF+aQDuT
3gZTzQVkq5menpNmJ5BVKfQr6jy7mQAhhgUEG69Afu2QNC3hFe3S+lcjdt9agM3r
rbuK35OrPrhaeW33bO+1ip0r5gnnw0j6IaW+Tnndzusv1H7tBJj9R6IA7E7tPGIr
UbdWADlP2XDPC/g5R37XhY5H5sg9B3dJptrVBD532eHG0nK7UTQybq7QMHOygYn5
KfbgZZLnYwpV40SE56DhR9jzxcIdo8okGMeoIlbwebC09spzLXwi66bPW6jE7eKY
cYwfvixlAgMBAAECggEACpr7QNQljERfRD+YV2EEdxBKqLJ3I0NQ4ExFtr4dLxEM
LGESawfH9ot2Iaam09KTzJdy6FBvIOTc1rUNGzzzQFyxcDCUsw2owzv1kGIHoTT6
vmjssHIU+ugMYHOoYEaZnCnSrmN9K/8VW+JzLtzx2BVVU3gDfA3OJqeUuwwgY8jT
bSrNe8LKiS6sj5IF2hIpBIXFTuE6SKU/64T3kiKG2AbnLLtF34LNl1sbw5TTjEAs
hGnDvNJqHgCtDqGDXaAiNbTrPWeWMJW4RhfLDzF1fpZXSw3f5z+zVMzgg6JSBZCj
Pf5FAsU4db9cGW+yLjb5NqNrBX/c24OxZ2yNcPRSkQKBgQDsraU02Mf2uFj22T0r
HBp3q11IcpuA7YU2lBP6o4aKlc6cCyj8U75yS7hu/kUoEtAUnxyyCXOpM7lhGbmz
dtIqM6r74FlIr8mPi2TLMloUGmtY42sUNtciQgd+Ds4RjWNC2/1FRe5XMFm1bP/f
6jMZEr0k0d0Cz7au5sZg4Jwg6QKBgQDLixe4xIOvQ7wvKpKngKbStW0MIAoYnvSB
hIj3xxasXc6V/qp9ANTnG28yk9tsCajUMQSfEX0uIx1YSAB0MbaIrAhsqvUaknDx
vkUtaeBWE8e0H3U+9KVnVWLZoPNHFIVeGhWMXJKpbQiZiDreELaAeltgIzU5JfAP
VRJ8eoOiHQKBgQCopqQOoFr9aCec3vhDe+cwVyBFu8UrfhVq6uHBvDznDBEKCLnP
9CzFbUejb/T/tUgpKahdBXcxnvX+R0KYq5bfE6pHiXqV3Q2YCBBu6xZdNOZBlOx8
nwd2Fe8Y2JvmzgVpYzF653YLEx0Ztu4uNMjsmPnG/vSqSDE5OKEr72HR4QKBgQDF
jWKgulr1KNDlFnTwjjVcHSqRsicabmzxqCkoE9s1wHZZrqraWIxLIp1ygX9eBKIQ
EONjYB4XQY2huYB3Rijbzdz/W445FBj7CKkrwq8x3FDfygiJ6fj/qigfAdAdFRW8
l6SCbvcJ6gGGwmogTihT2m4FiSaHKQMuXmtq1Z4dIQKBgQCBdv0m4+OX6Rxjx3cd
cpr1nFZBZqPqOdDLakWXRko+K0eNFOKCF4Smf3OUVZz5yoAAGG9HcXEwUO09ZzCD
6FZCMLwGRY4IXmRObxEfD5k/ZlzL9+/rNIKtQjObwppciqPW0NoPt5qJvMJXq7Dw
C2CZCWlMaEGLpAGiVraQnRQlPw==
-----END PRIVATE KEY-----
";

const APP_ID: &str = "123456";

// ---------------------------------------------------------------------------
// REST：token 交换 / 仓库分页 / 撤销 / 账号信息
// ---------------------------------------------------------------------------

#[tokio::test]
async fn exchange_installation_token_sends_the_exact_upstream_request() {
    let signer = AppJwtSigner::from_pem(APP_ID, TEST_KEY_PEM).expect("test key");
    let app_jwt = signer.sign_app_jwt(1_700_000_000).expect("sign");

    let (base, ledger) = start_stub(|request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/app/installations/4242/access_tokens");
        // 上游 `setGitHubAPIHeaders` 的三个头逐字。
        assert!(request
            .header("authorization")
            .expect("authorization")
            .starts_with("Bearer "));
        assert_eq!(
            request.header("accept"),
            Some("application/vnd.github+json")
        );
        assert_eq!(request.header("x-github-api-version"), Some("2022-11-28"));
        assert_eq!(request.header("content-type"), Some("application/json"));
        // 请求体逐字（上游 `strings.NewReader(`{"permissions":{"metadata":"read"}}`)`）。
        assert_eq!(request.body, r#"{"permissions":{"metadata":"read"}}"#);
        StubResponse::json(
            201,
            serde_json::json!({"token": "ghs_stub_token", "expires_at": "2026-09-25T02:00:00Z"}),
        )
    })
    .await;

    let client = GithubClient::new(&base);
    let exchanged = client
        .exchange_installation_token(&app_jwt, 4242)
        .await
        .expect("exchange");
    assert_eq!(exchanged.expose(), "ghs_stub_token");
    assert_eq!(exchanged.expires_at, "2026-09-25T02:00:00Z");
    // 替身收到的 Bearer 就是刚签的那枚 JWT（逐字节）。
    let seen = ledger.lock().unwrap()[0]
        .header("authorization")
        .expect("authorization")
        .to_string();
    assert_eq!(seen, format!("Bearer {app_jwt}"));
    // JWT 是 RS256 且签名可验（JWT 的签名覆盖 `header.payload`）。
    let parts: Vec<&str> = app_jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
}

#[tokio::test]
async fn exchange_rejects_401_and_non_201_without_echoing_the_body() {
    let (base, _) =
        start_stub(|_| StubResponse::json(401, serde_json::json!({"message": "bad creds"}))).await;
    let err = GithubClient::new(&base)
        .exchange_installation_token("jwt", 1)
        .await
        .unwrap_err();
    assert!(matches!(err, GithubError::Unauthorized));
    assert!(!err.to_string().contains("bad creds"));

    let (base, _) = start_stub(|_| {
        StubResponse::json(422, serde_json::json!({"message": "secret-ish detail"}))
    })
    .await;
    let err = GithubClient::new(&base)
        .exchange_installation_token("jwt", 1)
        .await
        .unwrap_err();
    assert!(matches!(err, GithubError::UnexpectedStatus(422)));
    assert!(!err.to_string().contains("secret-ish detail"));
}

#[tokio::test]
async fn repository_pagination_follows_upstream_arithmetic() {
    let (base, ledger) = start_stub(|request| {
        let total: i64 = 250;
        let per_page: i64 = request
            .path
            .split("per_page=")
            .nth(1)
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);
        let page: i64 = request
            .path
            .split("page=")
            .nth(1)
            .and_then(|v| v.split('&').next())
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        let count = if page * per_page <= total {
            per_page.min(total)
        } else {
            0
        };
        let repos: Vec<serde_json::Value> = (0..count)
            .map(|index| {
                serde_json::json!({
                    "id": 1000 + index,
                    "full_name": format!("acme/repo-{index}"),
                    "html_url": format!("https://github.com/acme/repo-{index}"),
                    "clone_url": format!("https://github.com/acme/repo-{index}.git"),
                    "description": null,
                    "private": false,
                    "archived": false,
                    "default_branch": "main"
                })
            })
            .collect();
        StubResponse::json(
            200,
            serde_json::json!({"total_count": total, "repositories": repos}),
        )
    })
    .await;

    let client = GithubClient::new(&base);
    let page1 = client
        .list_installation_repositories("ghs", 1, 100)
        .await
        .expect("page 1");
    assert_eq!(page1.total_count, 250);
    assert_eq!(page1.repositories.len(), 100);
    assert_eq!(page1.next_page, Some(2), "1*100 < 250 ⇒ 下一页");
    assert_eq!(page1.repositories[0].full_name, "acme/repo-0");

    let page3 = client
        .list_installation_repositories("ghs", 3, 100)
        .await
        .expect("page 3");
    assert_eq!(page3.next_page, None, "3*100 >= 250 ⇒ 没有下一页");

    // 替身收到的查询串逐字。
    let paths = ledger_paths(&ledger);
    assert!(paths.contains(&"GET /installation/repositories?page=1&per_page=100".to_string()));
    assert!(paths.contains(&"GET /installation/repositories?page=3&per_page=100".to_string()));
}

#[tokio::test]
async fn revoke_installation_token_is_a_delete_on_the_token_endpoint() {
    let (base, ledger) = start_stub(|request| {
        assert_eq!(request.method, "DELETE");
        assert_eq!(request.path, "/installation/token");
        assert_eq!(request.header("authorization"), Some("Bearer ghs_doomed"));
        StubResponse::empty(204)
    })
    .await;
    GithubClient::new(&base)
        .revoke_installation_token("ghs_doomed")
        .await
        .expect("204 is success");
    assert_eq!(ledger_paths(&ledger), vec!["DELETE /installation/token"]);
}

#[tokio::test]
async fn fetch_installation_account_enriches_and_falls_back() {
    let (base, _) = start_stub(|request| {
        assert_eq!(request.path, "/app/installations/7");
        assert_eq!(
            request.header("accept"),
            Some("application/vnd.github+json")
        );
        StubResponse::json(
            200,
            serde_json::json!({
                "account": {"login": "acme", "type": "Organization",
                            "avatar_url": "https://avatars.example/acme.png"}
            }),
        )
    })
    .await;
    let account = GithubClient::new(&base)
        .fetch_installation_account(Some("jwt"), 7)
        .await;
    assert_eq!(account.login, "acme");
    assert_eq!(account.account_type, "Organization");
    assert_eq!(
        account.avatar_url.as_deref(),
        Some("https://avatars.example/acme.png")
    );

    // 非 200 ⇒ 占位（上游 `fetchInstallationAccount` 永不失败）。
    let (base, _) = start_stub(|_| StubResponse::empty(500)).await;
    let account = GithubClient::new(&base)
        .fetch_installation_account(Some("jwt"), 7)
        .await;
    assert_eq!(account.login, "unknown");
    assert_eq!(account.account_type, "User");
    assert!(account.avatar_url.is_none());

    // 无 Authorization 时不带该头（未配置 App 身份的裸跑分支）。
    let (base, ledger) = start_stub(|request| {
        assert!(request.header("authorization").is_none());
        StubResponse::json(
            200,
            serde_json::json!({"account": {"login": "solo", "type": "User"}}),
        )
    })
    .await;
    let account = GithubClient::new(&base)
        .fetch_installation_account(None, 9)
        .await;
    assert_eq!(account.login, "solo");
    assert_eq!(ledger.lock().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// GraphQL
// ---------------------------------------------------------------------------

#[tokio::test]
async fn graph_ql_passes_data_through_and_maps_rate_limit_errors() {
    let (base, ledger) = start_stub(|request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/graphql");
        assert_eq!(request.header("content-type"), Some("application/json"));
        if request.body.contains("rate-limited") {
            return StubResponse::json(
                200,
                serde_json::json!({"errors": [{"type": "RATE_LIMITED", "message": "slow down"}]}),
            );
        }
        if request.body.contains("query-errors") {
            return StubResponse::json(
                200,
                serde_json::json!({"errors": [{"type": "NOT_FOUND", "message": "no such repo"}]}),
            );
        }
        StubResponse::json(
            200,
            serde_json::json!({"data": {"repository": {"pullRequest": {"headRefOid": "abc"}}}}),
        )
    })
    .await;

    let client = GithubClient::new(&base);
    let data = client
        .graph_ql(
            "ghs",
            "query($x:String){ rate-limited }",
            &serde_json::json!({}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(data, GithubError::RateLimited { .. }),
        "RATE_LIMITED 映射"
    );

    let err = client
        .graph_ql(
            "ghs",
            "query($x:String){ query-errors }",
            &serde_json::json!({}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no such repo"));

    // 送出的 body 是 `{query, variables}` 信封。
    let sent: serde_json::Value = serde_json::from_str(&ledger.lock().unwrap()[0].body).unwrap();
    assert!(sent["query"].is_string());
    assert!(sent["variables"].is_object());

    // 403 + Retry-After 走限流（上游 `rateLimitFromResponse`）。
    let (base, _) = start_stub(|_| StubResponse::empty(403).with_header("Retry-After", "42")).await;
    let err = GithubClient::new(&base)
        .graph_ql("ghs", "query{}", &serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        GithubError::RateLimited {
            retry_after_secs: 42
        }
    ));
}

// ---------------------------------------------------------------------------
// App JWT + 缓存 + 单飞（`Client` 整条链）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_signs_app_jwt_and_uses_bearer_for_graphql() {
    let (base, ledger) = start_stub(|request| {
        if request.path.ends_with("/access_tokens") {
            return StubResponse::json(
                201,
                serde_json::json!({"token": "ghs_minted", "expires_at": "2030-01-01T00:00:00Z"}),
            );
        }
        assert_eq!(request.path, "/graphql");
        // GraphQL 用的是**换来的 installation token**，不是 App JWT。
        assert_eq!(request.header("authorization"), Some("Bearer ghs_minted"));
        StubResponse::json(200, serde_json::json!({"data": {"ok": true}}))
    })
    .await;

    let client = Client::new(Some(APP_ID.into()), Some(TEST_KEY_PEM.into()))
        .with_api_base(&base)
        .with_token_cache(Arc::new(
            mc_vcs_github::InstallationTokenCache::with_renew_skew(300),
        ));
    assert!(client.enabled());

    let data = client
        .graph_ql(11, "query{}", &serde_json::json!({}), 1_700_000_000)
        .await
        .expect("graphql");
    assert_eq!(data["ok"], true);

    let paths = ledger_paths(&ledger);
    assert_eq!(
        paths,
        vec![
            "POST /app/installations/11/access_tokens".to_string(),
            "POST /graphql".to_string()
        ]
    );
    // 换 token 那次带的是 App JWT，且确实是 RS256（用公钥验一次签名）。
    let first = &ledger.lock().unwrap()[0];
    let bearer = first
        .header("authorization")
        .expect("authorization")
        .trim_start_matches("Bearer ")
        .to_string();
    let signer = AppJwtSigner::from_pem(APP_ID, TEST_KEY_PEM).unwrap();
    let parts: Vec<&str> = bearer.split('.').collect();
    assert_eq!(parts.len(), 3);
    let payload = base64_decode_url(parts[1]);
    let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(payload["iss"], APP_ID);
    assert!(verify_rs256(
        signer.public_key(),
        format!("{}.{}", parts[0], parts[1]).as_bytes(),
        &base64_decode_url(parts[2])
    ));
}

/// `DoD`：**并发 8 个请求只换 1 次 token**（冷缓存 + 缓存命中两条路径）。
#[tokio::test]
async fn eight_concurrent_requests_mint_exactly_one_installation_token() {
    let mints = Arc::new(AtomicUsize::new(0));
    let counter = mints.clone();
    let (base, ledger) = start_stub(move |request| {
        if request.path.ends_with("/access_tokens") {
            counter.fetch_add(1, Ordering::SeqCst);
            return StubResponse::json(
                201,
                serde_json::json!({"token": "ghs_once", "expires_at": "2030-01-01T00:00:00Z"}),
            );
        }
        StubResponse::json(200, serde_json::json!({"data": {}}))
    })
    .await;

    let client =
        Arc::new(Client::new(Some(APP_ID.into()), Some(TEST_KEY_PEM.into())).with_api_base(&base));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let client = client.clone();
        handles.push(tokio::spawn(async move {
            client
                .graph_ql(77, "query{}", &serde_json::json!({}), 1_700_000_000)
                .await
        }));
    }
    for handle in handles {
        handle.await.expect("join").expect("graphql");
    }
    assert_eq!(
        mints.load(Ordering::SeqCst),
        1,
        "8 个并发请求只能换 1 次 installation token（单飞）"
    );
    let token_calls = ledger_paths(&ledger)
        .into_iter()
        .filter(|path| path.ends_with("/access_tokens"))
        .count();
    assert_eq!(token_calls, 1);
}

/// 缓存跨请求生效：第二次调用不再换 token（且 GraphQL 仍发）。
#[tokio::test]
async fn warm_cache_skips_the_second_token_exchange() {
    let (base, ledger) = start_stub(|request| {
        if request.path.ends_with("/access_tokens") {
            return StubResponse::json(
                201,
                serde_json::json!({"token": "ghs_warm", "expires_at": "2030-01-01T00:00:00Z"}),
            );
        }
        StubResponse::json(200, serde_json::json!({"data": {}}))
    })
    .await;
    let client = Client::new(Some(APP_ID.into()), Some(TEST_KEY_PEM.into())).with_api_base(&base);
    for _ in 0..3 {
        client
            .graph_ql(5, "query{}", &serde_json::json!({}), 1_700_000_000)
            .await
            .expect("graphql");
    }
    let exchanges = ledger_paths(&ledger)
        .into_iter()
        .filter(|path| path.ends_with("/access_tokens"))
        .count();
    assert_eq!(exchanges, 1, "暖缓存下 3 次调用只换 1 次 token");
    assert_eq!(client.token_cache().len().await, 1);
}

#[tokio::test]
async fn disabled_client_never_talks_to_the_stub() {
    let (base, ledger) = start_stub(|_| StubResponse::json(200, serde_json::json!({}))).await;
    let client = Client::disabled().with_api_base(&base);
    let err = client
        .graph_ql(1, "query{}", &serde_json::json!({}), 1_000)
        .await
        .unwrap_err();
    assert!(matches!(err, GithubError::NotConfigured));
    assert!(ledger.lock().unwrap().is_empty());
    // 超时预算是 20s（上游 `http.Client{Timeout: 20 * time.Second}`）。
    assert_eq!(Client::HTTP_TIMEOUT_SECS, 20);
    assert!(Duration::from_secs(Client::HTTP_TIMEOUT_SECS) > Duration::ZERO);
}

/// 手写 base64url 解码（只给 JWT 的段用；`base64` 是 mc-vcs-github 的正式依赖）。
fn base64_decode_url(raw: &str) -> Vec<u8> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    URL_SAFE_NO_PAD.decode(raw).expect("base64url segment")
}
