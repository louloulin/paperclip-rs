//! `client.rs` 的用例。
//!
//! - **纯函数**部分：`AccessToken` 的脱敏 `Debug`、缓存寿命口径、错误信封只取 `code`；
//! - **真 HTTP** 部分：一个本地 raw-TCP 替身，逐条钉 `mint → 缓存 → 401 ⇒ 作废 + 重试一次 \
//!   → bot 名按 `robotCode` 精确匹配` 这四条语义（`docs/60` §4.2 的"只替平台 wire"）。
//!
//! `set_api_base` 是**进程全局**的 ⇒ 用它的用例串行（[`BASE_LOCK`]），与 M7-8 的
//! `outbound/tests/http.rs` 同款。

use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;
use crate::dingtalk::outbound::openapi::{reset_api_base, set_api_base, DingTalkApiError};

/// 串行锁：`set_api_base` 是进程全局的。
static BASE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 起一个服务 `expected` 个请求的 raw-TCP 替身，返回基址与"收到的请求头 + 体"。
async fn serve(
    expected: usize,
    responses: Vec<(&'static str, String)>,
) -> (String, tokio::sync::mpsc::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let port = listener.local_addr().expect("addr").port();
    let (sender, receiver) = tokio::sync::mpsc::channel(expected.max(1));
    tokio::spawn(async move {
        let mut responses = responses.into_iter();
        for _ in 0..expected {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = vec![0_u8; 8 * 1024];
            let mut request = Vec::new();
            loop {
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
            }
            let _ = sender
                .send(String::from_utf8_lossy(&request).to_string())
                .await;
            let (status, body) = responses
                .next()
                .unwrap_or_else(|| ("200 OK", r"{}".to_string()));
            let payload = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(payload.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (format!("http://127.0.0.1:{port}"), receiver)
}

fn secret() -> AppSecret {
    AppSecret::new("app-secret-value")
}

// =====================================================================
// 纯函数
// =====================================================================

#[test]
fn access_token_debug_never_prints_the_token() {
    let token = AccessToken {
        value: "super-secret-token".to_string(),
        expire_in: 7200,
    };
    let rendered = format!("{token:?}");
    assert!(!rendered.contains("super-secret-token"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(format!(
        "{:?}",
        AccessToken {
            value: String::new(),
            expire_in: 0
        }
    )
    .contains("<empty>"));
}

#[test]
fn the_cache_lifetime_keeps_at_least_one_safety_margin() {
    // 上游：`ttl < 2*margin ⇒ ttl = 2*margin`，再减一个 margin ⇒ 有效寿命 ≥ margin。
    let tiny = AccessToken {
        value: "t".to_string(),
        expire_in: 10,
    };
    assert_eq!(tiny.usable_lifetime(), TOKEN_SAFETY_MARGIN);
    let normal = AccessToken {
        value: "t".to_string(),
        expire_in: 7200,
    };
    assert_eq!(
        normal.usable_lifetime(),
        std::time::Duration::from_mins(115)
    );
    // 平台给负数 / 0 也不 panic。
    let broken = AccessToken {
        value: "t".to_string(),
        expire_in: -5,
    };
    assert_eq!(broken.usable_lifetime(), TOKEN_SAFETY_MARGIN);
}

/// 模块文档第 3 条：平台错误体可能回声请求体（里面就是 `appSecret`）⇒ 只取 `code`。
#[test]
fn the_error_envelope_keeps_only_the_platform_code() {
    let echoed = json!({
        "code": "InvalidAuthentication",
        "message": "appKey=dingkey appSecret=app-secret-value rejected",
    });
    let error = envelope_error(ACCESS_TOKEN_PATH, 400, &echoed);
    let rendered = error.to_string();
    assert!(!rendered.contains("app-secret-value"), "{rendered}");
    assert!(!rendered.contains("dingkey"), "{rendered}");
    assert!(rendered.contains("InvalidAuthentication"), "{rendered}");
    assert_eq!(error.code(), "refused");

    // 没有 code 的信封 ⇒ 只报状态码。
    let error = envelope_error(ACCESS_TOKEN_PATH, 502, &json!({}));
    assert_eq!(error.code(), "http_status");
    assert!(error.to_string().contains("502"));
}

// =====================================================================
// 真 HTTP
// =====================================================================

#[tokio::test]
async fn minting_caches_per_app_key_and_never_repeats_a_hit() {
    let _guard = BASE_LOCK.lock().await;
    // 两次调用、**只有一次**铸造 ⇒ 第二次命中缓存（上游 `accessToken` 的缓存语义）。
    let (base, mut seen) = serve(
        1,
        vec![(
            "200 OK",
            json!({ "accessToken": "tok-1", "expireIn": 7200 }).to_string(),
        )],
    )
    .await;
    set_api_base(base);
    let client = Client::new().with_mint_timeout(std::time::Duration::from_secs(5));
    let first = Client::access_token(&client, "dingkey", &secret())
        .await
        .expect("mint");
    let second = Client::access_token(&client, "dingkey", &secret())
        .await
        .expect("cached");
    assert_eq!(first, "tok-1");
    assert_eq!(second, "tok-1");
    let request = seen.recv().await.expect("one request");
    assert!(request.contains(ACCESS_TOKEN_PATH), "{request}");
    // 请求体里**有** AppSecret（那是平台协议），但那是唯一一次。
    assert!(request.contains("app-secret-value"));
    reset_api_base();
}

#[tokio::test]
async fn a_401_on_a_lookup_invalidates_and_retries_exactly_once() {
    let _guard = BASE_LOCK.lock().await;
    // 顺序：① 铸令牌 → ② 查询 401 → ③ 重铸 → ④ 重试成功。
    let (base, mut seen) = serve(
        4,
        vec![
            (
                "200 OK",
                json!({ "accessToken": "stale", "expireIn": 7200 }).to_string(),
            ),
            ("401 Unauthorized", r#"{"code":"Unauthorized"}"#.to_string()),
            (
                "200 OK",
                json!({ "accessToken": "fresh", "expireIn": 7200 }).to_string(),
            ),
            (
                "200 OK",
                json!({ "chatbotInstanceVOList": [ { "robotCode": "rc", "name": "My Bot" } ] })
                    .to_string(),
            ),
        ],
    )
    .await;
    set_api_base(base);
    let client = Client::new().with_mint_timeout(std::time::Duration::from_secs(5));
    let name = Client::bot_name_in_group(&client, "dingkey", &secret(), "rc", "cid-1")
        .await
        .expect("retry once");
    assert_eq!(name, "My Bot");

    let mut headers = Vec::new();
    while let Ok(request) = seen.try_recv() {
        headers.push(request);
    }
    assert_eq!(headers.len(), 4, "恰好四次往返");
    assert!(headers[0].contains(ACCESS_TOKEN_PATH));
    assert!(headers[1].contains(GROUP_BOTS_PATH));
    assert!(headers[1].contains("stale"));
    assert!(headers[2].contains(ACCESS_TOKEN_PATH), "401 之后重铸");
    assert!(headers[3].contains("fresh"), "第二次用的是新令牌");
    reset_api_base();
}

#[tokio::test]
async fn the_bot_name_lookup_matches_the_robot_code_exactly() {
    let _guard = BASE_LOCK.lock().await;
    let (base, _seen) = serve(
        2,
        vec![
            (
                "200 OK",
                json!({ "accessToken": "t", "expireIn": 7200 }).to_string(),
            ),
            (
                "200 OK",
                json!({ "chatbotInstanceVOList": [
                    { "robotCode": "someone-else", "name": "Other" },
                    { "robotCode": "rc", "name": "  My Bot  " },
                ] })
                .to_string(),
            ),
        ],
    )
    .await;
    set_api_base(base);
    let client = Client::new().with_mint_timeout(std::time::Duration::from_secs(5));
    // 上游逐字：绝不按列表位置推身份，也不持久化别的机器人的元数据。
    let name = Client::bot_name_in_group(&client, "dingkey", &secret(), "rc", "cid")
        .await
        .expect("exact match");
    assert_eq!(name, "My Bot", "两端空白被 trim");
    reset_api_base();
}

#[tokio::test]
async fn a_robot_absent_from_the_group_list_is_an_error_not_an_empty_name() {
    let _guard = BASE_LOCK.lock().await;
    let (base, _seen) = serve(
        2,
        vec![
            (
                "200 OK",
                json!({ "accessToken": "t", "expireIn": 7200 }).to_string(),
            ),
            (
                "200 OK",
                json!({ "chatbotInstanceVOList": [ { "robotCode": "other", "name": "Other" } ] })
                    .to_string(),
            ),
        ],
    )
    .await;
    set_api_base(base);
    let client = Client::new().with_mint_timeout(std::time::Duration::from_secs(5));
    let error = Client::bot_name_in_group(&client, "dingkey", &secret(), "rc", "cid")
        .await
        .expect_err("absent");
    assert_eq!(error.code(), "invalid_target");
    reset_api_base();
}

#[tokio::test]
async fn a_lookup_without_identifiers_never_reaches_the_network() {
    let client = Client::new();
    let error = Client::bot_name_in_group(&client, "k", &secret(), "  ", "cid")
        .await
        .expect_err("missing robot code");
    assert_eq!(error.code(), "invalid_target");
    let error = Client::bot_name_in_group(&client, "k", &secret(), "rc", "")
        .await
        .expect_err("missing conversation");
    assert_eq!(error.code(), "invalid_target");
}

#[tokio::test]
async fn transport_errors_never_echo_the_endpoint_or_the_secret() {
    // 指向一个不可达端口 ⇒ 传输失败；错误文案里既没有 URL 也没有凭据。
    let _guard = BASE_LOCK.lock().await;
    set_api_base("http://127.0.0.1:1");
    let client = Client::new().with_mint_timeout(std::time::Duration::from_secs(3));
    let error: DingTalkApiError = Client::access_token(&client, "dingkey", &secret())
        .await
        .expect_err("unreachable");
    let rendered = error.to_string();
    assert!(!rendered.contains("app-secret-value"), "{rendered}");
    assert!(!rendered.contains("127.0.0.1"), "{rendered}");
    assert_eq!(error.code(), "transport");
    reset_api_base();
}

#[test]
fn the_client_is_also_an_open_api_transport() {
    // `docs/32` §22 的 D3 交接项：M7-9 的完整客户端**换实现即可**
    // （在 `DingTalkDeps::with_outbound` 处换掉 `HttpOpenApi`）。类型层面的证据就是下面这一行。
    fn assert_transport<T: OpenApiTransport + Send + Sync>() {}
    assert_transport::<Client>();
}
