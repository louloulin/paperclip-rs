//! 认证 middleware：API key + Session cookie 双轨。
//!
//! 与原 `paperclip/server/src/middleware/auth.ts` 等价。
//! 解析顺序：Authorization Bearer → `pcp_*` token →  session cookie。
//!
//! 失败时不直接拒绝（axum 层有 handler 自己 require），而是把 `AuthContext`
//! 注入到 request extensions；handler 决定是否强制要求。

use axum::{
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::Response,
};
use serde::Serialize;
use uuid::Uuid;

use crate::AppState;

/// 认证上下文。
#[derive(Debug, Clone, Serialize)]
pub struct AuthContext {
    pub user_id: String,
    pub auth_kind: AuthKind,
    /// 关联的 company_id（如果能推断）
    pub company_id: Option<Uuid>,
    /// 角色：ceo / member / agent
    pub role: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    ApiKey,
    Session,
    Cookie,
    Anonymous,
}

/// axum middleware：尝试解析认证上下文并注入到 extensions。
pub async fn auth_layer(state: AppState, mut req: Request, next: Next) -> Response {
    let ctx = resolve_auth(&state, &req).await;
    // 始终注入一个 AuthContext（即使是 Anonymous），方便 handler 决策
    req.extensions_mut().insert(ctx);
    next.run(req).await
}

async fn resolve_auth(state: &AppState, req: &Request) -> AuthContext {
    // 1) Authorization: Bearer <token>
    if let Some(token) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        if let Ok(Some((_, user_id))) = pc_auth::resolve_api_key(&state.db, token).await {
            return AuthContext {
                user_id,
                auth_kind: AuthKind::ApiKey,
                company_id: None,
                role: None,
            };
        }
        if let Ok(Some((user_id, _))) = pc_auth::resolve_session(&state.db, token).await {
            return AuthContext {
                user_id,
                auth_kind: AuthKind::Session,
                company_id: None,
                role: None,
            };
        }
    }
    // 2) Cookie
    if let Some(cookie) = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        let prefix = format!("{}=", state.config.session_cookie);
        for part in cookie.split(';').map(str::trim) {
            if let Some(token) = part.strip_prefix(&prefix) {
                if let Ok(Some((user_id, _))) = pc_auth::resolve_session(&state.db, token).await {
                    return AuthContext {
                        user_id,
                        auth_kind: AuthKind::Cookie,
                        company_id: None,
                        role: None,
                    };
                }
            }
        }
    }
    AuthContext {
        user_id: String::new(),
        auth_kind: AuthKind::Anonymous,
        company_id: None,
        role: None,
    }
}

/// 拒绝匿名请求的便捷函数。
pub fn require_auth(ctx: &AuthContext) -> Result<(), crate::ApiError> {
    if matches!(ctx.auth_kind, AuthKind::Anonymous) || ctx.user_id.is_empty() {
        Err(crate::ApiError::Unauthorized(
            "user authentication required".into(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_kind_equality() {
        assert_eq!(AuthKind::ApiKey, AuthKind::ApiKey);
        assert_ne!(AuthKind::ApiKey, AuthKind::Session);
    }

    #[test]
    fn require_auth_rejects_anonymous() {
        let ctx = AuthContext {
            user_id: String::new(),
            auth_kind: AuthKind::Anonymous,
            company_id: None,
            role: None,
        };
        assert!(require_auth(&ctx).is_err());
    }

    #[test]
    fn require_auth_accepts_session() {
        let ctx = AuthContext {
            user_id: "u1".into(),
            auth_kind: AuthKind::Session,
            company_id: None,
            role: None,
        };
        assert!(require_auth(&ctx).is_ok());
    }
}
