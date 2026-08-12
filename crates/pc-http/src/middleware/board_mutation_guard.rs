//! Board actor 的浏览器来源守卫 — 等价 Node `middleware/board-mutation-guard.ts`。
//!
//! 规则：
//! - 安全方法 (GET / HEAD / OPTIONS) 一律放行
//! - 非 board actor (`Actor::User`) 一律放行（agent / system 调用不走浏览器）
//! - board actor 但 source ∈ {LocalImplicit, ApiKey, CloudTenant} 直接放行
//!   （本地 CLI / board API key / 受信 cloud tenant 调用不会带 origin）
//! - 其余 board mutation 要求 Request 的 Origin 或 Referer 命中
//!   trusted-origin 集合；否则 403。
//! - trusted-origin 集合 = 默认本地开发 + `http://<host>` / `https://<host>` + PAPERCLIP_PUBLIC_URL
//! - host 优先取 `x-forwarded-host` 第一项，否则 `Host` 头
use axum::{
    extract::Request,
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use pc_auth::{Actor, ActorSource, AuthContext};
use serde_json::json;
use std::collections::BTreeSet;
const SAFE_METHODS: [&str; 3] = ["GET", "HEAD", "OPTIONS"];
pub fn parse_origin(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let url = url::Url::parse(trimmed).ok()?;
    let scheme = url.scheme().to_lowercase();
    let host = url.host_str()?.to_lowercase();
    if host.is_empty() {
        return None;
    }
    let port = url.port();
    let default_port = match scheme.as_str() {
        "http" => Some(80u16),
        "https" => Some(443u16),
        _ => None,
    };
    let port_str = match port {
        Some(p) if Some(p) != default_port => format!(":{p}"),
        _ => String::new(),
    };
    Some(format!("{scheme}://{host}{port_str}"))
}
pub fn trusted_origins_for_request(
    headers: &HeaderMap,
    public_url: Option<&str>,
) -> BTreeSet<String> {
    let mut origins = BTreeSet::new();
    for d in ["http://localhost:3100", "http://127.0.0.1:3100"] {
        origins.insert(d.to_string());
    }
    let forwarded_host = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string());
    let host_header = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string());
    let host = forwarded_host.or(host_header);
    if let Some(h) = host {
        origins.insert(format!("http://{h}").to_lowercase());
        origins.insert(format!("https://{h}").to_lowercase());
    }
    if let Some(pu) = public_url {
        if let Some(o) = parse_origin(pu) {
            origins.insert(o);
        }
    }
    origins
}
pub fn is_trusted_board_mutation_request(headers: &HeaderMap, public_url: Option<&str>) -> bool {
    let allowed = trusted_origins_for_request(headers, public_url);
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_origin);
    if let Some(o) = origin {
        if allowed.contains(&o) {
            return true;
        }
    }
    let referer = headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_origin);
    if let Some(r) = referer {
        if allowed.contains(&r) {
            return true;
        }
    }
    false
}
pub async fn board_mutation_guard_layer(req: Request, next: Next) -> Response {
    let method = req.method().as_str().to_ascii_uppercase();
    if SAFE_METHODS.contains(&method.as_str()) {
        return next.run(req).await;
    }
    let actor = req
        .extensions()
        .get::<AuthContext>()
        .map(|c| c.actor.clone());
    let source = req
        .extensions()
        .get::<AuthContext>()
        .map(|c| c.source)
        .unwrap_or(ActorSource::None);
    let is_board = matches!(actor, Some(Actor::User { .. }));
    if !is_board {
        return next.run(req).await;
    }
    if matches!(
        source,
        ActorSource::LocalImplicit | ActorSource::ApiKey | ActorSource::CloudTenant
    ) {
        return next.run(req).await;
    }
    let public_url = std::env::var("PAPERCLIP_PUBLIC_URL").ok();
    if is_trusted_board_mutation_request(req.headers(), public_url.as_deref()) {
        return next.run(req).await;
    }
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "Board mutation requires trusted browser origin" })),
    )
        .into_response()
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::post, Router};
    use tower::ServiceExt;
    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        map
    }
    fn app_with_actor(source: ActorSource) -> Router {
        let actor_layer =
            axum::middleware::from_fn(move |mut req: Request, next: Next| async move {
                let actor = Actor::User {
                    id: "u".into(),
                    name: None,
                    email: None,
                    is_instance_admin: false,
                    company_ids: Vec::new(),
                    memberships: Vec::new(),
                    run_id: None,
                };
                req.extensions_mut().insert(AuthContext {
                    actor,
                    source,
                    method: "session",
                    api_key_id: None,
                });
                next.run(req).await
            });
        Router::new()
            .route(
                "/read",
                axum::routing::get(|| async { axum::http::StatusCode::NO_CONTENT }),
            )
            .route(
                "/mutate",
                axum::routing::post(|| async { axum::http::StatusCode::NO_CONTENT }),
            )
            .route_layer(axum::middleware::from_fn(board_mutation_guard_layer))
            .route_layer(actor_layer)
    }
    fn app_agent() -> Router {
        let actor_layer =
            axum::middleware::from_fn(move |mut req: Request, next: Next| async move {
                req.extensions_mut().insert(AuthContext {
                    actor: Actor::Agent {
                        id: uuid::Uuid::nil(),
                        company_id: uuid::Uuid::nil(),
                        key_id: None,
                        key_scope: Default::default(),
                        run_id: None,
                        on_behalf_of_user_id: None,
                        on_behalf_of_memberships: Vec::new(),
                    },
                    source: ActorSource::AgentKey,
                    method: "agent_key",
                    api_key_id: None,
                });
                next.run(req).await
            });
        Router::new()
            .route(
                "/mutate",
                axum::routing::post(|| async { axum::http::StatusCode::NO_CONTENT }),
            )
            .route_layer(axum::middleware::from_fn(board_mutation_guard_layer))
            .route_layer(actor_layer)
    }
    #[tokio::test]
    async fn allows_safe_methods_for_board_actor() {
        let app = app_with_actor(ActorSource::Session);
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/read")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert!(res.status().is_success());
    }
    #[tokio::test]
    async fn blocks_board_mutation_without_origin() {
        let app = app_with_actor(ActorSource::Session);
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mutate")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let body: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 4096).await.unwrap())
                .unwrap();
        assert_eq!(
            body["error"],
            "Board mutation requires trusted browser origin"
        );
    }
    #[tokio::test]
    async fn allows_local_implicit_without_origin() {
        let app = app_with_actor(ActorSource::LocalImplicit);
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mutate")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert!(res.status().is_success());
    }
    #[tokio::test]
    async fn allows_board_bearer_key_without_origin() {
        let app = app_with_actor(ActorSource::ApiKey);
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mutate")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert!(res.status().is_success());
    }
    #[tokio::test]
    async fn allows_cloud_tenant_without_origin() {
        let app = app_with_actor(ActorSource::CloudTenant);
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mutate")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert!(res.status().is_success());
    }
    #[tokio::test]
    async fn allows_trusted_origin() {
        let app = app_with_actor(ActorSource::Session);
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mutate")
            .header("origin", "http://localhost:3100")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert!(res.status().is_success());
    }
    #[tokio::test]
    async fn allows_trusted_referer_origin() {
        let app = app_with_actor(ActorSource::Session);
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mutate")
            .header("referer", "http://localhost:3100/issues/abc")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert!(res.status().is_success());
    }
    #[tokio::test]
    async fn allows_x_forwarded_host_match() {
        let app = app_with_actor(ActorSource::Session);
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mutate")
            .header("host", "127.0.0.1")
            .header("x-forwarded-host", "10.90.10.20:3443")
            .header("origin", "https://10.90.10.20:3443")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert!(res.status().is_success());
    }
    #[tokio::test]
    async fn blocks_when_x_forwarded_host_does_not_match_origin() {
        let app = app_with_actor(ActorSource::Session);
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mutate")
            .header("host", "127.0.0.1")
            .header("x-forwarded-host", "10.90.10.20:3443")
            .header("origin", "https://evil.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }
    #[tokio::test]
    async fn does_not_block_agent_mutations() {
        let app = app_agent();
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mutate")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert!(res.status().is_success());
    }
    #[test]
    fn parse_origin_strips_default_port() {
        assert_eq!(
            parse_origin("http://localhost").as_deref(),
            Some("http://localhost")
        );
        assert_eq!(
            parse_origin("http://localhost:80").as_deref(),
            Some("http://localhost")
        );
        assert_eq!(
            parse_origin("https://example.com:443").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            parse_origin("http://localhost:3100").as_deref(),
            Some("http://localhost:3100")
        );
        assert_eq!(
            parse_origin("HTTPS://EXAMPLE.COM:443").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            parse_origin("http://localhost:3100/path?x=1").as_deref(),
            Some("http://localhost:3100")
        );
        assert_eq!(parse_origin("").as_deref(), None);
        assert_eq!(parse_origin("not-a-url").as_deref(), None);
    }
    #[test]
    fn trusted_origins_includes_public_url() {
        let h = headers(&[("host", "example.com")]);
        let set = trusted_origins_for_request(&h, Some("https://public.example.com"));
        assert!(set.contains("http://example.com"));
        assert!(set.contains("https://example.com"));
        assert!(set.contains("https://public.example.com"));
    }
}
