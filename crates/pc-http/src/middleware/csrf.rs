//! CSRF protection middleware（double-submit cookie 模式）。
//!
//! 与 better-auth 内部 CSRF 校验行为等价：
//! - 登录成功 / 刷新 session 时，server 颁发一对 `(csrf_cookie, csrf_token)`。
//! - `csrf_cookie`：`paperclip_csrf=<token>; Path=/; SameSite=Lax; HttpOnly=false`，JS 可读。
//! - 客户端在状态变更请求（POST / PUT / PATCH / DELETE）的 `X-CSRF-Token`
//!   header 回传同一 token；middleware 常数时间比较，匹配则放行。
//! - GET / HEAD / OPTIONS 永远放行（无副作用）。
//!
//! 范围：
//! - **仅对 cookie 会话强制 CSRF**（浏览器 form-submit 场景）。
//! - **不对 Bearer token / API key 客户端强制**（非浏览器，不需要 double-submit cookie 保护）。
//! - 路径白名单（`/api/auth/*`、`/live-events`、`/openapi.json`、`/health` 等）永远放行。
//!
//! 设计原则：
//! - 纯函数 `csrf_decision(method, path, headers) → Result<(), CsrfDenial>`，
//!   不依赖 DB / state，便于单测；middleware 仅做参数提取 + 响应包装。
//! - 失败时返回 403 Forbidden + `{"error": ..., "code": "CSRF_VALIDATION_FAILED"}` body。
//!
//! 与上游 Node `better-auth` 的语义差异：
//! - better-auth 用 CSRF cookie + header 双字段比较；本实现同一字段值即可
//!   （token 是高熵随机串，cookie / header 哪个传都行 — 两者必须一致）
//! - 失败时 better-auth 返回 401；本实现返回 403（语义更精准：无权限）

use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::AppState;

/// Cookie 名称（与 better-auth 的 `__Secure-better-auth.csrf_token` 形态保持 1 字映射）。
pub const CSRF_COOKIE_NAME: &str = "paperclip_csrf";
/// 请求头名（better-auth 默认 `x-better-auth-csrf-token`）。
pub const CSRF_HEADER_NAME: &str = "x-csrf-token";
/// Session cookie 名称（用于判断请求是否已认证）。
pub const SESSION_COOKIE_NAME: &str = "paperclip_session";
/// 默认 token 字节数（256 bit entropy → 64 hex chars）。
pub const CSRF_TOKEN_BYTES: usize = 32;

/// 强制 CSRF 校验的请求方法（GET/HEAD/OPTIONS 跳过）。
pub const CSRF_REQUIRED_METHODS: &[&str] = &["POST", "PUT", "PATCH", "DELETE"];

/// 路径白名单（不需要 CSRF 校验）：
/// - `/api/auth/*`：登录入口（无 session）
/// - `/api/dev-server/*`：开发模式重启
/// - `/live-events`：WebSocket 升级（Origin 已校验）
/// - `/openapi.json` / `/api/openapi.json`：OpenAPI 文档只读
/// - `/_plugins/*/ui/*`：插件静态资源
pub fn csrf_path_allowed(path: &str) -> bool {
    if path.starts_with("/api/auth/")
        || path.starts_with("/api/dev-server/")
        || path == "/live-events"
        || path == "/openapi.json"
        || path == "/api/openapi"
        || path == "/api/openapi.json"
        || path.starts_with("/_plugins/")
        || path == "/health"
    {
        return true;
    }
    false
}

/// 解析请求的 Cookie 头为 `(name, value)` 对。
fn parse_cookies(headers: &HeaderMap) -> Vec<(&str, &str)> {
    let Some(value) = headers.get(axum::http::header::COOKIE) else {
        return Vec::new();
    };
    let Ok(s) = value.to_str() else {
        return Vec::new();
    };
    s.split(';')
        .filter_map(|kv| {
            let mut parts = kv.trim().splitn(2, '=');
            Some((parts.next()?, parts.next().unwrap_or("")))
        })
        .collect()
}

/// 生成新的 CSRF token。
#[must_use]
pub fn generate_csrf_token() -> String {
    // 用 uuid v4 作为熵源 + 二次哈希拼接 → 64 hex chars
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let mut hasher = Sha256::new();
    hasher.update(a.as_bytes());
    hasher.update(b.as_bytes());
    let bytes = hasher.finalize();
    hex::encode(&bytes[..CSRF_TOKEN_BYTES.min(32)])
}

/// 常数时间比较两个字符串。
pub fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.as_bytes().iter().zip(b.as_bytes().iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 构建 Set-Cookie 头，颁发 CSRF token。
#[must_use]
pub fn csrf_set_cookie(token: &str, max_age_sec: i64) -> String {
    format!(
        "{}={}; Path=/; SameSite=Lax; HttpOnly=false; Max-Age={}",
        CSRF_COOKIE_NAME, token, max_age_sec
    )
}

/// 主 middleware：状态变更请求必须带 `X-CSRF-Token` 且匹配 cookie。
/// Pure CSRF decision: given a method, path, and headers, returns
/// `Ok(())` if the request is allowed or `Err(reason)` if it should be
/// rejected with 403. Exposed for unit tests so the middleware can be
/// exercised without spinning up an [`AppState`].
pub fn csrf_decision(method: &str, path: &str, headers: &HeaderMap) -> Result<(), CsrfDenial> {
    let method_upper = method.to_uppercase();

    // GET/HEAD/OPTIONS 永远放行
    if !CSRF_REQUIRED_METHODS.contains(&method_upper.as_str()) {
        return Ok(());
    }

    // 路径白名单放行
    if csrf_path_allowed(path) {
        return Ok(());
    }

    let cookies = parse_cookies(headers);

    // 只对 cookie 浏览器会话强制 CSRF；Bearer / API key 客户端是非浏览器
    // （命令行 / 服务端调用），不需要 double-submit cookie 保护。
    // 这样与 better-auth 的语义一致：CSRF 保护浏览器 form-submit，不保护
    // API client。
    let has_session = cookies.iter().any(|(n, _)| *n == SESSION_COOKIE_NAME);
    if !has_session {
        return Ok(());
    }

    // 提取 cookie 中的 csrf token
    let cookie_token = cookies
        .iter()
        .find(|(n, _)| *n == CSRF_COOKIE_NAME)
        .map(|(_, v)| *v);

    // 提取 header 中的 csrf token
    let header_token = headers
        .get(CSRF_HEADER_NAME)
        .and_then(|v| v.to_str().ok());

    let cookie = cookie_token.ok_or(CsrfDenial::MissingCookie)?;
    let header = header_token.ok_or(CsrfDenial::MissingHeader)?;
    if ct_eq(cookie, header) {
        Ok(())
    } else {
        Err(CsrfDenial::Mismatch)
    }
}

/// Reason a CSRF check denied the request.
#[derive(Debug, PartialEq, Eq)]
pub enum CsrfDenial {
    MissingCookie,
    MissingHeader,
    Mismatch,
}

impl CsrfDenial {
    /// Stable string identifier for the denial reason (used in error body).
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::MissingCookie => "missing csrf cookie",
            Self::MissingHeader => "missing csrf header",
            Self::Mismatch => "csrf token mismatch",
        }
    }
}

/// axum middleware：状态变更请求必须带 `X-CSRF-Token` 且匹配 cookie。
pub async fn csrf_layer(State(_state): State<AppState>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str();
    let path = parts.uri.path();
    if let Err(reason) = csrf_decision(method, path, &parts.headers) {
        return forbidden(reason.reason());
    }
    let req = Request::from_parts(parts, body);
    next.run(req).await
}

/// 构建 403 Forbidden 响应（与 better-auth 错误响应 schema 对齐）。
fn forbidden(reason: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": format!("csrf validation failed: {reason}"),
            "code": "CSRF_VALIDATION_FAILED"
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn h(headers: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in headers {
            m.insert(*k, HeaderValue::from_static(v));
        }
        m
    }

    fn h_runtime(headers: &[(&'static str, String)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in headers {
            m.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        m
    }

    fn assert_allowed(method: &str, path: &str, headers: &HeaderMap) {
        match csrf_decision(method, path, headers) {
            Ok(()) => {}
            Err(reason) => panic!("expected allow, got {reason:?}"),
        }
    }

    fn assert_denied(method: &str, path: &str, headers: &HeaderMap, expected: CsrfDenial) {
        match csrf_decision(method, path, headers) {
            Ok(()) => panic!("expected deny {expected:?}, got Ok"),
            Err(reason) => assert_eq!(reason, expected, "wrong denial reason"),
        }
    }

    #[test]
    fn generate_csrf_token_has_expected_length() {
        let t = generate_csrf_token();
        // hex of 32 bytes → 64 chars
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        // 两次生成结果不同（高熵）
        let t2 = generate_csrf_token();
        assert_ne!(t, t2);
    }

    #[test]
    fn ct_eq_returns_true_for_matching() {
        assert!(ct_eq("hello", "hello"));
    }

    #[test]
    fn ct_eq_returns_false_for_different_lengths() {
        assert!(!ct_eq("hello", "hellos"));
        assert!(!ct_eq("hi", "hello"));
    }

    #[test]
    fn ct_eq_returns_false_for_different_values() {
        assert!(!ct_eq("hello", "world"));
        assert!(!ct_eq("AAAA", "BBBB"));
    }

    #[test]
    fn csrf_path_allowed_recognizes_whitelist() {
        assert!(csrf_path_allowed("/api/auth/sign-in/email"));
        assert!(csrf_path_allowed("/api/auth/sign-up/email"));
        assert!(csrf_path_allowed("/live-events"));
        assert!(csrf_path_allowed("/openapi.json"));
        assert!(csrf_path_allowed("/api/openapi.json"));
        assert!(csrf_path_allowed("/health"));
        assert!(!csrf_path_allowed("/api/companies"));
        assert!(!csrf_path_allowed("/api/agents/foo"));
    }

    #[test]
    fn csrf_set_cookie_format() {
        let c = csrf_set_cookie("abc", 3600);
        assert!(c.contains("paperclip_csrf=abc"));
        assert!(c.contains("Path=/"));
        assert!(c.contains("SameSite=Lax"));
        assert!(c.contains("Max-Age=3600"));
    }

        #[test]
    fn get_request_passes_through_without_csrf() {
        let headers = h(&[]);
        assert_allowed("GET", "/api/companies", &headers);
        assert_allowed("HEAD", "/api/companies", &headers);
        assert_allowed("OPTIONS", "/api/companies", &headers);
    }

    #[test]
    fn post_without_session_or_token_passes() {
        let headers = h(&[]);
        assert_allowed("POST", "/api/companies", &headers);
    }

    #[test]
    fn post_with_session_but_no_csrf_returns_missing_cookie() {
        let headers = h(&[("cookie", "paperclip_session=abc")]);
        assert_denied("POST", "/api/companies", &headers, CsrfDenial::MissingCookie);
    }

    #[test]
    fn post_with_cookie_but_no_header_returns_missing_header() {
        let headers = h(&[("cookie", "paperclip_session=abc; paperclip_csrf=token")]);
        assert_denied("POST", "/api/companies", &headers, CsrfDenial::MissingHeader);
    }

    #[test]
    fn post_with_header_but_no_cookie_returns_missing_cookie() {
        let headers = h(&[
            ("cookie", "paperclip_session=abc"),
            ("x-csrf-token", "abc123"),
        ]);
        assert_denied("POST", "/api/companies", &headers, CsrfDenial::MissingCookie);
    }

    #[test]
    fn post_with_session_and_matching_csrf_passes() {
        let token = "abc123def456";
        let cookie = format!("paperclip_session=xyz; paperclip_csrf={token}");
        let headers = h_runtime(&[("cookie", cookie), ("x-csrf-token", token.to_string())]);
        assert_allowed("POST", "/api/companies", &headers);
    }

    #[test]
    fn post_with_mismatched_csrf_returns_mismatch() {
        let headers = h(&[
            ("cookie", "paperclip_session=xyz; paperclip_csrf=cookie_token"),
            ("x-csrf-token", "different_header_token"),
        ]);
        assert_denied("POST", "/api/companies", &headers, CsrfDenial::Mismatch);
    }

    #[test]
    fn post_with_different_length_csrf_returns_mismatch() {
        let headers = h(&[
            ("cookie", "paperclip_session=xyz; paperclip_csrf=short"),
            ("x-csrf-token", "much_longer_header_token"),
        ]);
        assert_denied("POST", "/api/companies", &headers, CsrfDenial::Mismatch);
    }

    #[test]
    fn post_to_whitelisted_path_skips_csrf() {
        // /api/auth/* 入口即使有 session 也跳过 CSRF
        let headers = h(&[("cookie", "paperclip_session=abc")]);
        assert_allowed("POST", "/api/auth/sign-in/email", &headers);
        assert_allowed("POST", "/api/auth/sign-up/email", &headers);
        // /live-events / openapi.json
        assert_allowed("GET", "/live-events", &h(&[]));
        assert_allowed("GET", "/openapi.json", &h(&[]));
        assert_allowed("GET", "/api/openapi.json", &h(&[]));
        // /health
        assert_allowed("GET", "/health", &h(&[]));
    }

    #[test]
    fn bearer_token_alone_does_not_require_csrf() {
        // 非浏览器 API 客户端不需要 CSRF（better-auth 行为等价）
        let headers = h(&[("authorization", "Bearer abc")]);
        assert_allowed("POST", "/api/companies", &headers);
    }

    #[test]
    fn api_key_header_alone_does_not_require_csrf() {
        let headers = h(&[("x-paperclip-api-key", "pk_xyz")]);
        assert_allowed("POST", "/api/companies", &headers);
    }

    #[test]
    fn csrf_decision_is_case_insensitive_for_method() {
        // POST without auth headers → allowed (no protected state).
        let headers = h(&[]);
        assert_allowed("post", "/api/companies", &headers);
        assert_allowed("Post", "/api/companies", &headers);
        // DELETE with session cookie but no CSRF → 403 missing cookie.
        let auth_headers = h(&[("cookie", "paperclip_session=xyz")]);
        assert_denied("delete", "/api/companies", &auth_headers, CsrfDenial::MissingCookie);
        assert_denied("Delete", "/api/companies", &auth_headers, CsrfDenial::MissingCookie);
    }
}
