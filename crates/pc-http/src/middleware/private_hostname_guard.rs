//! 私有主机名守卫 — 等价于 Node `middleware/private-hostname-guard.ts`。
//!
//! 仅当部署暴露为 private 时启用（Node `shouldEnablePrivateHostnameGuard`：
//! exposure=private 且 mode ∈ {local_trusted, authenticated}；Rust 的
//! `DeploymentMode` 枚举只有这两个变体，因此等价于 exposure=private）。
//! 未命中 allow-set 的 Host 返回 403（API 路径返回 JSON，其余返回 text/plain）。

use axum::{
    extract::Request,
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::collections::BTreeSet;

/// 环回主机名（Node `isLoopbackHostname`）。
pub fn is_loopback_hostname(hostname: &str) -> bool {
    let normalized = hostname.trim().to_lowercase();
    normalized == "localhost" || normalized == "127.0.0.1" || normalized == "::1"
}

/// 从请求头提取主机名：优先 `x-forwarded-host` 第一项，其次 `Host`。
/// 解析失败时回退原始值（与 Node `new URL` try/catch 等价）。
///
/// 注意：Node `URL.hostname` 对 IPv6 返回带括号形式（如 `[::1]`），导致其自身
/// `isLoopbackHostname` / allow-set（均为无括号 `::1`）永远无法命中——这里剥离
/// 方括号，使环回判定与 allow-set 正常工作（有意修正的上游怪癖）。
pub fn extract_hostname(headers: &HeaderMap) -> Option<String> {
    let forwarded = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(",").next())
        .map(|s| s.trim().to_string());
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string());
    let raw = forwarded.or(host)?;
    if raw.is_empty() {
        return None;
    }
    match url::Url::parse(&format!("http://{raw}")) {
        Ok(u) => u.host_str().map(|h| {
            h.trim()
                .trim_start_matches("[")
                .trim_end_matches("]")
                .to_lowercase()
        }),
        Err(_) => Some(raw.trim().to_lowercase()),
    }
}

/// 规范化允许列表：trim + 小写 + 去重 + 去空（Node `normalizeAllowedHostnames`）。
pub fn normalize_allowed_hostnames(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    for value in values {
        let trimmed = value.trim().to_lowercase();
        if !trimmed.is_empty() {
            seen.insert(trimmed);
        }
    }
    seen.into_iter().collect()
}

/// 解析最终 allow-set（Node `resolvePrivateHostnameAllowSet`）：
/// 配置列表 + bindHost（非 0.0.0.0 时）+ localhost/127.0.0.1/::1。
pub fn resolve_private_hostname_allow_set(
    allowed_hostnames: &[String],
    bind_host: &str,
) -> BTreeSet<String> {
    let mut allow = BTreeSet::new();
    for host in normalize_allowed_hostnames(allowed_hostnames) {
        allow.insert(host);
    }
    let bind = bind_host.trim().to_lowercase();
    if !bind.is_empty() && bind != "0.0.0.0" {
        allow.insert(bind);
    }
    allow.insert("localhost".to_string());
    allow.insert("127.0.0.1".to_string());
    allow.insert("::1".to_string());
    allow
}

/// 阻断消息（Node `blockedHostnameMessage`）。
pub fn blocked_hostname_message(hostname: &str) -> String {
    format!(
        "Hostname {hostname:?} is not allowed for this Paperclip instance. If you want to allow this hostname, please run pnpm paperclipai allowed-hostname {hostname}"
    )
}

/// 是否启用守卫（等价 Node `shouldEnablePrivateHostnameGuard` 在当前枚举下的判定）。
pub fn should_enable_private_hostname_guard(exposure: pc_network_bind::DeploymentExposure) -> bool {
    matches!(exposure, pc_network_bind::DeploymentExposure::Private)
}

/// 等价 Node `req.accepts(["json","html","text"]) === "json"`（negotiator 算法）：
/// - 无 Accept 头 → 视为 `*/*` → 匹配顺序 json > html > text → true
/// - 每个候选类型取 (specificity, q, accept 顺序) 最优的匹配范围
/// - 最终按 q 降序、specificity 降序、accept 顺序升序、候选顺序升序排序
/// - 返回是否首选为 json
pub fn accept_prefers_json(accept: Option<&str>) -> bool {
    let raw = accept.unwrap_or("*/*");
    #[derive(Debug, Clone)]
    struct AcceptSpec {
        ty: String,
        subtype: String,
        q: f64,
        o: i64,
        params: Vec<(String, String)>,
    }
    let mut accepts: Vec<AcceptSpec> = Vec::new();
    for (o, part) in raw.split(",").enumerate() {
        let mut pieces = part.trim().split(";");
        let mime = pieces.next().unwrap_or("").trim().to_lowercase();
        let (ty, subtype) = mime.split_once("/").unwrap_or((mime.as_str(), ""));
        if ty.is_empty() || subtype.is_empty() {
            continue;
        }
        let mut q = 1.0;
        let mut params = Vec::new();
        for param in pieces {
            let param = param.trim();
            if param.to_lowercase().starts_with("q=") {
                q = param[2..].trim().parse::<f64>().unwrap_or(f64::NAN);
                if !q.is_finite() {
                    q = f64::NAN;
                }
                continue;
            }
            if let Some((k, v)) = param.split_once("=") {
                params.push((k.trim().to_lowercase(), v.trim().to_string()));
            }
        }
        if q.is_nan() {
            q = f64::NAN;
        }
        accepts.push(AcceptSpec {
            ty: ty.to_string(),
            subtype: subtype.to_string(),
            q,
            o: o as i64,
            params,
        });
    }

    let provided: [(&str, &str); 3] = [
        ("application/json", "json"),
        ("text/html", "html"),
        ("text/plain", "text"),
    ];

    // 每个候选类型计算 (specificity, q, accept 顺序) 的最优匹配。
    let mut priorities: Vec<(f64, i64, i64, usize)> = Vec::new();
    for (i, (full, _)) in provided.iter().enumerate() {
        let (ptype, psub) = full.split_once("/").unwrap();
        let mut best: Option<(i64, f64, i64)> = None;
        for spec in &accepts {
            let mut s = 0i64;
            if spec.ty.eq_ignore_ascii_case(ptype) {
                s |= 4;
            } else if spec.ty != "*" {
                continue;
            }
            if spec.subtype.eq_ignore_ascii_case(psub) {
                s |= 2;
            } else if spec.subtype != "*" {
                continue;
            }
            if !spec.params.is_empty() {
                if spec.params.iter().all(|(_, v)| v == "*") {
                    s |= 1;
                } else {
                    continue;
                }
            }
            let cand = (s, spec.q, spec.o);
            let replace = match best {
                None => true,
                Some(b) => {
                    b.0 < cand.0
                        || (b.0 == cand.0 && b.1 < cand.1)
                        || (b.0 == cand.0 && b.1 == cand.1 && b.2 < cand.2)
                }
            };
            if replace {
                best = Some(cand);
            }
        }
        if let Some((s, q, o)) = best {
            if q > 0.0 {
                priorities.push((q, s, o, i));
            }
        }
    }

    priorities.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.1.cmp(&a.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });
    priorities.first().is_some_and(|p| p.3 == 0)
}

/// 守卫配置（由 pc-server 启动时注入 Extension）。
#[derive(Debug, Clone)]
pub struct PrivateHostnameGuardConfig {
    pub enabled: bool,
    pub allowed_hostnames: Vec<String>,
    pub bind_host: String,
}

impl Default for PrivateHostnameGuardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_hostnames: Vec::new(),
            bind_host: "0.0.0.0".to_string(),
        }
    }
}

impl PrivateHostnameGuardConfig {
    /// 从环境变量读取 allow-list 与 bind host（enabled 由调用方按部署模式决定）。
    pub fn from_environment() -> Self {
        let mut cfg = Self::default();
        if let Ok(raw) = std::env::var("PAPERCLIP_ALLOWED_HOSTNAMES") {
            cfg.allowed_hostnames = raw
                .split(",")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(bind) = std::env::var("PAPERCLIP_BIND_HOST") {
            cfg.bind_host = bind;
        }
        cfg
    }
}

fn forbidden(req: &Request, error: &str) -> Response {
    let path = req.uri().path();
    let accept = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok());
    let wants_json = path.starts_with("/api") || accept_prefers_json(accept);
    if wants_json {
        (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({ "error": error })),
        )
            .into_response()
    } else {
        (
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            error.to_string(),
        )
            .into_response()
    }
}

/// 守卫中间件（from_fn 形式）。
pub async fn private_hostname_guard_layer(req: Request, next: Next) -> Response {
    let cfg = req
        .extensions()
        .get::<PrivateHostnameGuardConfig>()
        .cloned()
        .unwrap_or_default();
    if !cfg.enabled {
        return next.run(req).await;
    }
    let allow_set = resolve_private_hostname_allow_set(&cfg.allowed_hostnames, &cfg.bind_host);
    let Some(hostname) = extract_hostname(req.headers()) else {
        let error = "Missing Host header. If you want to allow a hostname, run pnpm paperclipai allowed-hostname <host>.";
        return forbidden(&req, error);
    };
    if is_loopback_hostname(&hostname) || allow_set.contains(&hostname) {
        return next.run(req).await;
    }
    forbidden(&req, &blocked_hostname_message(&hostname))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::HeaderName;
    use axum::{body::Body, routing::get, Router};
    use tower::ServiceExt;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback_hostname("localhost"));
        assert!(is_loopback_hostname("LOCALHOST"));
        assert!(is_loopback_hostname("127.0.0.1"));
        assert!(is_loopback_hostname("::1"));
        assert!(!is_loopback_hostname("example.com"));
    }

    #[test]
    fn hostname_extraction_prefers_forwarded_host() {
        let h = headers(&[
            (
                "x-forwarded-host",
                "forwarded.example.com, other.example.com",
            ),
            ("host", "host.example.com"),
        ]);
        assert_eq!(
            extract_hostname(&h).as_deref(),
            Some("forwarded.example.com")
        );
    }

    #[test]
    fn hostname_extraction_falls_back_to_host_header() {
        let h = headers(&[("host", "Host.Example.com:3100")]);
        assert_eq!(extract_hostname(&h).as_deref(), Some("host.example.com"));
    }

    #[test]
    fn hostname_extraction_ipv6_and_missing() {
        let h = headers(&[("host", "[::1]:3100")]);
        assert_eq!(extract_hostname(&h).as_deref(), Some("::1"));
        assert_eq!(extract_hostname(&HeaderMap::new()), None);
    }

    #[test]
    fn allow_set_includes_bind_host_and_loopback() {
        let allow = resolve_private_hostname_allow_set(&["My.Host".to_string()], "192.168.1.5");
        assert!(allow.contains("my.host"));
        assert!(allow.contains("192.168.1.5"));
        assert!(allow.contains("localhost"));
        assert!(allow.contains("127.0.0.1"));
        assert!(allow.contains("::1"));
        // bind 0.0.0.0 不加入
        let allow2 = resolve_private_hostname_allow_set(&[], "0.0.0.0");
        assert!(!allow2.contains("0.0.0.0"));
    }

    #[test]
    fn blocked_message_contains_hostname_and_command() {
        let msg = blocked_hostname_message("evil.example.com");
        assert!(msg.contains("evil.example.com"));
        assert!(msg.contains("allowed-hostname evil.example.com"));
    }

    #[test]
    fn accept_prefers_json_rules() {
        assert!(accept_prefers_json(None));
        assert!(accept_prefers_json(Some("application/json")));
        assert!(accept_prefers_json(Some("*/*")));
        assert!(!accept_prefers_json(Some("text/html")));
        assert!(!accept_prefers_json(Some("text/plain")));
        assert!(accept_prefers_json(Some(
            "text/html;q=0.5, application/json;q=0.8"
        )));
        // 平局时 negotiator 按 accept 头顺序（text/html 先出现）选择 html
        assert!(!accept_prefers_json(Some(
            "text/html;q=0.8, application/json;q=0.8"
        )));
        // q=0 排除 → 无任何可接受类型 → false（Node 实测）
        assert!(!accept_prefers_json(Some("text/html;q=0")));
        assert!(!accept_prefers_json(Some("text/plain, text/html")));
    }

    #[tokio::test]
    async fn disabled_guard_passes_through() {
        let app = Router::new()
            .route("/api/x", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(private_hostname_guard_layer));
        let req = axum::http::Request::builder()
            .uri("/api/x")
            .header("host", "evil.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn enabled_guard_blocks_unknown_host_with_json() {
        let app = Router::new().route("/api/x", get(|| async { "ok" })).layer(
            axum::middleware::from_fn_with_state(
                PrivateHostnameGuardConfig {
                    enabled: true,
                    allowed_hostnames: vec!["good.example.com".to_string()],
                    bind_host: "0.0.0.0".to_string(),
                },
                |req: Request, next: Next| async move {
                    let mut req = req;
                    req.extensions_mut().insert(PrivateHostnameGuardConfig {
                        enabled: true,
                        allowed_hostnames: vec!["good.example.com".to_string()],
                        bind_host: "0.0.0.0".to_string(),
                    });
                    private_hostname_guard_layer(req, next).await
                },
            ),
        );
        let req = axum::http::Request::builder()
            .uri("/api/x")
            .header("host", "evil.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("evil.example.com"));
    }

    #[tokio::test]
    async fn enabled_guard_allows_configured_host() {
        let app =
            Router::new()
                .route("/api/x", get(|| async { "ok" }))
                .layer(axum::middleware::from_fn(
                    |req: Request, next: Next| async move {
                        let mut req = req;
                        req.extensions_mut().insert(PrivateHostnameGuardConfig {
                            enabled: true,
                            allowed_hostnames: vec!["good.example.com".to_string()],
                            bind_host: "0.0.0.0".to_string(),
                        });
                        private_hostname_guard_layer(req, next).await
                    },
                ));
        let req = axum::http::Request::builder()
            .uri("/api/x")
            .header("host", "good.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
