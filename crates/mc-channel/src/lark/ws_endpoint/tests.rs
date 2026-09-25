//! [`super`]（引导面）的用例：
//!
//! 1. **纯函数**：请求体形态（PascalCase）/ 主机解析（region 与覆盖）/ `service_id` 解析；
//! 2. **真 `reqwest`**：本地 loopback 服务端跑完整的引导（含 `Content-Type` / `locale` 头）；
//! 3. **凭据纪律**：两个承载凭据的类型的 `Debug` 脱敏 + 三条「错误路径不回显凭据」用例。

use std::io::Write as _;
use std::time::Duration;

use reqwest::header::HeaderMap;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{
    bootstrap_base_url, build_bootstrap_request, parse_service_id_from_url, scrub_secret, seconds,
    EndpointFetcher, HttpEndpointFetcher, WsEndpoint, BOOTSTRAP_CONTENT_TYPE, BOOTSTRAP_LOCALE,
    WS_ENDPOINT_PATH,
};
use crate::channel::ChannelError;
use crate::lark::params::{AppSecret, InstallationCredentials};
use crate::lark::types::{Region, DEFAULT_LARK_BASE_URL, LARK_INTERNATIONAL_OPEN_BASE_URL};

fn credentials() -> InstallationCredentials {
    InstallationCredentials::new("cli_app_x", AppSecret::new("secret-xyz"))
}

// =====================================================================
// 一、纯函数
// =====================================================================

#[test]
fn the_bootstrap_request_uses_the_pascal_case_wire_shape() {
    let request = build_bootstrap_request(&credentials());
    assert_eq!(request.app_id, "cli_app_x");
    assert_eq!(request.app_secret, "secret-xyz");
    // 上游逐字：字段名是 `AppID` / `AppSecret`，**不是** snake_case（官方 SDK 的 schema 定的）。
    let wire = serde_json::to_value(&request).expect("序列化");
    assert_eq!(wire["AppID"], serde_json::json!("cli_app_x"));
    assert_eq!(wire["AppSecret"], serde_json::json!("secret-xyz"));
    assert_eq!(wire.as_object().map(serde_json::Map::len), Some(2));
}

#[test]
fn the_base_url_comes_from_the_region_unless_overridden() {
    let feishu = InstallationCredentials::new("cli", AppSecret::new("s"));
    let lark = InstallationCredentials::new("cli", AppSecret::new("s")).with_region(Region::Lark);

    let plain = HttpEndpointFetcher::new();
    assert_eq!(plain.base_url(), None);
    assert_eq!(plain.resolve_base_url(&feishu), DEFAULT_LARK_BASE_URL);
    assert_eq!(
        plain.resolve_base_url(&lark),
        LARK_INTERNATIONAL_OPEN_BASE_URL
    );

    // 部署级覆盖**无视** region（上游 `MULTICA_LARK_CALLBACK_BASE_URL` 的语义）；
    // 尾部 `/` 被剥掉（否则会拼出 `//callback/ws/endpoint`）。
    let forced = HttpEndpointFetcher::with_base_url("http://127.0.0.1:9/");
    assert_eq!(forced.base_url(), Some("http://127.0.0.1:9"));
    assert_eq!(forced.resolve_base_url(&lark), "http://127.0.0.1:9");
    // 空串 = 没有覆盖（不是"空主机"）。
    assert_eq!(HttpEndpointFetcher::with_base_url("").base_url(), None);

    assert_eq!(bootstrap_base_url(Region::Feishu), DEFAULT_LARK_BASE_URL);
    assert_eq!(
        bootstrap_base_url(Region::Lark),
        "https://open.larksuite.com"
    );
}

#[test]
fn the_service_id_is_parsed_from_the_wss_url() {
    assert_eq!(
        parse_service_id_from_url("wss://lark.example/ws/foo?device_id=d1&service_id=42"),
        Ok(42)
    );
    assert_eq!(
        parse_service_id_from_url("wss://lark.example/ws/foo?service_id=-1"),
        Ok(-1)
    );

    let cases = [
        "wss://lark.example/ws/foo?device_id=d1",
        "wss://lark.example/ws/foo?service_id=not-a-number",
        "not a url",
        "",
    ];
    for raw in cases {
        let error = parse_service_id_from_url(raw).expect_err("必须失败");
        let rendered = error.to_string();
        // 那条 URL 自带一次性 `device_id`（等价于凭据）⇒ 错误里**不得**出现它。
        assert!(!rendered.contains("device_id"), "{rendered}");
        assert!(!rendered.contains("lark.example"), "{rendered}");
    }
}

#[test]
fn only_positive_second_counts_become_durations() {
    assert_eq!(seconds(120), Duration::from_secs(120));
    assert_eq!(seconds(0), Duration::ZERO, "服务端省略该字段");
    assert_eq!(seconds(-1), Duration::ZERO, "上游把负数当缺失");
}

#[test]
fn scrubbing_only_replaces_a_non_empty_secret() {
    assert_eq!(
        scrub_secret("bad app_secret=secret-xyz here", "secret-xyz"),
        "bad app_secret=<redacted> here"
    );
    // 空 secret 绝不能替换（`str::replace` 对空串会插到每个位置，把文案毁掉）。
    assert_eq!(scrub_secret("hello", ""), "hello");
    assert_eq!(scrub_secret("", "secret"), "");
}

// =====================================================================
// 二、凭据纪律：两个类型的 `Debug`
// =====================================================================

#[test]
fn the_bootstrap_request_debug_redacts_the_secret() {
    let rendered = format!("{:?}", build_bootstrap_request(&credentials()));
    assert!(!rendered.contains("secret-xyz"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(
        rendered.contains("cli_app_x"),
        "app_id 不是秘密：{rendered}"
    );
}

#[test]
fn the_endpoint_debug_redacts_the_url_and_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer TOP-SECRET".parse().expect("头值"));
    let endpoint = WsEndpoint {
        url: "wss://lark.example/ws?device_id=dev-1&service_id=42".to_string(),
        headers,
        service_id: 42,
        ping_interval: Duration::from_secs(120),
        ..WsEndpoint::default()
    };
    let rendered = format!("{endpoint:?}");
    assert!(!rendered.contains("dev-1"), "一次性地址是凭据：{rendered}");
    assert!(!rendered.contains("lark.example"), "{rendered}");
    assert!(!rendered.contains("TOP-SECRET"), "握手头要脱敏：{rendered}");
    // 非秘密的运行参数照常给出（运维要靠它）。
    assert!(rendered.contains("service_id: 42"), "{rendered}");
    assert!(rendered.contains("120s"), "{rendered}");
}

// =====================================================================
// 三、真 reqwest 的 loopback 服务端
// =====================================================================

fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}

/// 起一个**只服务一次**的 HTTP 服务端，返回它的基址与"收到的请求"。
async fn serve_once(response: String) -> (String, tokio::sync::oneshot::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("local addr");
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0_u8; 16 * 1024];
        let read = socket.read(&mut buffer).await.unwrap_or(0);
        let _ = sender.send(String::from_utf8_lossy(&buffer[..read]).to_string());
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.flush().await;
    });
    (format!("http://{address}"), receiver)
}

/// 引导成功的完整响应（上游 `ws_endpoint_test.go` 的 happy path）。
const SUCCESS_BODY: &str = r#"{
    "code": 0,
    "data": {
        "URL": "wss://lark.example/ws/foo?device_id=dev-1&service_id=42",
        "ClientConfig": {
            "ReconnectCount": -1,
            "ReconnectInterval": 120,
            "ReconnectNonce": 30,
            "PingInterval": 120
        }
    }
}"#;

#[tokio::test]
async fn the_bootstrap_posts_the_credentials_and_assembles_the_endpoint() {
    let (base, request) = serve_once(http_response("200 OK", SUCCESS_BODY)).await;
    let fetcher = HttpEndpointFetcher::with_base_url(&base);

    let endpoint = fetcher.endpoint(&credentials()).await.expect("引导成功");

    assert_eq!(
        endpoint.url,
        "wss://lark.example/ws/foo?device_id=dev-1&service_id=42"
    );
    assert_eq!(endpoint.service_id, 42);
    assert_eq!(endpoint.ping_interval, Duration::from_secs(120));
    assert_eq!(endpoint.reconnect_interval, Duration::from_secs(120));
    assert_eq!(endpoint.reconnect_nonce, Duration::from_secs(30));
    assert_eq!(endpoint.reconnect_count, -1);
    assert!(endpoint.headers.is_empty(), "lark 当前不下发握手头");

    let request = request.await.expect("服务端记下了请求");
    assert!(
        request.starts_with(&format!("POST {WS_ENDPOINT_PATH} ")),
        "引导打的是 **lark 的** `/callback/ws/endpoint`（不是本服务的路由）：{request}"
    );
    // 上游逐字：引导走的是明文凭据，**不带**授权头。
    assert!(
        !request.to_lowercase().contains("authorization"),
        "{request}"
    );
    assert!(request.contains(BOOTSTRAP_CONTENT_TYPE), "{request}");
    assert!(
        request.contains(&format!("locale: {BOOTSTRAP_LOCALE}")),
        "{request}"
    );
    let body: serde_json::Value = serde_json::from_str(
        request
            .split("\r\n\r\n")
            .nth(1)
            .expect("请求体")
            .trim_end_matches('\0'),
    )
    .expect("请求体是 JSON");
    assert_eq!(body["AppID"], serde_json::json!("cli_app_x"));
    assert_eq!(body["AppSecret"], serde_json::json!("secret-xyz"));
}

#[tokio::test]
async fn a_missing_business_field_falls_back_to_zero_durations() {
    // 服务端省略整个 `ClientConfig` ⇒ 三个时长都是 0（连接器据此套静态默认值）。
    let body = r#"{"code":0,"data":{"URL":"wss://lark.example/ws?service_id=7"}}"#;
    let (base, _request) = serve_once(http_response("200 OK", body)).await;
    let endpoint = HttpEndpointFetcher::with_base_url(&base)
        .endpoint(&credentials())
        .await
        .expect("引导成功");
    assert_eq!(endpoint.service_id, 7);
    assert_eq!(endpoint.ping_interval, Duration::ZERO);
    assert_eq!(endpoint.reconnect_interval, Duration::ZERO);
    assert_eq!(endpoint.reconnect_count, 0);
}

#[tokio::test]
async fn a_response_without_a_service_id_is_a_failure() {
    let body = r#"{"code":0,"data":{"URL":"wss://lark.example/ws?device_id=dev-1"}}"#;
    let (base, _request) = serve_once(http_response("200 OK", body)).await;
    let error = HttpEndpointFetcher::with_base_url(&base)
        .endpoint(&credentials())
        .await
        .expect_err("没有 service_id 就必须失败");
    let rendered = error.to_string();
    assert!(
        !rendered.contains("dev-1"),
        "不得回显一次性地址：{rendered}"
    );
    assert!(!rendered.contains("lark.example"), "{rendered}");
}

#[tokio::test]
async fn an_unexpected_body_is_a_clear_failure() {
    let (base, _request) = serve_once(http_response("200 OK", "not json")).await;
    let error = HttpEndpointFetcher::with_base_url(&base)
        .endpoint(&credentials())
        .await
        .expect_err("非 JSON 必须失败");
    assert!(error.to_string().contains("expected JSON"), "{error}");

    // 2xx 但业务码非 0。
    let (base, _request) = serve_once(http_response("200 OK", r#"{"code":403}"#)).await;
    let error = HttpEndpointFetcher::with_base_url(&base)
        .endpoint(&credentials())
        .await
        .expect_err("业务码非 0 必须失败");
    assert!(error.to_string().contains("403"), "{error}");
}

// =====================================================================
// 四、凭据纪律：错误路径
// =====================================================================

#[tokio::test]
async fn missing_credentials_fail_before_any_request() {
    // 基址指向一个**没有**服务端在听的端口：要是先发请求再校验，这里会变成"传输错误"。
    let fetcher = HttpEndpointFetcher::with_base_url("http://127.0.0.1:9");
    let no_id = InstallationCredentials::new("", AppSecret::new("secret-xyz"));
    let error = fetcher.endpoint(&no_id).await.expect_err("缺 app_id");
    assert!(
        matches!(error, ChannelError::InvalidConfig { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("missing app_id"), "{error}");

    let no_secret = InstallationCredentials::new("cli_app_x", AppSecret::new(""));
    let error = fetcher
        .endpoint(&no_secret)
        .await
        .expect_err("缺 app_secret");
    assert!(error.to_string().contains("missing app_secret"), "{error}");
}

/// **错误路径不回显凭据**（HTTP 错误）：网关把请求回声进错误体也不该漏到我们的错误里。
#[tokio::test]
async fn an_http_error_status_never_echoes_the_secret() {
    let body = r#"{"error":"bad AppSecret secret-xyz for AppID cli_app_x"}"#;
    let (base, _request) = serve_once(http_response("401 Unauthorized", body)).await;
    let error = HttpEndpointFetcher::with_base_url(&base)
        .endpoint(&credentials())
        .await
        .expect_err("401 必须失败");
    let rendered = error.to_string();
    assert!(
        !rendered.contains("secret-xyz"),
        "错误回显了凭据：{rendered}"
    );
    assert!(rendered.contains("401"), "状态码要留下：{rendered}");
    // 连响应体的其余部分都不回显（上游会截 512 字节拼进错误；本仓只带状态码）。
    assert!(!rendered.contains("bad AppSecret"), "{rendered}");
}

/// **错误路径不回显凭据**（Lark 业务错误）：`code` 与 `msg` 要留下（运维靠它区分"应用类型
/// 不支持"与"凭据错"），但 `msg` 是不可信输入 ⇒ 先脱敏。
#[tokio::test]
async fn a_lark_business_error_surfaces_the_code_and_a_scrubbed_message() {
    let body = r#"{"code":403,"msg":"app_secret=secret-xyz is not allowed for cli_app_x"}"#;
    let (base, _request) = serve_once(http_response("200 OK", body)).await;
    let error = HttpEndpointFetcher::with_base_url(&base)
        .endpoint(&credentials())
        .await
        .expect_err("业务码非 0 必须失败");
    let rendered = error.to_string();
    assert!(
        !rendered.contains("secret-xyz"),
        "错误回显了凭据：{rendered}"
    );
    assert!(rendered.contains("403"), "业务码要留下：{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(
        rendered.contains("is not allowed"),
        "非秘密的 msg 部分要留下：{rendered}"
    );

    // 不回声凭据的正常文案原样透出（上游的 `app type not supported` 那条运维路径）。
    let body = r#"{"code":403,"msg":"app type not supported"}"#;
    let (base, _request) = serve_once(http_response("200 OK", body)).await;
    let error = HttpEndpointFetcher::with_base_url(&base)
        .endpoint(&credentials())
        .await
        .expect_err("业务码非 0 必须失败");
    assert!(
        error.to_string().contains("app type not supported"),
        "PersonalAgent 那条已知风险要靠这段文案看出来：{error}"
    );
}

/// 传输层失败（连不上）也只报类别：URL 与请求体都不进错误。
#[tokio::test]
async fn a_transport_failure_only_reports_the_class() {
    let fetcher = HttpEndpointFetcher::with_base_url("http://127.0.0.1:9");
    let error = fetcher
        .endpoint(&credentials())
        .await
        .expect_err("没有服务端在听");
    let rendered = error.to_string();
    assert!(rendered.contains("bootstrap request failed"), "{rendered}");
    assert!(!rendered.contains("secret-xyz"), "{rendered}");
    assert!(!rendered.contains("cli_app_x"), "{rendered}");
}

/// 把 `AppSecret` 用 `Write` 走一遍格式化（确认手写 `Debug` 覆盖了所有格式化路径）。
#[test]
fn no_formatting_path_reaches_the_plaintext_secret() {
    let request = build_bootstrap_request(&credentials());
    let mut rendered = Vec::new();
    write!(rendered, "{request:?}").expect("write");
    let rendered = String::from_utf8(rendered).expect("utf8");
    assert!(!rendered.contains("secret-xyz"), "{rendered}");
}
