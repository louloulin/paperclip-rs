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
    response::Response,
};

use crate::AppState;

/// axum middleware（带 state）：尝试解析认证上下文并注入到 extensions。
///
/// 始终注入一个 `AuthContext`（即使是 Anonymous），方便 handler 决策。
/// 必须通过 `axum::middleware::from_fn_with_state(state, auth_layer)` 调用。
pub async fn auth_layer(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = req.into_parts();
    let ctx = match pc_auth::resolve_auth(&state.db, &parts).await {
        Ok(ctx) => ctx,
        Err(_) => pc_auth::AuthContext::anonymous(),
    };
    parts.extensions.insert(ctx);
    let req = Request::from_parts(parts, body);
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
    use pc_auth::{Actor, ActorSource, AuthContext, CompanyMembership};
    use uuid::Uuid;

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
