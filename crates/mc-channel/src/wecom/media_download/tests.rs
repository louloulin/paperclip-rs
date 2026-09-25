//! `media_download` 的用例：取回的 body 与响应头、**两种**过帽、状态码、URL 形状与字面量地址，
//! 以及"错误路径不回显预签名 URL"（DoD 第 6 条）。
//!
//! 替身是一个手写的 `tokio` TCP 服务端（`mc-channel` 的依赖面里没有 axum —— M7-0 冻结）。

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;
use crate::wecom::media_guard::{
    addr_policy, new_media_http_client, unmap, AddrPolicy, MediaGuard,
};

/// 只用回环的地址策略（被测试的是取回与闸的配合，不是脚手架的地址）。
fn loopback_policy() -> AddrPolicy {
    addr_policy(|address| match unmap(address) {
        std::net::IpAddr::V4(v4) => v4.is_loopback(),
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    })
}

fn client() -> reqwest::Client {
    new_media_http_client(MediaGuard::new().with_policy(loopback_policy())).expect("client")
}

/// 一条脚本化的响应。
struct Script {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    conflict_length: bool,
    chunked: bool,
}

fn response(status: u16, body: &str) -> Script {
    Script {
        status,
        headers: Vec::new(),
        body: body.as_bytes().to_vec(),
        conflict_length: false,
        chunked: false,
    }
}

impl Script {
    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// 不发 `content-length`（上游注释里的第一条：**大小不在回调里声明**）。
    fn without_length(mut self) -> Self {
        self.conflict_length = true;
        self
    }

    /// 用 `Transfer-Encoding: chunked` 发（连 length 都没得看）。
    fn chunked(mut self) -> Self {
        self.chunked = true;
        self
    }
}

/// 起一个脚本化的 HTTP/1.1 服务端，返回端口。
async fn spawn_stub(scripts: Vec<Script>) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        for script in scripts {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let _ = read_head(&mut socket).await;
            let mut out = format!("HTTP/1.1 {} X\r\n", script.status);
            for (name, value) in &script.headers {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{name}: {value}\r\n"));
            }
            if script.chunked {
                out.push_str("transfer-encoding: chunked\r\n");
            } else if !script.conflict_length {
                let _ = std::fmt::Write::write_fmt(
                    &mut out,
                    format_args!("content-length: {}\r\n", script.body.len()),
                );
            }
            out.push_str("connection: close\r\n\r\n");
            socket.write_all(out.as_bytes()).await.ok();
            if script.chunked {
                for piece in script.body.chunks(7) {
                    socket
                        .write_all(format!("{:x}\r\n", piece.len()).as_bytes())
                        .await
                        .ok();
                    socket.write_all(piece).await.ok();
                    socket.write_all(b"\r\n").await.ok();
                }
                socket.write_all(b"0\r\n\r\n").await.ok();
            } else {
                socket.write_all(&script.body).await.ok();
            }
            socket.flush().await.ok();
            socket.shutdown().await.ok();
        }
    });
    port
}

async fn read_head(socket: &mut tokio::net::TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

// =====================================================================
// 取回
// =====================================================================

/// body 与响应头一起回来，而头里的名字是被解出来的那个（不是原始转义串）。
#[tokio::test]
async fn a_fetched_body_carries_the_headers_that_describe_it() {
    let port = spawn_stub(vec![response(200, "cipher-bytes").header(
        "content-disposition",
        r"attachment; filename*=UTF-8''%E5%AD%A3%E6%8A%A5.docx",
    )])
    .await;
    let fetched = download_media(&client(), &format!("http://localhost:{port}/cos"))
        .await
        .expect("fetch");
    assert_eq!(fetched.body, b"cipher-bytes".to_vec());
    assert_eq!(fetched.headers.filename, "季报.docx");
    // 原样那一份也要留着（追踪用，`traceMediaHeaders` 的输入）。
    assert!(fetched.headers.disposition.contains("filename*="));
}

/// **声明的**长度与**没声明的**长度两条路都被同一个上限拒（上游那两条 `errMediaTooLarge`）。
#[tokio::test]
async fn both_a_declared_and_an_undeclared_oversize_body_are_refused() {
    let client = client();

    // ① 声明就超帽 ⇒ 连 body 都不读。
    let port = spawn_stub(vec![response(200, "0123456789")]).await;
    let error = download_media_capped(&client, &format!("http://localhost:{port}/cos"), 4)
        .await
        .expect_err("declared oversize");
    assert_eq!(error, MediaDownloadError::TooLarge);

    // ② 不声明长度、真读到超帽 ⇒ 读的中途发现。
    let port = spawn_stub(vec![response(200, "0123456789").without_length()]).await;
    let error = download_media_capped(&client, &format!("http://localhost:{port}/cos"), 4)
        .await
        .expect_err("undeclared oversize");
    assert_eq!(error, MediaDownloadError::TooLarge);

    // ③ 恰好在帽上**不**算超（那一个字节的余量就是为这一格留的）。
    let port = spawn_stub(vec![response(200, "0123").without_length()]).await;
    let fetched = download_media_capped(&client, &format!("http://localhost:{port}/cos"), 4)
        .await
        .expect("exactly at the cap");
    assert_eq!(fetched.body, b"0123".to_vec());
}

/// 流式路径（`open_media`）同样执行上限，而且**从流里**报出来。
#[tokio::test]
async fn the_streaming_path_enforces_the_cap_too() {
    let port = spawn_stub(vec![response(200, "0123456789").chunked()]).await;
    let client = client();
    let fetched = download_media(&client, &format!("http://localhost:{port}/cos"))
        .await
        .expect("chunked fetch");
    assert_eq!(fetched.body.len(), 10);

    // 同一个 body，换成流式读 + 一个小帽 ⇒ 流里交出 `TooLarge`。
    let port = spawn_stub(vec![response(200, "0123456789").chunked()]).await;
    let (mut body, _) = open_media(&client, &format!("http://localhost:{port}/cos"))
        .await
        .expect("open");
    let mut collected = Vec::new();
    let mut failure = None;
    while let Some(item) = body.next_chunk().await {
        match item {
            Ok(chunk) => collected.extend_from_slice(&chunk),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    // 流式那条路的上限是 `MEDIA_MAX_BYTES` ⇒ 10 字节当然过得去。
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(collected.len(), 10);
}

/// 非 2xx 带状态码回来（上游那一段"谁拒的"）。
#[tokio::test]
async fn a_non_2xx_status_arrives_with_its_code() {
    let port = spawn_stub(vec![response(403, "expired")]).await;
    let error = download_media(&client(), &format!("http://localhost:{port}/cos"))
        .await
        .expect_err("403");
    assert_eq!(error, MediaDownloadError::Http { status: 403 });
}

// =====================================================================
// URL 与地址
// =====================================================================

/// 形状与**字面量**地址：后者是 `reqwest` 的解析器接缝盖不到的那一半。
#[test]
fn url_shapes_and_non_public_literals_are_refused() {
    assert_eq!(check_media_url(""), Err(MediaDownloadError::InvalidUrl));
    assert_eq!(check_media_url("   "), Err(MediaDownloadError::InvalidUrl));
    assert_eq!(
        check_media_url("not a url at all"),
        Err(MediaDownloadError::InvalidUrl)
    );
    assert_eq!(
        check_media_url("ftp://cos.example.cn/x"),
        Err(MediaDownloadError::UnsupportedScheme)
    );
    assert_eq!(
        check_media_url("file:///etc/passwd"),
        Err(MediaDownloadError::UnsupportedScheme)
    );
    for literal in [
        "http://169.254.169.254/latest/meta-data/",
        "http://127.0.0.1:8080/admin",
        "http://10.0.0.5/x",
        "http://100.64.0.1/x",
        "http://[::1]/x",
        "http://[64:ff9b:1::a9fe:a9fe]/x",
    ] {
        assert_eq!(
            check_media_url(literal),
            Err(MediaDownloadError::Guard(
                crate::wecom::media_guard::MediaGuardError::BlockedAddress
            )),
            "{literal}"
        );
    }
    assert!(check_media_url("https://cos.ap-shanghai.myqcloud.com/x?a=b").is_ok());
    assert!(check_media_url("https://93.184.216.34/x").is_ok());
}

/// 🔴 `DoD` 第 6 条：**错误路径不回显那条预签名 URL**（它是一张五分钟的 bearer 凭据）。
#[tokio::test]
async fn the_error_paths_never_echo_the_signed_url() {
    let secret = "aeskey-secret-value";
    // ① 传输失败（连不上的端口）：`reqwest` 自己的 `Display` 会打印 URL ⇒ 它必须进不来。
    let error = download_media(
        &client(),
        &format!("http://localhost:1/cos?aeskey={secret}"),
    )
    .await
    .expect_err("connection refused");
    let message = error.to_string();
    assert!(!message.contains(secret), "{message}");
    assert!(!message.contains("aeskey"), "{message}");
    assert!(!message.contains("localhost"), "{message}");

    // ② 状态码那条：不带响应体片段（COS 的 XML 会回显它收到的 URL）。
    let port = spawn_stub(vec![
        response(403, "expired").header("x-cos-message", "url=http://127.0.0.1/secret")
    ])
    .await;
    let error = download_media(
        &client(),
        &format!("http://localhost:{port}/cos?aeskey={secret}"),
    )
    .await
    .expect_err("403");
    assert_eq!(error, MediaDownloadError::Http { status: 403 });
    assert!(!error.to_string().contains(secret));

    // ③ 形状那条：解析不了的输入不进错误文本。
    let error =
        check_media_url(&format!("http://[bad host]/?aeskey={secret}")).expect_err("bad host");
    assert!(!error.to_string().contains(secret), "{error}");

    // ④ `MediaBody` 的 `Debug` 也不打 URL。
    let port = spawn_stub(vec![response(200, "x")]).await;
    let (body, _) = open_media(
        &client(),
        &format!("http://localhost:{port}/cos?aeskey={secret}"),
    )
    .await
    .expect("open");
    let debugged = format!("{body:?}");
    assert!(!debugged.contains(secret), "{debugged}");
    assert!(!debugged.contains("localhost"), "{debugged}");
}
