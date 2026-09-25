//! workspace MCP 服务器库：**4 条**路由（`router.go:1686/1710/1711/1712`）—— 写者 **M8-3**
//! （`LUM-1800` / `docs/61-M8-PLAN.md` §1.1 第 13–16 行 / §6.5 的 M8-3 行）。
//!
//! | 注册键 | 方法 | 授权 | 上游 |
//! | --- | :-: | --- | --- |
//! | `/api/workspaces/:id/mcp-servers` | GET | member | `ListWorkspaceMcpServers` |
//! | `/api/workspaces/:id/mcp-servers` | POST | **admin** | `CreateWorkspaceMcpServer`（201） |
//! | `/api/workspaces/:id/mcp-servers/:serverId` | PUT | **admin** | `UpdateWorkspaceMcpServer` |
//! | `/api/workspaces/:id/mcp-servers/:serverId` | DELETE | **admin** | `DeleteWorkspaceMcpServer`（204） |
//!
//! # write-only（`docs/61` §2.7 第 5 条；上游 `WorkspaceMcpServerResponse` 的注释逐字）
//!
//! 响应**永不**含 `url` / `command` / `args` / `headers` / `env` —— 其中每一个常规地带着
//! token（Composio 风格的 session URL **本身就是**一枚 bearer 凭证）。[`McpServerResponse`]
//! 是按列名手写构造的，**没有**「把 `config` 塞进去」的代码路径；用例在**原始 JSON 字节**
//! 上断言，所以以后有人加一个带密字段会在这里红。
//!
//! # 与上游的三处刻意不同（登记 `docs/32` §16）
//!
//! 1. **agent actor 的门不可实现**：上游 `requireWorkspaceMcpWriter` 先拒 `actor == "agent"`
//!    （403 `agents cannot modify the workspace MCP servers`）。本仓 mc-http 只有
//!    `AuthUser`（会话 / `X-Multica-User-Id`，**恒人类成员**）⇒ 没有 agent 身份的请求上下文，
//!    该分支与 M2-E 的 `properties.rs` 同款不可实现；admin 门保留。
//! 2. **坏 uuid 的文案**沿本仓 M3-5 约定（`<field> must be a valid uuid`），上游是
//!    `invalid <field>`；状态码一致（400）。
//! 3. `UPDATE` 的真实库故障回 **500**（[`Error::Database`]），上游把 `:one` 的任何错误都
//!    折成 404；只有真·未命中才 404（上游那 404 是 sqlc `:one` 的副产物）。
//!
//! # 时间戳
//!
//! 上游 `timestampToString` 是 `time.RFC3339`（**无**小数秒），本仓统一 `to_rfc3339()`
//! （带纳秒）—— 与 M8-2 的 `routes/vcs/dto.rs` 同一口径、同一偏离。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::mcp::workspace_server::{
    validate_entry, validate_name, McpServerValidationError, NewWorkspaceMcpServer,
    WorkspaceMcpServerRepo, WorkspaceMcpServerRow,
};
use mc_repos::RepoError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, forbidden, not_found, parse_uuid, workspace_role};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// `/api/workspaces/{id}/mcp-servers*`（M8-3 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/workspaces/:id/mcp-servers",
            get(list_workspace_mcp_servers).post(create_workspace_mcp_server),
        )
        .route(
            "/api/workspaces/:id/mcp-servers/:serverId",
            axum::routing::put(update_workspace_mcp_server).delete(delete_workspace_mcp_server),
        )
}

// ---------------------------------------------------------------------------
// 响应投影
// ---------------------------------------------------------------------------

/// 上游 `WorkspaceMcpServerResponse`：**刻意不含 secret 的形状**。
///
/// ⚠️ **不要**加 `url` / `command` / `args` / `headers` / `env` 字段
/// （`docs/61` §2.7 第 5 条；上游注释逐字「the stored entries are **write-only**」）。
/// `enabled` 只在 agent 面有意义（绑定开关），库列表里省略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerResponse {
    /// 条目 id。
    pub id: String,
    /// 所属 workspace。
    pub workspace_id: String,
    /// 条目名。
    pub name: String,
    /// 规范化 / 原样透传的 transport 字符串（见 [`mcp_transport_of`]）。
    pub transport: String,
    /// 绑定开关；库列表里缺席。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 更新时间（RFC3339）。
    pub updated_at: String,
}

impl McpServerResponse {
    /// workspace 库列表的单条（上游 `workspaceMcpServerToResponse`）。
    #[must_use]
    pub fn from_row(row: &WorkspaceMcpServerRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            name: row.name.clone(),
            transport: mcp_transport_of(&row.config),
            enabled: None,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }

    /// agent 面绑定的单条（上游 `ListAgentMcpServers` / `writeAgentMcpServers` 内联构造）。
    #[must_use]
    pub fn from_binding(row: &mc_repos::mcp::agent_binding::AgentMcpServerRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            name: row.name.clone(),
            transport: mcp_transport_of(&row.config),
            enabled: Some(row.enabled),
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

/// 上游 `mcpTransportOf`：把条目分类成展示用的 transport **字符串**。
///
/// 它同时驱动客户端的「引导式表单能不能编辑这个条目」判定 ⇒ **不得**把不认识的协议
/// 洗成认识的：把 `{"type":"websocket","url":"wss://…"}` 报成 `http` 会让设置表单打开、
/// 并在保存时把条目**改写成** `type: "http"`（静默换协议）。所以只有**没有显式 `type`**
/// 时才从 `command` / `url` 推断 —— 那是无损的，因为表单写回的就是同一形状。
///
/// ⚠️ `type` 的已知三值之外**原样返回（小写化）**：上游的 switch 只有
/// `local|stdio`、`remote|http|streamable-http` 两档，连字符那一支是 `streamable-http`
/// —— 写 `streamable_http`（下划线）时上游回的就是那个**原样字符串**。
/// （anchor 的 `McpTransport` 枚举把两种写法都归成 `Http`，那是领域层的窄口径；
/// 线格式是自由字符串，两者刻意不同，见 `docs/32` §16。）
#[must_use]
pub fn mcp_transport_of(config: &JsonValue) -> String {
    let Some(object) = config.as_object() else {
        return "unknown".to_string();
    };
    if let Some(declared) = object.get("type").and_then(JsonValue::as_str) {
        let declared = declared.trim().to_ascii_lowercase();
        if !declared.is_empty() {
            return match declared.as_str() {
                "local" | "stdio" => "stdio".to_string(),
                "remote" | "http" | "streamable-http" => "http".to_string(),
                // `sse` 与新客户端发明的任何值都落在这里，原样返回。
                _ => declared,
            };
        }
    }
    if object.contains_key("command") {
        return "stdio".to_string();
    }
    if object.contains_key("url") {
        return "http".to_string();
    }
    "unknown".to_string()
}

// ---------------------------------------------------------------------------
// 调用上下文
// ---------------------------------------------------------------------------

/// 一次请求的 `(workspace, 调用者, 角色)`。
struct WorkspaceMcpScope {
    workspace_id: Id,
    user_id: Id,
    role: String,
}

impl WorkspaceMcpScope {
    /// workspace id（400）→ 成员身份（非成员 404 `workspace`）。
    async fn resolve(
        state: &AppState,
        user: AuthUser,
        raw_workspace_id: &str,
    ) -> Result<Self, Error> {
        let workspace_id = Id(parse_uuid(raw_workspace_id, "workspace id")?);
        let user_id = user.id();
        let role = workspace_role(state, workspace_id, user_id).await?;
        Ok(Self {
            workspace_id,
            user_id,
            role,
        })
    }

    /// 上游 `requireWorkspaceMcpWriter` 的角色半段
    /// （`roleAllowed(member.Role, "owner", "admin")`）。失败 ⇒ 403。
    fn require_writer(&self) -> Result<(), Error> {
        if mc_repos::agent::role_is_admin(&self.role) {
            Ok(())
        } else {
            Err(forbidden("insufficient permissions"))
        }
    }
}

/// 条目校验错误 → 上游逐字的 400 文案。
fn validation_error(error: McpServerValidationError) -> Error {
    bad_request(error.to_string())
}

/// 仓储错误 → HTTP：`NotFound` ⇒ 404 `mcp server`，`Conflict` ⇒ 上游逐字的 409，
/// 其余 ⇒ 500（见模块头第 3 条）。
fn repo_error(error: RepoError) -> Error {
    match error {
        RepoError::NotFound => not_found("mcp server"),
        RepoError::Conflict => Error::Conflict {
            message: "an MCP server with this name already exists in the workspace".to_string(),
        },
        RepoError::Db(message) => Error::Database(message),
    }
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// 上游 `WorkspaceMcpServerRequest` 的解码结果。
///
/// `config` 用 `Option<JsonValue>` 而不是 `RawMessage`：serde 的 `Option` 会把
/// `{"config":null}` 与「键缺席」都折成 `None`，而 Go 的 `json.RawMessage` 对显式 `null`
/// 拿到的是**四个字节** `null`（`len > 0` ⇒ 走校验并 400）。⇒ 这里手工判：
/// `None` = 键缺席，`Some(Value::Null)` = 显式 null（校验时判「不是对象」）。
struct ServerRequest {
    name: Option<String>,
    config: Option<JsonValue>,
}

/// 解码 `POST` / `PUT` 的 body，对齐上游 `json.NewDecoder(r.Body).Decode(&req)`：
/// 空 body / 语法错误 / 非对象（含 `[]`）⇒ 400；字面 `null` ⇒ 零值结构体；
/// `name` 是 `null` ⇒ 零值空串，是其它类型 ⇒ 400（Go 解不进 `string` 字段）。
fn decode_server_request(body: &Bytes) -> Result<ServerRequest, Error> {
    let value: JsonValue =
        serde_json::from_slice(body).map_err(|_| bad_request("invalid request body"))?;
    let object: Map<String, JsonValue> = match value {
        JsonValue::Null => Map::new(),
        JsonValue::Object(object) => object,
        _ => return Err(bad_request("invalid request body")),
    };
    let name = match object.get("name") {
        None | Some(JsonValue::Null) => None,
        Some(JsonValue::String(name)) => Some(name.clone()),
        Some(_) => return Err(bad_request("invalid request body")),
    };
    Ok(ServerRequest {
        name,
        config: object.get("config").cloned(),
    })
}

/// 上游 `ListWorkspaceMcpServers`：**member 可见** —— 载荷不带凭据材料，而 agent owner
/// 需要看到「有什么可以加给自己的 agent」。
async fn list_workspace_mcp_servers(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(raw_workspace_id): Path<String>,
) -> ApiResult<Json<Vec<McpServerResponse>>> {
    let scope = WorkspaceMcpScope::resolve(&state, user, &raw_workspace_id).await?;
    let rows = WorkspaceMcpServerRepo::new(state.db.clone())
        .list_by_workspace(scope.workspace_id)
        .await
        .map_err(repo_error)?;
    Ok(Json(rows.iter().map(McpServerResponse::from_row).collect()))
}

/// 上游 `CreateWorkspaceMcpServer`：**admin** 动作，成功 **201**。
///
/// 新条目**绑给谁都不给** —— 库条目必须先被加到某个 agent 上才有作用，这正是本特性的形状。
/// 顺序逐条对齐：workspace（400）→ 成员（404）→ 角色（403）→ body（400）→ 名字（400）→
/// 条目形状（400）→ 落库（workspace 栅栏 / 重名 409）。
async fn create_workspace_mcp_server(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(raw_workspace_id): Path<String>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<McpServerResponse>)> {
    let scope = WorkspaceMcpScope::resolve(&state, user, &raw_workspace_id).await?;
    scope.require_writer()?;

    let request = decode_server_request(&body)?;
    let name = request.name.unwrap_or_default().trim().to_string();
    validate_name(&name).map_err(validation_error)?;
    // 键缺席与显式 `null` 在上游都是「不是对象」⇒ 同一条 400。
    let config = request.config.unwrap_or(JsonValue::Null);
    validate_entry(&config).map_err(validation_error)?;

    let row = WorkspaceMcpServerRepo::new(state.db.clone())
        .create(NewWorkspaceMcpServer {
            workspace_id: scope.workspace_id,
            name,
            config,
            created_by: Some(scope.user_id),
        })
        .await
        .map_err(|error| match error {
            // 栅栏没锁到 workspace ⇒ 它刚被删掉（上游 404 `workspace not found`）。
            RepoError::NotFound => not_found("workspace"),
            other => repo_error(other),
        })?;
    Ok((StatusCode::CREATED, Json(McpServerResponse::from_row(&row))))
}

/// 上游 `UpdateWorkspaceMcpServer`：**admin** 动作，成功 **200**。
///
/// `name` / `config` 各自「给了才动」（`COALESCE`）；`name` 去空白后为空 ⇒ 视为没给。
/// 重命名在这里是安全的：绑定以 id 为键，正在用它的 agent 不受影响。
async fn update_workspace_mcp_server(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((raw_workspace_id, raw_server_id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<McpServerResponse>> {
    let scope = WorkspaceMcpScope::resolve(&state, user, &raw_workspace_id).await?;
    scope.require_writer()?;
    let server_id = Id(parse_uuid(&raw_server_id, "server id")?);

    let request = decode_server_request(&body)?;
    // 上游 `if name := strings.TrimSpace(req.Name); name != ""`：去空白后为空 ⇒ 视为没给。
    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_string);
    if let Some(name) = name.as_deref() {
        validate_name(name).map_err(validation_error)?;
    }
    // 键存在（含显式 `null`）⇒ 校验并替换；键缺席 ⇒ 不动那一列。
    if let Some(config) = request.config.as_ref() {
        validate_entry(config).map_err(validation_error)?;
    }

    let row = WorkspaceMcpServerRepo::new(state.db.clone())
        .update(
            server_id,
            scope.workspace_id,
            name.as_deref(),
            request.config.as_ref(),
        )
        .await
        .map_err(repo_error)?;
    Ok(Json(McpServerResponse::from_row(&row)))
}

/// 上游 `DeleteWorkspaceMcpServer`：**admin** 动作，成功 **204**。
///
/// 一个事务里「`FOR UPDATE` 锁 server 行 → 删行 → 扫掉它的全部绑定」：绑定表没有 FK，
/// 残留的行会一直指向一个已经消失的 server。
async fn delete_workspace_mcp_server(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((raw_workspace_id, raw_server_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let scope = WorkspaceMcpScope::resolve(&state, user, &raw_workspace_id).await?;
    scope.require_writer()?;
    let server_id = Id(parse_uuid(&raw_server_id, "server id")?);

    let deleted = WorkspaceMcpServerRepo::new(state.db.clone())
        .delete(server_id, scope.workspace_id)
        .await
        .map_err(repo_error)?;
    if !deleted {
        return Err(not_found("mcp server").into());
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 上游 `TestMcpTransportOf` 逐条。
    #[test]
    fn transport_projection_matches_upstream() {
        for (entry, want) in [
            (json!({"type":"stdio","command":"x"}), "stdio"),
            (json!({"type":"local","command":"x"}), "stdio"),
            (json!({"type":"http","url":"https://x"}), "http"),
            (json!({"type":"streamable-http","url":"https://x"}), "http"),
            (json!({"type":"sse","url":"https://x"}), "sse"),
            // 回归点：未知 type **带 url** 也不得被当成 http。
            (json!({"type":"websocket","url":"wss://x"}), "websocket"),
            (json!({"type":"grpc","command":"x"}), "grpc"),
            (json!({"type":"WebSocket","url":"wss://x"}), "websocket"),
            // 只有「没声明 type」时才从 command / url 推断（无损）。
            (json!({"command":"x"}), "stdio"),
            (json!({"url":"https://x"}), "http"),
            (json!({}), "unknown"),
            (json!([]), "unknown"),
            (json!("not an object"), "unknown"),
            // 上游 switch 里连字符那一支是 `streamable-http`；下划线写法原样返回。
            (json!({"type":"streamable_http"}), "streamable_http"),
        ] {
            assert_eq!(mcp_transport_of(&entry), want, "{entry}");
        }
    }

    /// 上游 `{"type":null}`：`as_str` 落空 ⇒ 与「没声明」同判（Go 侧 `Type` 是 string，
    /// `null` 是 no-op ⇒ 同一个零值空串）。
    #[test]
    fn transport_projection_treats_a_null_type_as_absent() {
        assert_eq!(
            mcp_transport_of(&json!({"type":null,"url":"https://x"})),
            "http"
        );
    }

    /// 上游 `json.Decoder` 的三条行为：空 body / 非对象 ⇒ 400；`null` ⇒ 零值。
    #[test]
    fn request_decoding_matches_go() {
        assert!(decode_server_request(&Bytes::from_static(b"")).is_err());
        assert!(decode_server_request(&Bytes::from_static(b"[]")).is_err());
        assert!(decode_server_request(&Bytes::from_static(b"not json")).is_err());
        // `name` 是其它类型 ⇒ Go 解不进 `string` ⇒ 400。
        assert!(decode_server_request(&Bytes::from_static(br#"{"name":123}"#)).is_err());

        let null_body = decode_server_request(&Bytes::from_static(b"null")).expect("null");
        assert!(null_body.name.is_none() && null_body.config.is_none());

        let null_name =
            decode_server_request(&Bytes::from_static(br#"{"name":null}"#)).expect("name null");
        assert!(null_name.name.is_none());

        // **显式 `null` 与键缺席必须可区分**：Go 的 RawMessage 拿到 `null` 四个字节。
        let explicit =
            decode_server_request(&Bytes::from_static(br#"{"config":null}"#)).expect("config null");
        assert_eq!(explicit.config, Some(JsonValue::Null));
        let absent = decode_server_request(&Bytes::from_static(b"{}")).expect("empty object");
        assert_eq!(absent.config, None);
    }

    /// **write-only 的核心断言**：响应 DTO 的 JSON 里，条目的值一个字节都不出现。
    #[test]
    fn response_never_carries_the_entry() {
        let secret = "sk-live-workspace-should-never-be-echoed";
        let row = WorkspaceMcpServerRow {
            id: uuid::Uuid::from_u128(1),
            workspace_id: uuid::Uuid::from_u128(2),
            name: "linear".into(),
            config: json!({"url":"https://linear.example",
                           "headers":{"Authorization":format!("Bearer {secret}")}}),
            created_by: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let rendered = serde_json::to_string(&McpServerResponse::from_row(&row)).expect("json");
        assert!(!rendered.contains(secret), "{rendered}");
        // URL 本身就是凭据材料（上游注释逐字）。
        assert!(!rendered.contains("linear.example"), "{rendered}");
        // 库列表不带 `enabled`。
        assert!(!rendered.contains("enabled"), "{rendered}");
        assert!(rendered.contains("\"transport\":\"http\""), "{rendered}");
        assert!(rendered.contains("\"name\":\"linear\""), "{rendered}");
    }

    /// 校验错误 → 上游逐字 400 文案。
    #[test]
    fn validation_errors_reach_the_wire_verbatim() {
        assert_eq!(
            validation_error(McpServerValidationError::NameRequired).to_string(),
            "validation error: name is required"
        );
    }
}
