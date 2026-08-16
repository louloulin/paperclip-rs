//! 认证 middleware：API key + Session cookie + agent header 三轨。
//!
//! 与原 `paperclip/server/src/middleware/auth.ts` 等价。
//! 解析顺序：Authorization Bearer → `paperclip_session=` cookie → `x-paperclip-agent-id` header。
//!
//! 失败时不直接拒绝（handler 决定是否强制要求），而是把 `AuthContext` 注入到
//! request extensions；handler 通过 `pc_auth::AuthContext::require_user` 等便捷方法校验。

use axum::{
    extract::{Request, State},
    http::request::Parts,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::AppState;

/// axum middleware（带 state）：尝试解析认证上下文并注入到 extensions。
///
/// 始终注入一个 `AuthContext`（即使是 Anonymous），方便 handler 决策。
/// 必须通过 `axum::middleware::from_fn_with_state(state, auth_layer)` 调用。
/// 检测当前是否为 local_trusted 部署模式（与 Node `DEPLOYMENT_MODE === "local_trusted"` 等价）。
///
/// R664: 在 local_trusted 模式下，未认证请求会被自动注入一个 instance admin 用户
///（Node `actorMiddleware` 的等价行为）。authenticated 模式下，未认证请求得到
/// `Actor::Anonymous`，由 `require_board_layer` / `require_auth` 强制拒绝。
pub fn is_local_trusted_mode() -> bool {
    matches!(
        std::env::var("PAPERCLIP_DEPLOYMENT_MODE").as_deref(),
        Ok("local_trusted") | Ok("local-trusted")
    )
}

/// 构造 local-board 用户（Node `actorMiddleware` 在 local_trusted 模式下注入的等价值）。
fn local_board_auth_context() -> pc_auth::AuthContext {
    use pc_auth::{Actor, ActorSource, AuthContext};
    let actor = Actor::User {
        id: "local-board".to_string(),
        name: Some("Local Board".to_string()),
        email: None,
        is_instance_admin: true,
        company_ids: Vec::new(),
        memberships: Vec::new(),
        run_id: None,
    };
    AuthContext::for_actor(actor, ActorSource::LocalImplicit, "local_implicit")
}

/// axum middleware（带 state）：尝试解析认证上下文并注入到 extensions。
///
/// 始终注入一个 `AuthContext`（即使是 Anonymous），方便 handler 决策。
/// 必须通过 `axum::middleware::from_fn_with_state(state, auth_layer)` 调用。
///
/// R664: 在 local_trusted 模式下，未认证请求会自动获得 `Actor::User { id: "local-board",
/// is_instance_admin: true }`，与 Node 版 `actorMiddleware` 行为一致；在 authenticated
/// 模式下，未认证请求得到 `Actor::Anonymous`，由 `require_board_layer` 拒绝。
pub async fn auth_layer(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    let mut ctx = match pc_auth::resolve_auth(&state.db, &parts).await {
        Ok(ctx) => ctx,
        Err(_) => pc_auth::AuthContext::anonymous(),
    };
    // R664: local_trusted fallback — 注入 local-board user 当作 board 身份。
    if !ctx.actor.is_authenticated() && is_local_trusted_mode() {
        ctx = local_board_auth_context();
    }
    parts.extensions.insert(ctx);
    let req = Request::from_parts(parts, body);
    next.run(req).await
}

/// 公开路径白名单：这些路径不需要 board 身份，与 Node `routes/health.ts` 等价。
///
/// 其它 `/api/*` 路径在 `require_board_layer` 拒绝 anonymous，行为与 Node 版
/// `routes/authz.ts::assertBoard` 等价。
///
/// 豁免规则：
/// - 根路径与 `/health`、`/api/health`：UI 探针 / K8s liveness 必需
/// - `/api/*/health`：任何子系统的 health 端点都视为公开（运维探针）
/// - `/api/`、`/api`：列出根路由
pub fn is_public_auth_path(path: &str) -> bool {
    if matches!(path, "/health" | "/api/health" | "/api" | "/api/") {
        return true;
    }
    // /api/<subsystem>/health — any subsystem health is public.
    if let Some(rest) = path.strip_prefix("/api/") {
        if rest.ends_with("/health") {
            return true;
        }
    }
    false
}

/// axum middleware：拒绝 anonymous 请求，返回 403 forbidden。
///
/// 镜像 Node 版 `assertBoard(req)`：要求 actor 是 User（含 local-board）或 Agent；
/// Anonymous 一律返回 403 "Board access required"，与 Node `routes/authz.ts::assertBoard` 等价。
///
/// 公开路径（health、auth）通过 `is_public_auth_path` 白名单豁免。
///
/// 必须由调用方通过 `axum::middleware::from_fn(require_board_layer)` 注入；通常在
/// `auth_layer` 之后应用，覆盖所有需要 board 身份的 `/api/*` 路由。
pub async fn require_board_layer(req: Request, next: Next) -> Response {
    use crate::ApiError;
    let path = req.uri().path();
    if is_public_auth_path(path) {
        return next.run(req).await;
    }
    let ctx = req
        .extensions()
        .get::<pc_auth::AuthContext>()
        .cloned()
        .unwrap_or_else(pc_auth::AuthContext::anonymous);
    let is_board = matches!(ctx.actor, pc_auth::Actor::User { .. } | pc_auth::Actor::Agent { .. })
        || matches!(ctx.actor, pc_auth::Actor::System);
    if !is_board {
        return ApiError::Forbidden("Board access required".to_string()).into_response();
    }
    next.run(req).await
}

/// 拒绝匿名请求的便捷函数（向后兼容）。
pub fn require_auth(ctx: &pc_auth::AuthContext) -> Result<(), crate::ApiError> {
    if !ctx.actor.is_authenticated() {
        Err(crate::ApiError::Unauthorized(
            "user authentication required".into(),
        ))
    } else {
        Ok(())
    }
}

/// 拒绝匿名+跨公司请求（与 Node 版 `assertCompanyAccess` 等价）。
pub fn require_company_access(
    ctx: &pc_auth::AuthContext,
    company_id: uuid::Uuid,
) -> Result<(), crate::ApiError> {
    if !ctx.actor.has_company_access(company_id) {
        return Err(crate::ApiError::Forbidden(
            "actor lacks access to this company".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use pc_auth::{Actor, ActorSource, AuthContext, CompanyMembership};
    use uuid::Uuid;

    use axum::routing::get;
    use tower::ServiceExt;

    async fn dummy_ok() -> Response {
        Response::new(Body::empty())
    }

    /// 注入 AuthContext 的测试 middleware + apply require_board_layer。
    fn build_test_router(initial: AuthContext) -> axum::Router {
        let initial = std::sync::Arc::new(initial);
        let inject = {
            let initial = std::sync::Arc::clone(&initial);
            move |mut req: Request<Body>, next: axum::middleware::Next| {
                let initial = std::sync::Arc::clone(&initial);
                async move {
                    req.extensions_mut().insert((*initial).clone());
                    next.run(req).await
                }
            }
        };
        axum::Router::new()
            .route("/api/companies", get(dummy_ok))
            .layer(axum::middleware::from_fn(require_board_layer))
            .layer(axum::middleware::from_fn(inject))
    }

    #[tokio::test]
    async fn require_board_layer_rejects_anonymous() {
        let app = build_test_router(AuthContext::anonymous());
        let req = Request::builder().uri("/api/companies").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn public_auth_path_whitelist() {
        // Root + api root
        assert!(is_public_auth_path("/health"));
        assert!(is_public_auth_path("/api/health"));
        assert!(is_public_auth_path("/api"));
        assert!(is_public_auth_path("/api/"));
        // Subsystem health
        assert!(is_public_auth_path("/api/workspace-runtime/health"));
        assert!(is_public_auth_path("/api/companies/health"));
        // Private endpoints
        assert!(!is_public_auth_path("/api/companies"));
        assert!(!is_public_auth_path("/api/auth/sign-in"));
        assert!(!is_public_auth_path("/api/workspace-runtime/readiness-timeout"));
    }

    #[tokio::test]
    async fn require_board_layer_accepts_user() {
        let ctx = AuthContext::for_actor(
            Actor::User {
                id: "u1".into(),
                name: None,
                email: None,
                is_instance_admin: false,
                company_ids: vec![],
                memberships: vec![],
                run_id: None,
            },
            ActorSource::Session,
            "session",
        );
        let app = build_test_router(ctx);
        let req = Request::builder().uri("/api/companies").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn require_board_layer_accepts_agent() {
        let ctx = AuthContext::for_actor(
            Actor::Agent {
                id: Uuid::new_v4(),
                company_id: Uuid::new_v4(),
                key_id: None,
                key_scope: pc_auth::KeyScope::default(),
                run_id: None,
                on_behalf_of_user_id: None,
                on_behalf_of_memberships: vec![],
            },
            ActorSource::AgentHeader,
            "agent_header",
        );
        let app = build_test_router(ctx);
        let req = Request::builder().uri("/api/companies").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
    }

    #[test]
    fn local_trusted_mode_detected_from_env() {
        let prev = std::env::var("PAPERCLIP_DEPLOYMENT_MODE").ok();
        std::env::set_var("PAPERCLIP_DEPLOYMENT_MODE", "local_trusted");
        assert!(is_local_trusted_mode());
        std::env::set_var("PAPERCLIP_DEPLOYMENT_MODE", "local-trusted");
        assert!(is_local_trusted_mode());
        std::env::set_var("PAPERCLIP_DEPLOYMENT_MODE", "authenticated");
        assert!(!is_local_trusted_mode());
        match prev {
            Some(v) => std::env::set_var("PAPERCLIP_DEPLOYMENT_MODE", v),
            None => std::env::remove_var("PAPERCLIP_DEPLOYMENT_MODE"),
        }
    }

    #[test]
    fn require_auth_rejects_anonymous() {
        let ctx = AuthContext::anonymous();
        assert!(require_auth(&ctx).is_err());
    }

    #[test]
    fn require_auth_accepts_user() {
        let ctx = AuthContext::for_actor(
            Actor::User {
                id: "u1".into(),
                name: None,
                email: None,
                is_instance_admin: false,
                company_ids: vec![],
                memberships: vec![],
                run_id: None,
            },
            ActorSource::Session,
            "session",
        );
        assert!(require_auth(&ctx).is_ok());
    }

    #[test]
    fn require_company_access_blocks_cross_company_agent() {
        let company_a = Uuid::new_v4();
        let company_b = Uuid::new_v4();
        let ctx = AuthContext::for_actor(
            Actor::Agent {
                id: Uuid::new_v4(),
                company_id: company_a,
                key_id: None,
                key_scope: pc_auth::KeyScope::default(),
                run_id: None,
                on_behalf_of_user_id: None,
                on_behalf_of_memberships: vec![],
            },
            ActorSource::AgentHeader,
            "agent_header",
        );
        assert!(require_company_access(&ctx, company_a).is_ok());
        assert!(require_company_access(&ctx, company_b).is_err());
    }

    #[test]
    fn instance_admin_can_access_any_company() {
        let target = Uuid::new_v4();
        let ctx = AuthContext::for_actor(
            Actor::User {
                id: "admin".into(),
                name: None,
                email: None,
                is_instance_admin: true,
                company_ids: vec![],
                memberships: vec![],
                run_id: None,
            },
            ActorSource::LocalImplicit,
            "local",
        );
        assert!(require_company_access(&ctx, target).is_ok());
    }

    #[test]
    fn membership_user_can_access_their_company() {
        let company = Uuid::new_v4();
        let ctx = AuthContext::for_actor(
            Actor::User {
                id: "u2".into(),
                name: None,
                email: None,
                is_instance_admin: false,
                company_ids: vec![company],
                memberships: vec![CompanyMembership {
                    company_id: company,
                    role: Some("member".into()),
                    status: Some("active".into()),
                }],
                run_id: None,
            },
            ActorSource::Session,
            "session",
        );
        assert!(require_company_access(&ctx, company).is_ok());
    }
}

// =====================================================================
// AuthContext extractor：handler 通过 Extension<AuthContext> 访问
// =====================================================================

/// Handler 端便捷提取器：从 extensions 读取 AuthContext。
/// 必须先经过 `auth_layer` 才能工作，否则返回 401。
#[axum::async_trait]
impl axum::extract::FromRequestParts<crate::AppState> for pc_auth::AuthContext {
    type Rejection = crate::ApiError;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<pc_auth::AuthContext>()
            .cloned()
            .ok_or_else(|| crate::ApiError::Unauthorized("auth context missing".into()))
    }
}
