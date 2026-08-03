//! 通用 authz 决策端点：返回当前 actor 的授权能力摘要。

use axum::{
    extract::State,
    http::HeaderMap,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use crate::{state::require_user_id, AppState};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/authz", get(authz))
}

async fn authz(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Json<Value> {
    // Return actor authorization summary; resolve user from session/API key.
    let user_id = require_user_id(&state, &headers).await.ok();
    let pool = state.db.pool();
    let memberships: Vec<(String, String)> = if let Some(uid) = user_id.as_deref() {
        sqlx::query_as(
            "SELECT company_id::text, membership_role FROM company_memberships \
             WHERE principal_id = $1 AND status = 'active' AND principal_type = 'user'",
        )
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let companies: Vec<&str> = memberships.iter().map(|(c, _)| c.as_str()).collect();
    let roles: Vec<&str> = memberships.iter().map(|(_, r)| r.as_str()).collect();
    let can_write = roles.iter().any(|r| matches!(*r, "owner" | "admin" | "member"));
    let is_owner = roles.iter().any(|r| *r == "owner");
    Json(json!({
        "module": "authz",
        "status": "ok",
        "actor": user_id,
        "companies": companies,
        "roles": roles,
        "permissions": {
            "canCreateAgents": can_write,
            "canApprove": can_write,
            "canManageBilling": is_owner,
            "canManageMembers": can_write,
            "canManageEnvironments": can_write,
            "isInstanceAdmin": false,
        }
    }))
}
