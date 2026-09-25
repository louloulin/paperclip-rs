//! agent MCP 服务器绑定：**4 条**路由（`router.go:2206/2207/2208/2209`）—— 写者 **M8-3**
//! （`LUM-1800` / `docs/61-M8-PLAN.md` §1.1 第 17–20 行 / §6.5 的 M8-3 行）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/agents/:id/mcp-servers` | GET | `ListAgentMcpServers` |
//! | `/api/agents/:id/mcp-servers` | POST | `AddAgentMcpServer`（响应是**更新后的绑定列表**） |
//! | `/api/agents/:id/mcp-servers/:serverId/enabled` | PUT | `SetAgentMcpServerEnabled` |
//! | `/api/agents/:id/mcp-servers/:serverId` | DELETE | `RemoveAgentMcpServer` |
//!
//! # 授权面（`docs/61` §6.5 的 M8-3 行逐字：「**不是**裸 workspace member」）
//!
//! 上游 `requireAgentMcpWriter` = `loadAgentForUser`（404）→ 拒 agent actor（403）→
//! workspace 成员（404）→ `canViewAgentSecrets`（agent owner 或 workspace owner/admin ⇒ 403）。
//! 本文件走 M6-4 `/skills*` 的同一手法：[`AgentScope::resolve`]（workspace 400 / 非成员 404）
//! → [`AgentScope::load_agent`]（`kind='user'` 且本 workspace，否则 404）→
//! [`AgentScope::can_view_secrets`]（403 `insufficient permissions`）。
//!
//! 两处与上游的**顺序/可实现性**差异（登记 `docs/32` §16）：
//! ① 本仓先解析 workspace 再加载 agent（上游先加载 agent）—— 状态码组合相同，只是
//!    「workspace 不是我的」与「agent 不存在」同时成立时报哪个的优先级不同；
//! ② 「拒 agent actor」那一支不可实现：本仓 mc-http 只有 `AuthUser`（恒人类成员），
//!    没有 agent 身份的请求上下文（与 M2-E 的 `properties.rs` 同款登记）。
//!
//! # 语义（与 workspace 面共享同一套 DTO / transport 投影）
//!
//! - **加**：幂等（`ON CONFLICT DO NOTHING`）—— 加两次不是错误，也不重复插行；跨 workspace 的
//!   `server_id` ⇒ 404（与「不存在」同判，不泄露存在性）。
//! - **开关**：只动 `enabled`，绑定**存活**（关掉再打开不必重新找一遍）；连开两次同一值幂等。
//! - **摘**：只摘绑定，库条目本身不动。
//! - 三条写路由的响应都是**更新后的绑定列表**（200），客户端不必猜结果状态。
//! - `enabled` 在列表里**总是**出现（绑定状态就是它的意思）；库列表里没有它。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::mcp::agent_binding::AgentMcpBindingRepo;
use mc_repos::RepoError;
use serde_json::Value as JsonValue;

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, forbidden, not_found, parse_uuid, repo_err, AgentScope};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

use super::workspace::McpServerResponse;

/// `/api/agents/{id}/mcp-servers*`（M8-3 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/agents/:id/mcp-servers",
            get(list_agent_mcp_servers).post(add_agent_mcp_server),
        )
        .route(
            "/api/agents/:id/mcp-servers/:serverId/enabled",
            axum::routing::put(set_agent_mcp_server_enabled),
        )
        .route(
            "/api/agents/:id/mcp-servers/:serverId",
            axum::routing::delete(remove_agent_mcp_server),
        )
}

fn binding_repo(state: &AppState) -> AgentMcpBindingRepo {
    AgentMcpBindingRepo::new(state.db.clone())
}

/// 上游 `writeAgentMcpServers`：变更之后返回该 agent 的绑定列表 + 200。
async fn updated_agent_mcp_servers(
    state: &AppState,
    agent_id: Id,
) -> ApiResult<Json<Vec<McpServerResponse>>> {
    let rows = binding_repo(state)
        .list_for_agent(agent_id)
        .await
        .map_err(|error| repo_err(error, "agent mcp server"))?;
    Ok(Json(
        rows.iter().map(McpServerResponse::from_binding).collect(),
    ))
}

/// 按 agent id 解析 workspace + 成员身份 + agent（404）后判 `canViewAgentSecrets`（403）。
async fn require_agent_mcp_reader(
    state: &AppState,
    auth: AuthUser,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    raw_agent_id: &str,
) -> Result<(AgentScope, mc_repos::agent::AgentRow), Error> {
    let scope = AgentScope::resolve(state, auth, headers, query).await?;
    let agent = scope.load_agent(raw_agent_id).await?;
    if !scope.can_view_secrets(&agent) {
        return Err(forbidden("insufficient permissions"));
    }
    Ok((scope, agent))
}

// ---------------------------------------------------------------------------
// GET /api/agents/{id}/mcp-servers
// ---------------------------------------------------------------------------

/// 上游 `ListAgentMcpServers`：该 agent 绑定的 workspace 库条目 + 每个绑定的开关状态。
async fn list_agent_mcp_servers(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(raw_agent_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<McpServerResponse>>> {
    let (_scope, agent) =
        require_agent_mcp_reader(&state, auth, &headers, &query, &raw_agent_id).await?;
    updated_agent_mcp_servers(&state, agent.id()).await
}

// ---------------------------------------------------------------------------
// POST /api/agents/{id}/mcp-servers
// ---------------------------------------------------------------------------

/// 上游 `AddAgentMcpServerRequest`。
struct AddRequest {
    /// `server_id`（缺席 / `null` / 非字符串 ⇒ 400）。
    server_id: Option<String>,
}

/// 上游对这段 body 的解码：`json.NewDecoder(...).Decode(&req)` ⇒ 空 / 非对象 ⇒ 400，
/// `null` ⇒ 零值（`server_id` 空串 ⇒ 后面 400）。
fn decode_add_request(body: &Bytes) -> Result<AddRequest, Error> {
    let value: JsonValue =
        serde_json::from_slice(body).map_err(|_| bad_request("invalid request body"))?;
    let object = match value {
        JsonValue::Null => serde_json::Map::new(),
        JsonValue::Object(object) => object,
        _ => return Err(bad_request("invalid request body")),
    };
    let server_id = match object.get("server_id") {
        None | Some(JsonValue::Null) => None,
        Some(JsonValue::String(id)) => Some(id.clone()),
        Some(_) => return Err(bad_request("invalid request body")),
    };
    Ok(AddRequest { server_id })
}

/// 上游 `AddAgentMcpServer`：把一个 workspace 库条目给到这个 agent —— 库条目**唯一**的散布
/// 途径。200 + 更新后的列表。
async fn add_agent_mcp_server(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(raw_agent_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<Vec<McpServerResponse>>> {
    let (scope, agent) =
        require_agent_mcp_reader(&state, auth, &headers, &query, &raw_agent_id).await?;
    let request = decode_add_request(&body)?;
    let server_id = Id(parse_uuid(
        request.server_id.as_deref().unwrap_or_default(),
        "server_id",
    )?);

    binding_repo(&state)
        .add(agent.id(), scope.workspace_id, server_id)
        .await
        .map_err(|error| match error {
            // 作用域校验与「不存在」同判（上游 404 `MCP server not found in this workspace`）。
            RepoError::NotFound => not_found("mcp server"),
            other => repo_err(other, "agent mcp server"),
        })?;
    updated_agent_mcp_servers(&state, agent.id()).await
}

// ---------------------------------------------------------------------------
// PUT /api/agents/{id}/mcp-servers/{serverId}/enabled
// ---------------------------------------------------------------------------

/// 上游 `SetAgentMcpServerEnabledRequest`：`enabled` 是**必填**。
fn decode_enabled(body: &Bytes) -> Result<bool, Error> {
    let value: JsonValue = serde_json::from_slice(body).unwrap_or(JsonValue::Null);
    value
        .get("enabled")
        .and_then(JsonValue::as_bool)
        .ok_or_else(|| bad_request("enabled is required"))
}

/// 上游 `SetAgentMcpServerEnabled`：**只**翻开关，不摘绑定。200 + 更新后的列表。
async fn set_agent_mcp_server_enabled(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path((raw_agent_id, raw_server_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<Vec<McpServerResponse>>> {
    let (_scope, agent) =
        require_agent_mcp_reader(&state, auth, &headers, &query, &raw_agent_id).await?;
    let server_id = Id(parse_uuid(&raw_server_id, "server id")?);
    let enabled = decode_enabled(&body)?;

    let rows = binding_repo(&state)
        .set_enabled(agent.id(), server_id, enabled)
        .await
        .map_err(|error| repo_err(error, "agent mcp server"))?;
    if rows == 0 {
        return Err(not_found("agent mcp server").into());
    }
    updated_agent_mcp_servers(&state, agent.id()).await
}

// ---------------------------------------------------------------------------
// DELETE /api/agents/{id}/mcp-servers/{serverId}
// ---------------------------------------------------------------------------

/// 上游 `RemoveAgentMcpServer`：把库条目从这个 agent 上摘下来，**库条目本身不动**。
/// 200 + 更新后的列表。
async fn remove_agent_mcp_server(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path((raw_agent_id, raw_server_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<McpServerResponse>>> {
    let (_scope, agent) =
        require_agent_mcp_reader(&state, auth, &headers, &query, &raw_agent_id).await?;
    let server_id = Id(parse_uuid(&raw_server_id, "server id")?);

    let rows = binding_repo(&state)
        .remove(agent.id(), server_id)
        .await
        .map_err(|error| repo_err(error, "agent mcp server"))?;
    if rows == 0 {
        return Err(not_found("agent mcp server").into());
    }
    updated_agent_mcp_servers(&state, agent.id()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `mc_errors::Error` 没有 `PartialEq` ⇒ 断言比较它的渲染文案（同一句话）。
    fn message(result: Result<bool, Error>) -> Result<bool, String> {
        result.map_err(|error| error.to_string())
    }

    /// 上游那段匿名结构体：`enabled` 缺字段 / `null` / 非布尔 ⇒ 同一句 400。
    #[test]
    fn enabled_decoding_requires_a_boolean() {
        assert_eq!(
            message(decode_enabled(&Bytes::from_static(b"{\"enabled\":true}"))),
            Ok(true)
        );
        assert_eq!(
            message(decode_enabled(&Bytes::from_static(b"{\"enabled\":false}"))),
            Ok(false)
        );
        for body in [
            &b"{}"[..],
            &b"null"[..],
            &b"{\"enabled\":null}"[..],
            &b"{\"enabled\":\"true\"}"[..],
        ] {
            assert_eq!(
                message(decode_enabled(&Bytes::from_static(body))),
                Err(bad_request("enabled is required").to_string()),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    /// `server_id`：缺席 / `null` / 非字符串都不进 uuid 解析（上游同理先解成零值再 400）。
    #[test]
    fn add_request_decoding_matches_go() {
        let ok = decode_add_request(&Bytes::from_static(
            br#"{"server_id":"2b5f0a4e-0000-4000-8000-000000000000"}"#,
        ))
        .expect("valid");
        assert!(ok.server_id.is_some());

        for body in [&b"{}"[..], &b"null"[..], &b"{\"server_id\":null}"[..]] {
            let request = decode_add_request(&Bytes::from_static(body)).expect("zero value");
            assert!(request.server_id.is_none());
            assert!(parse_uuid("", "server_id").is_err());
        }
        for body in [&b""[..], &b"[]"[..], &b"{\"server_id\":7}"[..]] {
            assert!(decode_add_request(&Bytes::from_static(body)).is_err());
        }
    }

    /// 建路由本身不 panic（axum 同 path+method 重复注册会在 build 期 panic）。
    #[test]
    fn router_builds_without_panicking() {
        let _ = router();
        let _ = super::super::router();
    }

    /// agent 面的响应**必定**带 `enabled`（列表里两个绑定一个开一个关）。
    #[test]
    fn binding_responses_always_carry_enabled() {
        let row = mc_repos::mcp::agent_binding::AgentMcpServerRow {
            id: uuid::Uuid::from_u128(1),
            workspace_id: uuid::Uuid::from_u128(2),
            name: "linear".into(),
            config: json!({"url":"https://secret.example"}),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            enabled: false,
        };
        let rendered = serde_json::to_string(&McpServerResponse::from_binding(&row)).expect("json");
        assert!(rendered.contains("\"enabled\":false"), "{rendered}");
        // write-only：URL 不进响应。
        assert!(!rendered.contains("secret.example"), "{rendered}");
    }
}
