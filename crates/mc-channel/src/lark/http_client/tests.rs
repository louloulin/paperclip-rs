//! `http_client.rs` 的用例：**真 HTTP**（本地 raw-TCP 替身）钉令牌缓存 / 过期刷新 / 被拒后的
//! 作废 + 重放一次 / 三类错误反例 / 各端点的 wire 形状 / **错误路径不回显凭据**。
//!
//! 纪律照 M7-8 / M7-9（`dingtalk/outbound/tests/http.rs`、`dingtalk/client/tests.rs`）：
//! **只替平台 wire，不替业务路径** —— 走真 `reqwest`、真超时、真 JSON 编解码，替身只回字节。
//! 与那两处不同的一点：本片的基址是**配置字段**（上游 `HTTPClientConfig.BaseURL`）而不是
//! 进程全局，所以用例之间**不需要**串行锁。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;
use crate::lark::client::{
    is_thread_reply_unsupported, ApiClient, ApiError, ErrorClass, StubApiClient,
};
use crate::lark::params::{AppSecret, ReplyTarget, SendCardParams, SendTextParams};
use crate::lark::types::{ChatId, Region, DEFAULT_LARK_BASE_URL, LARK_INTERNATIONAL_OPEN_BASE_URL};

// =====================================================================
// 替身（raw TCP；每条脚本项 = 一次响应）
// =====================================================================

/// 一次脚本化响应。
///
/// `pub(crate)`：另外两个测试模块（`http_client/api/tests.rs` / `http_client/resource/tests.rs`）
/// 共享这一份替身。
#[derive(Default)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) content_type: &'static str,
    pub(crate) body: Vec<u8>,
    /// 覆盖 `content-length`（默认按 `body.len()`）—— 只有"超上限"用例需要它。
    pub(crate) content_length: Option<usize>,
    /// 可选 `Content-Disposition`（只有文件名用例需要它）。
    pub(crate) disposition: Option<&'static str>,
}

pub(crate) fn json_reply(status: u16, body: &str) -> Reply {
    Reply {
        status,
        content_type: "application/json; charset=utf-8",
        body: body.as_bytes().to_vec(),
        ..Reply::default()
    }
}

pub(crate) fn binary_reply(content_type: &'static str, body: Vec<u8>) -> Reply {
    Reply {
        status: 200,
        content_type,
        body,
        ..Reply::default()
    }
}

fn html_reply(status: u16, body: &str) -> Reply {
    Reply {
        status,
        content_type: "text/html",
        body: body.as_bytes().to_vec(),
        ..Reply::default()
    }
}

/// 替身收到的一条请求（原样记录，供用例断言 wire 形状）。
#[derive(Debug, Clone)]
pub(crate) struct Recorded {
    request_line: String,
    headers: Vec<(String, String)>,
    pub(crate) body: String,
}

impl Recorded {
    pub(crate) fn method(&self) -> &str {
        self.request_line.split(' ').next().unwrap_or_default()
    }

    pub(crate) fn target(&self) -> &str {
        self.request_line.split(' ').nth(1).unwrap_or_default()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn bearer(&self) -> Option<&str> {
        self.header("authorization")?.strip_prefix("Bearer ")
    }

    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

/// 起一个按脚本回应的替身；返回它的基址与"收到的请求"台账。
pub(crate) async fn serve(script: Vec<Reply>) -> (String, Arc<Mutex<Vec<Recorded>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let port = listener.local_addr().expect("addr").port();
    let records = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&records);
    let script = Arc::new(Mutex::new(script));
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let Ok(request) = read_request(&mut socket).await else {
                continue;
            };
            let reply = {
                let mut guard = script.lock().expect("script");
                if guard.is_empty() {
                    None
                } else {
                    Some(guard.remove(0))
                }
            };
            if let Ok(mut guard) = recorder.lock() {
                guard.push(request);
            }
            let reply = reply.unwrap_or_else(|| json_reply(500, r#"{"code":500}"#));
            let length = reply.content_length.unwrap_or(reply.body.len());
            let mut head = format!(
                "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {length}\r\n",
                reply.status,
                reason(reply.status),
                reply.content_type
            );
            if let Some(disposition) = reply.disposition {
                head.push_str("content-disposition: ");
                head.push_str(disposition);
                head.push_str("\r\n");
            }
            head.push_str("connection: close\r\n\r\n");
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&reply.body).await;
            let _ = socket.shutdown().await;
        }
    });
    (format!("http://127.0.0.1:{port}"), records)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        429 => "Too Many Requests",
        _ => "Error",
    }
}

/// 读一条完整的 HTTP/1.1 请求（按 `content-length` 收满体）。
async fn read_request(socket: &mut tokio::net::TcpStream) -> Result<Recorded, ()> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = socket.read(&mut chunk).await.map_err(|_| ())?;
        if read == 0 {
            return Err(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        let Some(header_end) = find_header_end(&buffer) else {
            continue;
        };
        let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
        let length = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if buffer.len() >= header_end + 4 + length {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buffer).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let headers = lines
        .filter_map(|line| {
            line.split_once(':')
                .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    Ok(Recorded {
        request_line,
        headers,
        body: body.to_string(),
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

// =====================================================================
// 装置
// =====================================================================

const APP_ID: &str = "cli_test_app";
const APP_SECRET: &str = "super-secret-app-secret-value";

pub(crate) fn token_reply(token: &str, expire: i64) -> Reply {
    json_reply(
        200,
        &format!(r#"{{"code":0,"tenant_access_token":"{token}","expire":{expire}}}"#),
    )
}

pub(crate) fn client(base_url: &str) -> HttpApiClient {
    HttpApiClient::new(HttpClientConfig::new().with_base_url(base_url))
}

pub(crate) fn credentials() -> InstallationCredentials {
    InstallationCredentials::new(APP_ID, AppSecret::new(APP_SECRET))
}

fn lark_credentials() -> InstallationCredentials {
    credentials().with_region(Region::Lark)
}

/// 可推进的时钟（确定性过期测试）。
fn test_clock() -> (Arc<Mutex<Duration>>, Clock) {
    let base = Instant::now();
    let offset = Arc::new(Mutex::new(Duration::ZERO));
    let handle = Arc::clone(&offset);
    let clock: Clock = Arc::new(move || base + *handle.lock().expect("clock"));
    (offset, clock)
}

fn advance(handle: &Arc<Mutex<Duration>>, by: Duration) {
    *handle.lock().expect("clock") += by;
}

pub(crate) fn recorded(records: &Arc<Mutex<Vec<Recorded>>>) -> Vec<Recorded> {
    records.lock().expect("records").clone()
}

// =====================================================================
// 令牌：缓存与过期刷新（本片专属验收第 1 条）
// =====================================================================

#[tokio::test]
async fn a_live_token_is_minted_once_and_reused() {
    let (base, records) = serve(vec![token_reply("t1", 7200)]).await;
    let client = client(&base);

    assert_eq!(
        client
            .tenant_access_token(&credentials())
            .await
            .expect("t1"),
        "t1"
    );
    assert_eq!(
        client
            .tenant_access_token(&credentials())
            .await
            .expect("t1"),
        "t1"
    );

    let requests = recorded(&records);
    assert_eq!(requests.len(), 1, "第二次必须命中缓存，不再往返");
    assert_eq!(requests[0].method(), "POST");
    assert_eq!(requests[0].target(), TENANT_ACCESS_TOKEN_PATH);
    // 铸令牌请求体里就是明文 `app_secret`（所以错误路径的回显风险是真实的）。
    assert_eq!(requests[0].json()["app_id"], APP_ID);
    assert_eq!(requests[0].json()["app_secret"], APP_SECRET);
    // 铸令牌那条**不带** `Authorization`（还没有令牌）。
    assert!(requests[0].bearer().is_none());
}

#[tokio::test]
async fn an_expired_token_is_replaced() {
    let (offset, clock) = test_clock();
    let (base, records) = serve(vec![token_reply("t1", 7200), token_reply("t2", 7200)]).await;
    let client = HttpApiClient::new(HttpClientConfig::new().with_base_url(&base).with_now(clock));

    assert_eq!(
        client
            .tenant_access_token(&credentials())
            .await
            .expect("t1"),
        "t1"
    );
    // 安全窗口是 `7200 - 60 = 7140s`：推进 3600s 仍应命中缓存。
    advance(&offset, Duration::from_secs(3600));
    assert_eq!(
        client
            .tenant_access_token(&credentials())
            .await
            .expect("t1"),
        "t1"
    );
    assert_eq!(recorded(&records).len(), 1);

    // 越过安全窗口 ⇒ 重铸。
    advance(&offset, Duration::from_secs(3600));
    assert_eq!(
        client
            .tenant_access_token(&credentials())
            .await
            .expect("t2"),
        "t2"
    );
    assert_eq!(recorded(&records).len(), 2);
}

#[tokio::test]
async fn a_tiny_expire_is_clamped_to_two_safety_margins() {
    // 平台回一个亚分钟的 `expire`：缓存寿命夹到 `2 × 60 - 60 = 60s`
    // （否则我们会缓存一枚已经过了安全窗口的令牌 —— 上游的夹紧判据）。
    let (offset, clock) = test_clock();
    let (base, records) = serve(vec![token_reply("t1", 1), token_reply("t2", 1)]).await;
    let client = HttpApiClient::new(HttpClientConfig::new().with_base_url(&base).with_now(clock));

    assert_eq!(
        client
            .tenant_access_token(&credentials())
            .await
            .expect("t1"),
        "t1"
    );
    advance(&offset, Duration::from_secs(59));
    assert_eq!(
        client
            .tenant_access_token(&credentials())
            .await
            .expect("t1"),
        "t1"
    );
    assert_eq!(recorded(&records).len(), 1);

    // 第 61 秒已经越过 60s 的夹紧窗口。
    advance(&offset, Duration::from_secs(2));
    assert_eq!(
        client
            .tenant_access_token(&credentials())
            .await
            .expect("t2"),
        "t2"
    );
    assert_eq!(recorded(&records).len(), 2);
}

#[tokio::test]
async fn minting_requires_both_credential_halves() {
    // 缺一半就在**发之前**拒（不产生任何请求）。
    let (base, records) = serve(vec![]).await;
    let client = client(&base);

    let error = client
        .tenant_access_token(&InstallationCredentials::new(
            "",
            AppSecret::new(APP_SECRET),
        ))
        .await
        .expect_err("missing app_id");
    assert_eq!(
        error,
        ApiError::InvalidRequest {
            op: "tenant access token",
            reason: "missing app_id"
        }
    );
    let error = client
        .tenant_access_token(&InstallationCredentials::new(APP_ID, AppSecret::default()))
        .await
        .expect_err("missing app_secret");
    assert_eq!(
        error,
        ApiError::InvalidRequest {
            op: "tenant access token",
            reason: "missing app_secret"
        }
    );
    assert!(recorded(&records).is_empty());
}

// =====================================================================
// 被拒即作废 + 重放一次（上游 `doAuthedJSON`）
// =====================================================================

#[tokio::test]
async fn a_rejected_token_invalidates_the_cache_and_replays_exactly_once() {
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        // 第一次发送：Lark 把凭据失效表达成 **HTTP 400 + code**（不是 2xx 信封）。
        json_reply(400, r#"{"code":99991663,"msg":"token expired"}"#),
        token_reply("t2", 7200),
        json_reply(200, r#"{"code":0,"data":{"message_id":"om_new"}}"#),
    ])
    .await;
    let client = client(&base);

    let params = SendCardParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        card_json: "{}".to_string(),
        reply_target: ReplyTarget::default(),
    };
    let message_id = client
        .send_interactive_card(params)
        .await
        .expect("重放应当成功");
    assert_eq!(message_id, "om_new");

    let requests = recorded(&records);
    assert_eq!(requests.len(), 4, "铸 → 失败 → 重铸 → 重放恰好一次");
    assert_eq!(requests[0].target(), TENANT_ACCESS_TOKEN_PATH);
    let send_path = "/open-apis/im/v1/messages?receive_id_type=chat_id";
    assert_eq!(requests[1].target(), send_path);
    assert_eq!(requests[1].bearer(), Some("t1"));
    assert_eq!(requests[2].target(), TENANT_ACCESS_TOKEN_PATH);
    assert_eq!(requests[3].target(), send_path);
    assert_eq!(requests[3].bearer(), Some("t2"), "重放必须用新令牌");
}

#[tokio::test]
async fn a_two_xx_envelope_carrying_a_token_code_also_refreshes_and_replays() {
    // 同一条判据的另一半：Lark 也可能用 **2xx + 非零 code** 说凭据不认。
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(200, r#"{"code":99991664,"msg":"app token invalid"}"#),
        token_reply("t2", 7200),
        json_reply(200, r#"{"code":0,"data":{"message_id":"om_retry"}}"#),
    ])
    .await;
    let client = client(&base);
    let params = SendTextParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        text: "hi".to_string(),
        reply_target: ReplyTarget::default(),
    };
    assert_eq!(
        client.send_text_message(params).await.expect("replay"),
        "om_retry"
    );
    assert_eq!(recorded(&records).len(), 4);
}

#[tokio::test]
async fn a_definitive_business_refusal_is_never_retried() {
    // 反例：请求到了、Lark 明确拒绝、**什么都没发** ⇒ 换令牌不解决，不重放。
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(
            200,
            r#"{"code":230011,"msg":"the message has been recalled"}"#,
        ),
    ])
    .await;
    let client = client(&base);
    let params = SendCardParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        card_json: "{}".to_string(),
        reply_target: ReplyTarget::default(),
    };
    let error = client
        .send_interactive_card(params)
        .await
        .expect_err("refused");
    assert_eq!(
        error,
        ApiError::Refused {
            op: "send interactive card",
            status: None,
            code: 230_011
        }
    );
    assert_eq!(error.class(), ErrorClass::Refused);
    // 只有那两条请求：铸 + 一次发送。
    assert_eq!(recorded(&records).len(), 2);
    // 而这一个码**是**允许回落会话层发送的那六个之一（由调用方决定要不要回落）。
    assert!(is_thread_reply_unsupported(&error));
}

// =====================================================================
// 错误分类：限流 / 凭据失效 / 网络 各一反例（真 HTTP）
// =====================================================================

#[tokio::test]
async fn a_429_is_a_rate_limit_and_costs_no_extra_round_trip() {
    let (base, records) = serve(vec![token_reply("t1", 7200), json_reply(429, "")]).await;
    let client = client(&base);
    let params = SendTextParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        text: "hi".to_string(),
        reply_target: ReplyTarget::default(),
    };
    let error = client.send_text_message(params).await.expect_err("429");
    assert_eq!(
        error,
        ApiError::Http {
            op: "send text message",
            status: 429
        }
    );
    assert_eq!(error.class(), ErrorClass::RateLimited);
    assert_eq!(recorded(&records).len(), 2, "限流**不**重放");
}

#[tokio::test]
async fn a_platform_rate_limit_code_is_also_a_rate_limit() {
    // IM 发送端点的频控码（上游把它排除在"不能线程回复"之外的那一个）。
    let (base, _records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(200, r#"{"code":230020,"msg":"too many requests"}"#),
    ])
    .await;
    let client = client(&base);
    let params = SendTextParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        text: "hi".to_string(),
        reply_target: ReplyTarget::default(),
    };
    let error = client.send_text_message(params).await.expect_err("limited");
    assert_eq!(error.class(), ErrorClass::RateLimited);
    assert!(!is_thread_reply_unsupported(&error));
}

#[tokio::test]
async fn a_bad_credential_never_loops_forever() {
    // 凭据**真的**坏了：作废 + 重铸 + 重放一次之后仍然被拒 ⇒ 把第二次的错误交出去
    // （不许无限重试）。
    let (base, records) = serve(vec![json_reply(
        400,
        r#"{"code":10003,"msg":"app_secret invalid"}"#,
    )])
    .await;
    let client = client(&base);
    let error = client
        .tenant_access_token(&credentials())
        .await
        .expect_err("bootstrap credentials are dead");
    assert_eq!(error.class(), ErrorClass::Refused);
    assert_eq!(recorded(&records).len(), 1);
}

#[tokio::test]
async fn a_network_failure_is_a_transport_error() {
    // 拿一个"绑过再放掉"的端口：连接必然被拒（不是超时，所以用例很快）。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let client = client(&format!("http://127.0.0.1:{port}"));
    let error = client
        .tenant_access_token(&credentials())
        .await
        .expect_err("connection refused");
    assert_eq!(
        error,
        ApiError::Transport {
            op: "tenant access token"
        }
    );
    assert_eq!(error.class(), ErrorClass::Transport);
}

#[tokio::test]
async fn a_non_json_error_body_stays_on_the_transport_path() {
    // 代理的 HTML 错误页：没有可解析的平台码 ⇒ 交付与否不明确 ⇒ 传输类。
    let (base, _records) = serve(vec![
        token_reply("t1", 7200),
        html_reply(502, "<html>bad gateway</html>"),
    ])
    .await;
    let client = client(&base);
    let params = SendTextParams {
        credentials: credentials(),
        chat_id: ChatId::new("oc_1"),
        text: "hi".to_string(),
        reply_target: ReplyTarget::default(),
    };
    let error = client.send_text_message(params).await.expect_err("502");
    assert_eq!(
        error,
        ApiError::Http {
            op: "send text message",
            status: 502
        }
    );
    assert_eq!(error.class(), ErrorClass::Transport);
}

// =====================================================================
// 凭据不回显（本片 DoD 第 6 条）
// =====================================================================

#[tokio::test]
async fn error_paths_never_echo_the_app_secret() {
    // 平台把请求体**回声**回来（真实风险：铸令牌的请求体里就是 `app_secret`）。
    let echoed = format!(r#"{{"code":10003,"msg":"invalid app_secret: {APP_SECRET}"}}"#);
    let (base, records) = serve(vec![json_reply(400, &echoed)]).await;
    let client = client(&base);

    let error = client
        .tenant_access_token(&credentials())
        .await
        .expect_err("rejected");

    // ① 用例不是空跑：那一次请求**确实**把明文发出去了。
    let requests = recorded(&records);
    assert_eq!(requests.len(), 1);
    assert!(requests[0].body.contains(APP_SECRET));

    // ② 错误对象的两条格式化路径都不含它。
    let display = error.to_string();
    let debug = format!("{error:?}");
    assert!(!display.contains(APP_SECRET), "Display 漏了：{display}");
    assert!(!debug.contains(APP_SECRET), "Debug 漏了：{debug}");
    // 也**不带**平台的 `msg`（那正是回声的载体）。
    assert!(!display.contains("invalid app_secret"));

    // ③ 客户端自身的 `Debug` 只报缓存条数，不打印缓存里的令牌。
    let _ = client
        .tenant_access_token(&InstallationCredentials::new(
            APP_ID,
            AppSecret::new(APP_SECRET),
        ))
        .await;
    let client_debug = format!("{client:?}");
    assert!(!client_debug.contains(APP_SECRET));
}

#[tokio::test]
async fn a_minted_token_never_reaches_a_debug_rendering() {
    let (base, _records) = serve(vec![token_reply("tenant-access-token-abc", 7200)]).await;
    let client = client(&base);
    let _ = client
        .tenant_access_token(&credentials())
        .await
        .expect("minted");
    let rendered = format!("{client:?}");
    assert!(!rendered.contains("tenant-access-token-abc"));
    assert!(rendered.contains("cached_tokens"));
}

// =====================================================================
// 主机解析（region）
// =====================================================================

#[test]
fn the_cloud_host_comes_from_the_installation_region_without_an_override() {
    let client = HttpApiClient::new(HttpClientConfig::new());
    assert_eq!(
        client.resolve_base_url(&credentials()),
        DEFAULT_LARK_BASE_URL
    );
    assert_eq!(
        client.resolve_base_url(&lark_credentials()),
        LARK_INTERNATIONAL_OPEN_BASE_URL
    );
}

#[test]
fn an_explicit_base_url_overrides_every_region() {
    let client = client("http://127.0.0.1:9/");
    // 尾部 `/` 被剥掉（否则会拼出 `//open-apis/...`）。
    assert_eq!(
        client.resolve_base_url(&credentials()),
        "http://127.0.0.1:9"
    );
    // 覆盖**无视** region。
    assert_eq!(
        client.resolve_base_url(&lark_credentials()),
        "http://127.0.0.1:9"
    );
}

#[test]
fn an_empty_override_is_the_same_as_no_override() {
    let config = HttpClientConfig::new().with_base_url("");
    assert!(config.base_url.is_none());
    let client = HttpApiClient::new(config);
    assert_eq!(
        client.resolve_base_url(&lark_credentials()),
        LARK_INTERNATIONAL_OPEN_BASE_URL
    );
}

// =====================================================================
// 端点 wire 形状
// =====================================================================

#[tokio::test]
async fn the_real_client_reports_itself_configured() {
    let (base, _records) = serve(vec![]).await;
    let client = client(&base);
    assert!(client.is_configured());
}

#[tokio::test]
async fn the_token_cache_invalidator_forgets_one_app_only() {
    let (base, records) = serve(vec![token_reply("t1", 7200), token_reply("t1", 7200)]).await;
    let client = client(&base);
    assert_eq!(
        client
            .tenant_access_token(&credentials())
            .await
            .expect("t1"),
        "t1"
    );
    // 凭据轮换：重新注册 Bot 会在**同一个 `app_id`** 下发新密钥，
    // 缓存键不变 ⇒ 必须能被显式叫停。
    client.invalidate_token_cache(APP_ID);
    assert_eq!(
        client
            .tenant_access_token(&credentials())
            .await
            .expect("t1"),
        "t1"
    );
    assert_eq!(recorded(&records).len(), 2, "作废后必须重铸");
    // 作废别的 `app_id` 不影响这一格。
    client.invalidate_token_cache("cli_someone_else");
    assert_eq!(
        client
            .tenant_access_token(&credentials())
            .await
            .expect("t1"),
        "t1"
    );
    assert_eq!(recorded(&records).len(), 2);
}

#[tokio::test]
async fn the_client_can_be_used_behind_a_dyn_port() {
    let (base, _records) = serve(vec![token_reply("t1", 7200)]).await;
    let port: Arc<dyn ApiClient> = Arc::new(client(&base));
    assert!(port.is_configured());
    let stub: Arc<dyn ApiClient> = Arc::new(StubApiClient::new());
    assert!(!stub.is_configured());
}
