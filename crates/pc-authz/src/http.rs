//! pc-authz：HTTP 路由层便捷函数。
//!
//! 提供 `enforce` 系列：把 `Actor` + DB → `Context` → `evaluate` → `Result<(), ApiError>`。
//!
//! 调用方通常：
//! ```ignore
//! use pc_authz::{enforce, Action, PermissionKey, Resource};
//!
//! async fn handler(
//!     State(state): State<AppState>,
//!     actor: Extension<AuthContext>,
//!     Path(company_id): Path<Uuid>,
//! ) -> ApiResult<Json<Value>> {
//!     enforce(&state.db, &actor, Resource::Company { company_id },
//!             Action::Permission(PermissionKey::AgentsCreate)).await?;
//!     // ... handler 主体
//! }
//! ```

use pc_auth::AuthContext;
use pc_db::Db;
use uuid::Uuid;

use crate::builder::build_context;
use crate::policy::{evaluate, AuthzError};
use crate::types::{Action, PermissionKey, Resource};

/// 通用拒绝：把 `AuthzError` 转成 `ApiError` 的等价物。
///
/// 该函数**仅依赖 `serde::Serialize` + `Display`**，不引入 axum；
/// 调用方按需映射到自己的错误类型（例如 `crate::ApiError::Forbidden`）。
pub fn denial_to_string(err: AuthzError) -> String {
    err.to_string()
}

/// 在路由 handler 开头调用：注入 actor + resource + action 检查。
///
/// 失败时返回 Err(`AuthzError`)，调用方映射到自己的 ApiError。
pub async fn enforce(
    db: &Db,
    actor: &AuthContext,
    resource: Resource,
    action: Action,
) -> Result<(), AuthzError> {
    let ctx = build_context(db, &actor.actor).await;
    let decision = evaluate(&actor.actor, &ctx, &resource, action);
    if decision.allowed {
        Ok(())
    } else {
        Err(AuthzError::Forbidden(decision.explanation))
    }
}

/// 便捷包装：检查指定 permission key。
pub async fn enforce_permission(
    db: &Db,
    actor: &AuthContext,
    company_id: Uuid,
    permission: PermissionKey,
) -> Result<(), AuthzError> {
    enforce(
        db,
        actor,
        Resource::Company { company_id },
        Action::Permission(permission),
    )
    .await
}

/// 便捷包装：检查 issue 维度。
pub async fn enforce_issue(
    db: &Db,
    actor: &AuthContext,
    resource: crate::types::Resource,
    action: Action,
) -> Result<(), AuthzError> {
    enforce(db, actor, resource, action).await
}

/// 把 actor + 公司映射到 Resource::Company（节省调用方模板代码）。
pub fn company_resource(company_id: Uuid) -> Resource {
    Resource::Company { company_id }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denial_to_string_includes_message() {
        let s = denial_to_string(AuthzError::Forbidden("nope".into()));
        assert!(s.contains("nope"));
    }

    #[test]
    fn company_resource_constructs_correct_variant() {
        let c = Uuid::new_v4();
        match company_resource(c) {
            Resource::Company { company_id } => assert_eq!(company_id, c),
            _ => panic!("wrong variant"),
        }
    }
}
