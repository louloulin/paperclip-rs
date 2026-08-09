//! R520 — pc-storage S3 provider 真实集成测试
//!
//! 启动一个进程内 tokio TcpListener 模拟 S3 兼容端点，验证
//! `S3Storage` 端到端：
//! - put_object 携带 SigV4 Authorization header + body hash
//! - get_object 返回字节 + 404 → NotFound
//! - delete_object 幂等
//! - list_prefix 解析 ListObjectsV2 XML
//! - presign_get 生成合法 URL
//! - 错误映射（无 creds → NotConfigured）
//!
//! 故意不引入 hyper/tower：mock server 用 raw TCP 读取请求行 + headers
//! 即可验证 S3 client 的发送格式 + 解析响应格式。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use pc_storage::provider::StorageProvider;
use pc_storage::types::{ObjectKey, StorageLocation};
use pc_storage::{S3Storage, StorageError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Default, Debug, Clone)]
struct CapturedRequest {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Bytes,
}

#[derive(Default)]
struct MockS3State {
    objects: HashMap<String, Bytes>,
    requests: Vec<CapturedRequest>,
    /// 强制返回的下个状态码（一次）
    next_status: Option<u16>,
    next_body: Option<String>,
}

type SharedState = Arc<Mutex<MockS3State>>;

fn extract_header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

/// 解析 HTTP/1.1 请求到 (method, path, query, headers, body)。
fn parse_request(buf: &[u8]) -> Option<(String, String, String, Vec<(String, String)>, Bytes)> {
    let s = std::str::from_utf8(buf).ok()?;
    let mut parts = s.splitn(2, "\r\n\r\n");
    let head = parts.next()?;
    let body_str = parts.next().unwrap_or("");
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut rl = request_line.split_whitespace();
    let method = rl.next()?.to_string();
    let target = rl.next()?.to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let mut headers = Vec::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    // body 长度取 Content-Length
    let body_len = extract_header(&headers, "content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let body = if body_str.len() >= body_len {
        Bytes::copy_from_slice(&body_str.as_bytes()[..body_len.min(body_str.len())])
    } else {
        Bytes::new()
    };
    Some((method, path, query, headers, body))
}

fn make_response(status: u16, reason: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    resp.extend_from_slice(body);
    resp
}

async fn handle_conn(mut conn: tokio::net::TcpStream, state: SharedState) {
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = Vec::new();
    loop {
        match conn.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                total.extend_from_slice(&buf[..n]);
                if let Some((method, path, query, headers, body)) = parse_request(&total) {
                    // PUT 完整 body 必须读够：若 Content-Length > body 实际长度继续读
                    let content_length = extract_header(&headers, "content-length")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    if method == "PUT" && body.len() < content_length {
                        continue;
                    }
                    let body_clone = body.clone();
                    state.lock().unwrap().requests.push(CapturedRequest {
                        method: method.clone(),
                        path: path.clone(),
                        query: query.clone(),
                        headers: headers.clone(),
                        body: body_clone,
                    });

                    let stripped = path
                        .strip_prefix("/mock-bucket")
                        .map(|s| s.trim_start_matches('/').to_string())
                        .unwrap_or_else(|| path.clone());
                    let (status, body_resp) = {
                        let mut s = state.lock().unwrap();
                        if let Some(forced) = s.next_status.take() {
                            let b = s.next_body.take().unwrap_or_default();
                            (forced, b.into_bytes())
                        } else if method == "PUT" {
                            s.objects.insert(stripped, body);
                            (200u16, Vec::new())
                        } else if method == "DELETE" {
                            s.objects.remove(&stripped);
                            (204u16, Vec::new())
                        } else if method == "GET" && query.contains("list-type=2") {
                            let prefix = query
                                .split('&')
                                .find_map(|kv| kv.strip_prefix("prefix="))
                                .map(|p| url_decode(p))
                                .unwrap_or_default();
                            let matching: Vec<String> = s
                                .objects
                                .keys()
                                .filter(|k| k.starts_with(&prefix))
                                .cloned()
                                .collect();
                            let xml = format!(
                                r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult><Name>mock</Name><Prefix>{prefix}</Prefix><KeyCount>{n}</KeyCount>{items}</ListBucketResult>"#,
                                n = matching.len(),
                                items = matching
                                    .iter()
                                    .map(|k| format!("<Contents><Key>{k}</Key></Contents>"))
                                    .collect::<String>()
                            );
                            (200u16, xml.into_bytes())
                        } else if method == "GET" {
                            match s.objects.get(&stripped) {
                                Some(stored) => (200u16, stored.to_vec()),
                                None => (404u16, b"<Error><Code>NoSuchKey</Code></Error>".to_vec()),
                            }
                        } else {
                            (200u16, Vec::new())
                        }
                    };
                    let reason = match status {
                        200 => "OK",
                        204 => "No Content",
                        404 => "Not Found",
                        500 => "Internal Server Error",
                        _ => "OK",
                    };
                    let ct = if query.contains("list-type=2") {
                        "application/xml"
                    } else if status == 404 {
                        "application/xml"
                    } else {
                        "application/octet-stream"
                    };
                    let resp = make_response(status, reason, ct, &body_resp);
                    let _ = conn.write_all(&resp).await;
                    let _ = conn.flush().await;
                    return;
                }
            }
            Err(_) => break,
        }
    }
}

fn url_decode(s: &str) -> String {
    // 最小化解码（%+hex）
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00"),
                16,
            ) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn start_mock_s3() -> (SharedState, SocketAddr, oneshot::Sender<()>) {
    let state: SharedState = Arc::new(Mutex::new(MockS3State::default()));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, mut rx) = oneshot::channel::<()>();
    let state_clone = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                accept = listener.accept() => {
                    if let Ok((stream, _)) = accept {
                        let st = state_clone.clone();
                        tokio::spawn(async move { handle_conn(stream, st).await });
                    } else {
                        break;
                    }
                }
                _ = &mut rx => break,
            }
        }
    });
    (state, addr, tx)
}

fn make_s3(endpoint: &str) -> S3Storage {
    S3Storage::new("us-east-1", "mock-bucket")
        .with_credentials(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        )
        .with_endpoint(endpoint)
        .path_style(true)
}

fn loc(bucket: &str, key: &str) -> StorageLocation {
    StorageLocation {
        bucket: bucket.into(),
        key: ObjectKey::new(key),
    }
}

// --- tests ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r520_put_object_writes_with_sigv4_and_sha256() {
    let (state, addr, _shutdown) = start_mock_s3().await;
    let endpoint = format!("http://{addr}");
    let s3 = make_s3(&endpoint);

    let payload = Bytes::from_static(b"hello-r520");
    let meta = s3
        .put_object(
            &loc("mock-bucket", "alpha.txt"),
            payload.clone(),
            Some("text/plain"),
        )
        .await
        .expect("put");
    assert_eq!(meta.size, payload.len() as u64);
    assert_eq!(meta.content_type.as_deref(), Some("text/plain"));
    assert!(meta.content_sha256.is_some());
    assert_eq!(meta.content_sha256.as_ref().unwrap().len(), 64);

    let captured = state.lock().unwrap().requests.clone();
    assert_eq!(captured.len(), 1);
    let req = &captured[0];
    assert_eq!(req.method, "PUT");
    assert_eq!(req.path, "/mock-bucket/alpha.txt");
    let auth = extract_header(&req.headers, "authorization").expect("auth header");
    assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/"));
    assert!(auth.contains("SignedHeaders="));
    assert!(auth.contains("Signature="));
    assert!(extract_header(&req.headers, "x-amz-date").is_some());
    assert!(extract_header(&req.headers, "x-amz-content-sha256").is_some());
    assert_eq!(req.body, payload);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r520_get_object_round_trip() {
    let (_state, addr, _shutdown) = start_mock_s3().await;
    let s3 = make_s3(&format!("http://{addr}"));
    let payload = Bytes::from_static(b"data-roundtrip");
    s3.put_object(&loc("mock-bucket", "beta.bin"), payload.clone(), None)
        .await
        .expect("put");
    let got = s3
        .get_object(&loc("mock-bucket", "beta.bin"))
        .await
        .expect("get");
    assert_eq!(got, payload);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r520_get_object_not_found() {
    let (_state, addr, _shutdown) = start_mock_s3().await;
    let s3 = make_s3(&format!("http://{addr}"));
    let err = s3
        .get_object(&loc("mock-bucket", "missing.bin"))
        .await
        .expect_err("must be NotFound");
    assert!(matches!(err, StorageError::NotFound(_)), "got {err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r520_delete_object_idempotent() {
    let (_state, addr, _shutdown) = start_mock_s3().await;
    let s3 = make_s3(&format!("http://{addr}"));
    s3.delete_object(&loc("mock-bucket", "nope.bin"))
        .await
        .expect("delete idempotent");
    s3.put_object(
        &loc("mock-bucket", "ok.bin"),
        Bytes::from_static(b"x"),
        None,
    )
    .await
    .expect("put");
    s3.delete_object(&loc("mock-bucket", "ok.bin"))
        .await
        .expect("delete");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r520_list_prefix_filters_by_prefix() {
    let (_state, addr, _shutdown) = start_mock_s3().await;
    let s3 = make_s3(&format!("http://{addr}"));
    s3.put_object(
        &loc("mock-bucket", "logs/2024/01.txt"),
        Bytes::from_static(b"a"),
        None,
    )
    .await
    .unwrap();
    s3.put_object(
        &loc("mock-bucket", "logs/2024/02.txt"),
        Bytes::from_static(b"b"),
        None,
    )
    .await
    .unwrap();
    s3.put_object(
        &loc("mock-bucket", "logs/2025/01.txt"),
        Bytes::from_static(b"c"),
        None,
    )
    .await
    .unwrap();
    s3.put_object(
        &loc("mock-bucket", "other/x.txt"),
        Bytes::from_static(b"d"),
        None,
    )
    .await
    .unwrap();
    let keys = s3
        .list_prefix("mock-bucket", "logs/2024/")
        .await
        .expect("list");
    let names: Vec<String> = keys.iter().map(|k| k.0.clone()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"logs/2024/01.txt".to_string()));
    assert!(names.contains(&"logs/2024/02.txt".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r520_presign_get_url_format() {
    let (_state, addr, _shutdown) = start_mock_s3().await;
    let s3 = make_s3(&format!("http://{addr}"));
    let presigned = s3
        .presign_get(
            &loc("mock-bucket", "x.bin"),
            std::time::Duration::from_secs(600),
        )
        .await
        .expect("presign");
    assert!(presigned.url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
    // AWS SigV4 presign URL-encodes "/" in credential scope.
    assert!(
        presigned
            .url
            .contains("X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F")
            || presigned
                .url
                .contains("X-Amz-Credential=AKIAIOSFODNN7EXAMPLE/")
    );
    assert!(presigned.url.contains("X-Amz-Date="));
    assert!(presigned.url.contains("X-Amz-Expires=600"));
    assert!(presigned.url.contains("X-Amz-Signature="));
    assert!(presigned.url.contains("X-Amz-SignedHeaders=host"));
    assert!(presigned.url.contains("/mock-bucket/x.bin"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r520_health_requires_credentials() {
    let (_state, addr, _shutdown) = start_mock_s3().await;
    let s3 = S3Storage::new("us-east-1", "mock-bucket").with_endpoint(&format!("http://{addr}"));
    let err = s3.health().await.expect_err("no creds");
    assert!(matches!(err, StorageError::NotConfigured(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r520_put_object_requires_credentials() {
    let (_state, addr, _shutdown) = start_mock_s3().await;
    let s3 = S3Storage::new("us-east-1", "mock-bucket").with_endpoint(&format!("http://{addr}"));
    let err = s3
        .put_object(&loc("mock-bucket", "x"), Bytes::from_static(b"x"), None)
        .await
        .expect_err("no creds");
    assert!(matches!(err, StorageError::NotConfigured(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r520_health_ok_with_credentials() {
    let (_state, addr, _shutdown) = start_mock_s3().await;
    let s3 = make_s3(&format!("http://{addr}"));
    s3.health().await.expect("health");
}
