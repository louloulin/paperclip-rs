//! 响应压缩中间件 — 等价于 Node `middleware/api-compression.ts`。
//!
//! 语义对齐：
//! - 仅协商 `gzip` / `deflate`（q 值选择，`*` 通配回退，q=0 排除）
//! - 仅压缩 JSON content-type（`application/json` 或 `+json`）
//! - 响应体 ≥ 1024 字节才压缩
//! - 跳过 `Cache-Control: no-transform`、流式响应头、204/304、已编码响应
//! - 压缩后追加 `Vary: Accept-Encoding`、弱化强 ETag、移除 Content-MD5
//! - 压缩失败 best-effort 回退原始 body（不破坏健康响应）

use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{
        header::{
            HeaderMap, HeaderValue, ACCEPT_ENCODING, ACCEPT_RANGES, CACHE_CONTROL,
            CONTENT_DISPOSITION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE,
            ETAG, VARY,
        },
        StatusCode,
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use flate2::{write::GzEncoder, write::ZlibEncoder, Compression};
use std::io::Write;
use tracing::warn;

/// Node 上游 `API_COMPRESSION_THRESHOLD_BYTES = 1024`。
pub const API_COMPRESSION_THRESHOLD_BYTES: usize = 1024;

/// 缓冲上限：Node 无上限，这里给安全上限防止恶意大响应耗尽内存。
pub const MAX_BUFFERED_BODY_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedEncoding {
    Gzip,
    Deflate,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncodingPreference {
    pub encoding: String,
    pub q: f64,
}

/// 解析 `Accept-Encoding` 头（Node 对数组值 join(",")，Rust HeaderMap 天然支持
/// 多值，这里同样以逗号拼接后统一解析）。
pub fn parse_accept_encoding(value: Option<&str>) -> Vec<EncodingPreference> {
    let raw = value.unwrap_or("").trim();
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(",")
        .filter_map(|part| {
            let mut pieces = part.trim().split(";");
            let encoding = pieces.next().unwrap_or("").trim().to_lowercase();
            if encoding.is_empty() {
                return None;
            }
            let mut q = 1.0;
            for param in pieces {
                let param = param.trim();
                if param.to_lowercase().starts_with("q=") {
                    q = param[2..].parse::<f64>().unwrap_or(0.0);
                    if !q.is_finite() {
                        q = 0.0;
                    }
                    break;
                }
            }
            Some(EncodingPreference { encoding, q })
        })
        .collect()
}

/// 选择最终编码：过滤 q<=0；gzip 与 deflate 各取最大 q（含 `*` 通配回退）；
/// 平局时 gzip 优先（与 Node `gzipQ >= deflateQ ? gzip : deflate` 一致）。
pub fn select_encoding(value: Option<&str>) -> Option<SupportedEncoding> {
    let preferences = parse_accept_encoding(value);
    let find_q = |encoding: &str| {
        preferences
            .iter()
            .find(|e| e.encoding == encoding)
            .map(|e| e.q)
            .or_else(|| preferences.iter().find(|e| e.encoding == "*").map(|e| e.q))
            .unwrap_or(0.0)
    };
    let gzip_q = find_q("gzip");
    let deflate_q = find_q("deflate");
    if gzip_q <= 0.0 && deflate_q <= 0.0 {
        return None;
    }
    Some(if gzip_q >= deflate_q {
        SupportedEncoding::Gzip
    } else {
        SupportedEncoding::Deflate
    })
}

pub fn is_json_content_type(value: Option<&str>) -> bool {
    let content_type = value.unwrap_or("").to_lowercase();
    content_type.contains("application/json") || content_type.contains("+json")
}

/// `Cache-Control: no-transform` 判定（word boundary 匹配，与 Node
/// `\bno-transform\b` 等价——连字符是 non-word 字符，不能按 split 拆词）。
pub fn should_skip_for_cache_control(value: Option<&str>) -> bool {
    let v = value.unwrap_or("").to_lowercase();
    let bytes = v.as_bytes();
    let needle = b"no-transform";
    let is_word = |b: u8| b.is_ascii_alphanumeric();
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let before_ok = i == 0 || !is_word(bytes[i - 1]);
            let after_ok = i + needle.len() == bytes.len() || !is_word(bytes[i + needle.len()]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

pub fn status_allows_body(status: StatusCode) -> bool {
    status.as_u16() != 204 && status.as_u16() != 304 && status.as_u16() >= 200
}

pub fn should_skip_for_streamed_response(headers: &HeaderMap) -> bool {
    headers.contains_key(CONTENT_DISPOSITION)
        || headers.contains_key(ACCEPT_RANGES)
        || headers.contains_key(CONTENT_RANGE)
}

/// write 阶段直通判定（Node `shouldPassthroughWrite`，不含 headersSent——
/// axum 响应在写入前头已确定）。
pub fn should_passthrough_write(status: StatusCode, headers: &HeaderMap) -> bool {
    let already_encoded = headers
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_lowercase() != "identity");
    let content_type = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok());
    already_encoded
        || !status_allows_body(status)
        || should_skip_for_cache_control(headers.get(CACHE_CONTROL).and_then(|v| v.to_str().ok()))
        || should_skip_for_streamed_response(headers)
        || content_type.is_none()
        || !is_json_content_type(content_type)
}

/// 弱化 ETag（Node `weakenStrongEtag`）：`"abc"` → `W/"abc"`，已弱化不变。
pub fn weaken_etag(value: &str) -> String {
    if value.trim_start().starts_with("W/") || value.trim_start().starts_with("w/") {
        value.to_string()
    } else {
        format!("W/{value}")
    }
}

/// 追加 `Vary` 值（Node `res.vary` 语义：已存在则不重复）。
fn append_vary(headers: &mut HeaderMap, value: &str) {
    let exists = headers
        .get_all(VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(","))
        .any(|v| v.trim().eq_ignore_ascii_case(value));
    if !exists {
        headers.append(
            VARY,
            HeaderValue::from_str(value).expect("vary value is static"),
        );
    }
}

fn compress_bytes(encoding: SupportedEncoding, body: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    match encoding {
        SupportedEncoding::Gzip => {
            let mut enc = GzEncoder::new(&mut out, Compression::default());
            enc.write_all(body)?;
            enc.finish()?;
        }
        SupportedEncoding::Deflate => {
            // Node `zlib.deflate` 输出 zlib 封装（RFC 1950），HTTP `deflate`
            // 编码在 RFC 9110 中即指 zlib 格式。
            let mut enc = ZlibEncoder::new(&mut out, Compression::default());
            enc.write_all(body)?;
            enc.finish()?;
        }
    }
    Ok(out)
}

/// 压缩中间件（from_fn 形式，需挂在路由最内层以看到最终响应头）。
pub async fn compression_layer(req: Request, next: Next) -> Response {
    if req.method() == axum::http::Method::HEAD {
        return next.run(req).await;
    }
    let accept_encoding = req
        .headers()
        .get(ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok());
    let Some(selected) = select_encoding(accept_encoding) else {
        return next.run(req).await;
    };
    let response = next.run(req).await;
    let status = response.status();
    let headers = response.headers().clone();
    if should_passthrough_write(status, &headers) {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let body_bytes = match to_bytes(body, MAX_BUFFERED_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "compression: response body read failed; skipping");
            return Response::from_parts(parts, Body::empty());
        }
    };
    if body_bytes.len() < API_COMPRESSION_THRESHOLD_BYTES {
        return Response::from_parts(parts, Body::from(body_bytes));
    }
    let compressed = match compress_bytes(selected, &body_bytes) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "compression: compress failed; sending original body");
            return Response::from_parts(parts, Body::from(body_bytes));
        }
    };
    append_vary(&mut parts.headers, "Accept-Encoding");
    parts.headers.insert(
        CONTENT_ENCODING,
        HeaderValue::from_str(match selected {
            SupportedEncoding::Gzip => "gzip",
            SupportedEncoding::Deflate => "deflate",
        })
        .expect("static encoding value"),
    );
    parts.headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&compressed.len().to_string()).expect("length is numeric"),
    );
    if let Some(etag) = parts.headers.get(ETAG).and_then(|v| v.to_str().ok()) {
        let weakened = weaken_etag(etag);
        if let Ok(v) = HeaderValue::from_str(&weakened) {
            parts.headers.insert(ETAG, v);
        }
    }
    parts.headers.remove("content-md5");
    Response::from_parts(parts, Body::from(compressed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body as AxBody, routing::get, Router};
    use flate2::read::{GzDecoder, ZlibDecoder};
    use std::io::Read;
    use tower::ServiceExt;

    fn json_body(prefix: &str, n: usize) -> String {
        format!("{{\"prefix\":\"{prefix}\",\"data\":\"{}\"}}", "x".repeat(n))
    }

    fn app() -> Router {
        Router::new()
            .route(
                "/big",
                get(|| async {
                    (
                        [(CONTENT_TYPE, "application/json; charset=utf-8")],
                        json_body("big", 4096),
                    )
                }),
            )
            .route(
                "/small",
                get(|| async {
                    (
                        [(CONTENT_TYPE, "application/json; charset=utf-8")],
                        json_body("small", 64),
                    )
                }),
            )
            .route("/text", get(|| async { "plain text ".repeat(512) }))
            .route(
                "/nocache",
                get(|| async {
                    (
                        [
                            (CACHE_CONTROL, "public, no-transform".to_string()),
                            (CONTENT_TYPE, "application/json; charset=utf-8".to_string()),
                        ],
                        json_body("nc", 4096),
                    )
                }),
            )
            .route(
                "/etag",
                get(|| async {
                    (
                        [
                            (ETAG, "\"abc123\"".to_string()),
                            (CONTENT_TYPE, "application/json; charset=utf-8".to_string()),
                        ],
                        json_body("etag", 4096),
                    )
                }),
            )
            .route("/empty", get(|| async { StatusCode::NO_CONTENT }))
            .layer(axum::middleware::from_fn(compression_layer))
    }

    async fn get_bytes(path: &str, accept: &str) -> Response {
        let req = axum::http::Request::builder()
            .uri(path)
            .header(ACCEPT_ENCODING, accept)
            .body(AxBody::empty())
            .unwrap();
        app().oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn gzip_compresses_big_json() {
        let res = get_bytes("/big", "gzip").await;
        assert_eq!(res.headers()[CONTENT_ENCODING], "gzip");
        assert!(res
            .headers()
            .get_all(VARY)
            .iter()
            .any(|v| v.to_str().unwrap().contains("Accept-Encoding")));
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let mut dec = GzDecoder::new(&body[..]);
        let mut out = String::new();
        dec.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("{\"prefix\":\"big\""));
    }

    #[tokio::test]
    async fn deflate_compresses_when_only_deflate_supported() {
        let res = get_bytes("/big", "deflate").await;
        assert_eq!(res.headers()[CONTENT_ENCODING], "deflate");
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let mut dec = ZlibDecoder::new(&body[..]);
        let mut out = String::new();
        dec.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("{\"prefix\":\"big\""));
    }

    #[tokio::test]
    async fn gzip_preferred_over_deflate_on_tie() {
        let res = get_bytes("/big", "gzip, deflate").await;
        assert_eq!(res.headers()[CONTENT_ENCODING], "gzip");
    }

    #[tokio::test]
    async fn qzero_excludes_encoding() {
        let res = get_bytes("/big", "gzip;q=0, deflate").await;
        assert_eq!(res.headers()[CONTENT_ENCODING], "deflate");
    }

    #[tokio::test]
    async fn wildcard_falls_back_to_gzip() {
        let res = get_bytes("/big", "*").await;
        assert_eq!(res.headers()[CONTENT_ENCODING], "gzip");
    }

    #[tokio::test]
    async fn small_body_not_compressed() {
        let res = get_bytes("/small", "gzip").await;
        assert!(res.headers().get(CONTENT_ENCODING).is_none());
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        assert!(body.starts_with(b"{\"prefix\":\"small\""));
    }

    #[tokio::test]
    async fn text_body_not_compressed() {
        let res = get_bytes("/text", "gzip").await;
        assert!(res.headers().get(CONTENT_ENCODING).is_none());
    }

    #[tokio::test]
    async fn no_transform_skips_compression() {
        let res = get_bytes("/nocache", "gzip").await;
        assert!(res.headers().get(CONTENT_ENCODING).is_none());
    }

    #[tokio::test]
    async fn strong_etag_is_weakened() {
        let res = get_bytes("/etag", "gzip").await;
        assert_eq!(res.headers()[ETAG], "W/\"abc123\"");
    }

    #[tokio::test]
    async fn no_content_status_skipped() {
        let res = get_bytes("/empty", "gzip").await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(res.headers().get(CONTENT_ENCODING).is_none());
    }

    #[tokio::test]
    async fn head_requests_never_compressed() {
        let req = axum::http::Request::builder()
            .uri("/big")
            .method("HEAD")
            .header(ACCEPT_ENCODING, "gzip")
            .body(AxBody::empty())
            .unwrap();
        let res = app().oneshot(req).await.unwrap();
        assert!(res.headers().get(CONTENT_ENCODING).is_none());
    }

    #[test]
    fn parses_q_values() {
        let prefs = parse_accept_encoding(Some("gzip;q=0.5, deflate;q=0.8, br;q=0"));
        assert_eq!(prefs.len(), 3);
        assert_eq!(prefs[0].q, 0.5);
        assert_eq!(prefs[1].q, 0.8);
        assert_eq!(prefs[2].q, 0.0);
    }

    #[test]
    fn empty_accept_encoding_yields_none() {
        assert!(select_encoding(None).is_none());
        assert!(select_encoding(Some("")).is_none());
    }

    #[test]
    fn invalid_q_treated_as_zero() {
        let prefs = parse_accept_encoding(Some("gzip;q=abc"));
        assert_eq!(prefs[0].q, 0.0);
    }

    #[test]
    fn status_rules() {
        assert!(status_allows_body(StatusCode::OK));
        assert!(status_allows_body(StatusCode::BAD_REQUEST));
        assert!(!status_allows_body(StatusCode::NO_CONTENT));
        assert!(!status_allows_body(StatusCode::NOT_MODIFIED));
        assert!(!status_allows_body(StatusCode::from_u16(199).unwrap()));
    }

    #[test]
    fn etag_weakening_rules() {
        assert_eq!(weaken_etag("\"x\""), "W/\"x\"");
        assert_eq!(weaken_etag("W/\"x\""), "W/\"x\"");
        assert_eq!(weaken_etag("w/\"x\""), "w/\"x\"");
    }
}
