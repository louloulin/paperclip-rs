//! `/v1/context`（**1 个注册键**）+ 该 handler 的**共享实现**（bridge 面复用）。
//!
//! - **写者**：M6-7（`docs/57` §3.2）。
//! - **上游**：`router.go:103` 的挂载点，加上 `internal/handler/plugin_action.go` 的
//!   `GetPluginContext` 与 `internal/service/plugin_action.go` 的 `BuildPluginContext`。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/v1/context` | GET | `router.go:103` |
//!
//! - **共享实现**：`/api/plugin-bridge/v1/context` 是**同一条** handler 挂在另一个前缀上
//!   （上游逐字如此）⇒ 本文件的 `get_context` 被 `routes/plugin_bridge/context.rs` 直接注册，
//!   于是 `Context` 的字段投影**只有一份**（两份会在 bridge/公开面之间漂移，而本片 `DoD` 的
//!   硬项正是「同一请求经两侧的响应**字节**相同」）。
//! - **返回内容**：调用者（插件）/工作区/可见的 surface 等上下文，按凭据种类收窄；
//!   **绝不**回显密钥值 —— `config` 是安装行的**非 secret** 字段（上游注释：密文在
//!   `plugin_secret`，那张表没有任何把 ciphertext 交给 handler 的读口）。
//! - **`issue_id` 查询参数**：给了就要走第三步授权（`plugin_issue_for_caller`），
//!   拿不到就是 404 —— 插件不能借这个参数确认一个它读不到的 id 是否存在。
//! - **不做什么**：不在这里做凭据校验（`policy.rs`）。
//!
//! 行预算（门 ⑩）：本文件 ≤260 行。

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;

use mc_openapi::v1::{Context, ContextIssue, ContextUser, ContextWorkspace};
use mc_plugin_host::scope::net_domains;

use super::issues::plugin_issue_for_caller;
use super::policy::{self as policy, ActionError, ActionResult};
use crate::state::AppState;

/// `GET /v1/context` 的查询串（上游 `r.URL.Query().Get("issue_id")`）。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ContextQuery {
    #[serde(default)]
    pub(crate) issue_id: Option<String>,
}

/// `/v1/context`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/v1/context", get(get_context))
}

/// `GET /v1/context`：凭据 + 安装 + （可选）issue 三步之后给出启动上下文。
pub(crate) async fn get_context(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ContextQuery>,
) -> Response {
    let request_id = policy::request_id(&headers);
    match get_context_inner(&state, &headers, &query).await {
        Ok(response) => response,
        Err(error) => error.into_response_for(&request_id),
    }
}

async fn get_context_inner(
    state: &AppState,
    headers: &HeaderMap,
    query: &ContextQuery,
) -> ActionResult<Response> {
    // 无 scope：这是用户**本来就看着的那个页面**，不含任何他看不到的东西。
    let caller = policy::resolve_caller(state, headers, "").await?;

    let workspace = load_workspace(state, caller.workspace_id).await?;

    // plugin actor 没有可描述的人，载荷就**明说**没有，而不是给出一个空壳让 handler 读成真的。
    let user = match caller.actor {
        policy::ActionActor::Member(user_id) => Some(load_user(state, user_id).await?),
        policy::ActionActor::Plugin => None,
    };

    let issue = match query
        .issue_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(issue_ref) => Some(plugin_issue_for_caller(state, &caller, issue_ref).await?),
        None => None,
    };

    let payload = Context {
        workspace: ContextWorkspace {
            id: workspace.0.to_string(),
            name: workspace.1,
            slug: workspace.2,
        },
        user: user.map(|(id, name)| ContextUser {
            id: id.to_string(),
            name,
        }),
        issue: issue.map(|row| ContextIssue {
            id: row.id.to_string(),
            identifier: row.identifier.clone(),
            title: row.title.clone(),
        }),
        config: caller
            .installation
            .config_object()
            .into_iter()
            .collect::<BTreeMap<String, Value>>(),
        granted_net_domains: net_domains(&caller.scopes),
        actor: if caller.is_member() {
            "member"
        } else {
            "plugin"
        }
        .to_string(),
    };
    Ok(Json(payload).into_response())
}

/// workspace 的 `(id, name, slug)`。
///
/// 不用 `WorkspaceRepo`：它的读口只有 `get_by_slug` / `list_for_user`（没有按 id 取），本片又
/// 不得编辑那个文件 ⇒ 这里只取三个字段的直查（比 `WorkspaceRow` 的 8 个字段窄，
/// 且不引入第二份 workspace 领域转换）。
async fn load_workspace(
    state: &AppState,
    workspace_id: mc_core::Id,
) -> ActionResult<(mc_core::Id, String, String)> {
    let row = sqlx::query_as::<_, (uuid::Uuid, String, String)>(
        "SELECT id, name, slug FROM workspace WHERE id = $1",
    )
    .bind(workspace_id.0)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|error| ActionError::unavailable(format!("load the workspace: {error}")))?;
    let (id, name, slug) = row.ok_or_else(|| ActionError::not_found("workspace not found"))?;
    Ok((mc_core::Id(id), name, slug))
}

/// 用户摘要 `(id, name)`。**不含 email**：要向上游后端标识成员，用不透明 id，不用邮箱。
async fn load_user(state: &AppState, user_id: mc_core::Id) -> ActionResult<(mc_core::Id, String)> {
    let row =
        sqlx::query_as::<_, (uuid::Uuid, String)>("SELECT id, name FROM \"user\" WHERE id = $1")
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|error| ActionError::unavailable(format!("load the user: {error}")))?;
    let (id, name) = row.ok_or_else(|| ActionError::not_found("user not found"))?;
    Ok((mc_core::Id(id), name))
}
