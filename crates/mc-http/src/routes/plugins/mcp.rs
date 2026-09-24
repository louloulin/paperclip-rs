//! 插件运行时面路由：调用历史 + remote MCP 工具采纳（**3 个注册键**）。
//!
//! - **写者**：M6-6（`docs/57` §3.2）。
//! - **上游**：`internal/handler/plugin_mcp.go`（+ `internal/handler/plugin_hook.go` 的
//!   invocations 段 + `internal/service/plugin_mcp_transport.go` 的**前半**）；`pkg/remotemcp`
//!   的 `tools/list` 由 M6-1 的 `mc_mcp::client` 提供（本文件只调它）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/workspaces/:id/plugins/:installationId/invocations` | GET | `router.go:1737` |
//! | `/api/workspaces/:id/plugins/:installationId/mcp/:hookKey/tools` | GET, PUT | `router.go:1742-1743` |
//!
//! - **两条硬语义**：
//!   1. `GET tools` 的返回是**当前远端**的工具列表（出网 → 超时要映射成明确错误，不能挂住
//!      请求）；`PUT tools` 是**采纳**动作，落 `plugin_installation.mcp_approvals` 的 JSONB；
//!   2. 采纳要与远端**逐条交叉校验**（名字 + `schema_digest`）：远端变了 ⇒ 视为**未采纳**，
//!      要求重新采纳 —— 判据在 `mc_mcp::client`，本文件不要自己比。
//! - **出网边界**：remote MCP 的 HTTP 只走 `mc_mcp`（`reqwest` 那条依赖在 `mc-mcp` 里，
//!   `mc-http` 也有 `reqwest` 但本文件不要用它去直接调 MCP）。
//! - **不做什么**：不做 surface 启动（`surface_launch.rs`）、不做 token 签发（`install.rs`）、
//!   不做 daemon broker 的 `validatePinnedRemoteMcpTools`（M6-9）、不做 hook 引擎（M6-8）。
//!
//! # M6-6 落地说明（LUM-1671）
//!
//! 1. **门与上游一致**：这三条路由在上游的管理员组里（`RequireWorkspaceRoleFromURL(owner,
//!    admin)`），且都经过 `pluginInstallationFromURL` ⇒ 开关门（`plugins_v1`）+ 管理员门 +
//!    「安装行必须属于路径里的 workspace」。顺序照 `install.rs` 的既有约定：
//!    `require_plugins_v1` → `workspace_admin` → 安装行。
//! 2. **分页是本片新增的能力**：上游 `ListPluginInvocations` 硬编码 `LIMIT 100`、无 offset。
//!    本片暴露 `?limit=&offset=`（缺省 `100` / `0` 与上游逐字相同），上限 500 —— 登记在
//!    `mc-repos/src/plugin/invocation_read.rs` 与 `docs/32` §9.8。查询串**手解**而不是
//!    `Query<T>`：后者的拒绝体是 axum 的纯文本，会与插件面的 JSON 信封不一致。
//! 3. **存的是窄形态**：`mcp_approvals` 里落 `mc_core::plugin::PluginApprovedTool`
//!    （`name` + `schema_digest`），不是上游那份带 `description` / `inputSchema` 的
//!    `remotemcp.Tool` —— 迁移 `369` 的注释与 `mc-core` 的头表就是这个形状（见
//!    `mc_repos::plugin::mcp_approval` 的文件头）。比对（本文件唯一关心的语义）只用这两个字段。
//! 4. **`workspace_mcp_api.go` 的台账段不在本片**：`/api/workspaces/:id/mcp-servers` 四条
//!    （`router.go:1686/1710-1712`）在 `docs/fixtures/upstream-routes.tsv` 里归属 **M8**，
//!    本片只做 `plugin_*` 的三条。`mcp_overlay.go` 是 per-task agent overlay（M8 面），同理不接。
//!
//! **状态：M6-6 已落地（LUM-1671）**。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use mc_core::plugin::{PluginApprovedTool, PluginMcpApproval};
use mc_core::Id;
use mc_mcp::client::{discover, Discovery};
use mc_mcp::devorigin::{EndpointPolicy, DEV_CA_ENV, DEV_ORIGINS_ENV};
use mc_mcp::types::Tool;
use mc_plugin_host::credentials::open_secret;
use mc_plugin_host::manifest::{Hook, Manifest, CONFIG_SECRET};
use mc_plugin_host::scope::net_domains;
use mc_repos::plugin::invocation_read::{
    PluginInvocationRepo, PluginInvocationRow, DEFAULT_LIMIT, MAX_LIMIT,
};
use mc_repos::plugin::mcp_approval::{PluginApprovalRepo, PluginInstallationRow};
use mc_repos::RepoError;

use super::install::{
    deployment_key, installation_payload, installation_repo, parse_installation_manifest,
    require_plugins_v1, timestamp, workspace_admin, PluginError, PluginResult,
};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 上游 `plugincontract.TransportMCP`（`hook.transport.type` 的字面量）。
const TRANSPORT_MCP: &str = "mcp";

/// `/api/workspaces/:id/plugins/:installationId/{invocations,mcp/*}`（M6-6 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/workspaces/:id/plugins/:installationId/invocations",
            get(list_invocations),
        )
        .route(
            "/api/workspaces/:id/plugins/:installationId/mcp/:hookKey/tools",
            get(list_mcp_tools).put(approve_mcp_tools),
        )
}

// ---------------------------------------------------------------------------
// 门
// ---------------------------------------------------------------------------

/// 上游 `pluginInstallationFromURL`：开关门 → workspace 管理员门 → 安装行。
///
/// 安装行取的是本片的**窄读投影**（`PluginInstallationRow`）—— 启动一个 MCP 端点需要的列
/// 只有 `manifest` / `granted_scopes` / `mcp_approvals` / `enabled`（见
/// `mc_repos::plugin::mcp_approval` 的差异 3）。
async fn load_installation(
    state: &AppState,
    user_id: Id,
    raw_workspace: &str,
    raw_installation: &str,
) -> PluginResult<(Id, PluginInstallationRow)> {
    require_plugins_v1(state)?;
    let workspace_id = workspace_admin(state, raw_workspace, user_id).await?;
    let installation = PluginApprovalRepo::new(state.db.clone())
        .installation_for_workspace(workspace_id, raw_installation)
        .await
        .map_err(|err| match err {
            RepoError::NotFound => PluginError::not_found("plugin installation not found"),
            _ => PluginError::unavailable("load the Plugin"),
        })?;
    Ok((workspace_id, installation))
}

// ---------------------------------------------------------------------------
// ① 调用记录（`GET …/invocations`）
// ---------------------------------------------------------------------------

/// 上游没有这两个参数（见文件头说明 2）；`<= 0` / 非数字一律回缺省值 —— 分页参数是**能力**，
/// 拼错一个不该让排障视图变成 400。
fn page_params(raw: Option<&str>) -> (i64, i64) {
    let mut limit = DEFAULT_LIMIT;
    let mut offset = 0_i64;
    for pair in raw.unwrap_or_default().split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let Ok(parsed) = value.parse::<i64>() else {
            continue;
        };
        match key {
            "limit" if parsed > 0 => limit = parsed.min(MAX_LIMIT),
            "offset" if parsed > 0 => offset = parsed,
            _ => {}
        }
    }
    (limit, offset)
}

/// 上游 `pluginInvocationResponse`。`installation_id` / `workspace_id` **不下发**：
/// 路径已经钉死了安装，上游也不回这两列。
fn invocation_payload(row: &PluginInvocationRow) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(row.id.to_string()));
    out.insert("hook_key".into(), json!(row.hook_key));
    out.insert("trigger".into(), json!(row.trigger));
    out.insert("status".into(), json!(row.status));
    if let Some(event_type) = &row.event_type {
        out.insert("event_type".into(), json!(event_type));
    }
    out.insert("attempt".into(), json!(row.attempt));
    out.insert("latency_ms".into(), json!(row.latency_ms));
    if let Some(error) = &row.error {
        out.insert("error".into(), json!(error));
    }
    if let Some(delivery_id) = &row.delivery_id {
        out.insert("delivery_id".into(), json!(delivery_id));
    }
    if let Some(planned_at) = row.planned_at {
        out.insert("planned_at".into(), json!(timestamp(planned_at)));
    }
    out.insert("created_at".into(), json!(timestamp(row.created_at)));
    Value::Object(out)
}

/// `GET /api/workspaces/:id/plugins/:installationId/invocations` —— **管理员可见**。
async fn list_invocations(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, installation)): Path<(String, String)>,
    RawQuery(query): RawQuery,
) -> Response {
    let (limit, offset) = page_params(query.as_deref());
    let inner = async {
        let (_, installation) =
            load_installation(&state, auth.id(), &workspace, &installation).await?;
        let rows = PluginInvocationRepo::new(state.db.clone())
            .list(installation.id(), limit, offset)
            .await
            .map_err(|_| PluginError::unavailable("failed to load the Plugin activity"))?;
        Ok::<Value, PluginError>(json!({
            "invocations": rows.iter().map(invocation_payload).collect::<Vec<_>>(),
        }))
    }
    .await;
    match inner {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

// ---------------------------------------------------------------------------
// ② remote MCP 工具（`GET|PUT …/mcp/:hookKey/tools`）
// ---------------------------------------------------------------------------

/// 上游 `FindHook` + `DiscoverMCPHookTools` 的传输判定。
fn find_mcp_hook<'a>(manifest: &'a Manifest, hook_key: &str) -> PluginResult<&'a Hook> {
    let Some(hook) = manifest
        .contributes
        .hooks
        .iter()
        .find(|hook| hook.key == hook_key)
    else {
        return Err(PluginError::not_found(format!(
            "this Plugin has no hook named {hook_key:?}"
        )));
    };
    if hook.transport.kind != TRANSPORT_MCP {
        return Err(PluginError::invalid(format!(
            "hook {hook_key:?} is not an mcp transport"
        )));
    }
    Ok(hook)
}

/// 入口层的 dev-origin 策略装配。
///
/// `mc_mcp::devorigin` 只导出两个 env 名字、**不读值**（它的头注：读取集中到入口层）；
/// 上游 `isDevOrigin` / `devTLSConfig` 是在包里直接查 env 的，本仓按入口层约定在这里读。
/// 未设置 ⇒ 空白名单 / 无额外 CA，与上游「没配就是没有」同判。
fn endpoint_policy(allowed_hosts: &[String]) -> EndpointPolicy {
    let origins = std::env::var(DEV_ORIGINS_ENV).unwrap_or_default();
    let ca = std::env::var(DEV_CA_ENV)
        .ok()
        .and_then(|path| std::fs::read(path).ok());
    EndpointPolicy::from_values(allowed_hosts, &origins, ca)
}

/// 上游 `mcpCredentialHeaders`：manifest 声明了 `<hookKey>_credential`（且类型是 `secret`）
/// 才附 `Authorization`。
///
/// 三条**降级为「不带头」**的分支都与上游一致：没声明、声明了但没设值、密文解不开 ——
/// 「声明了却没配」是配置缺口，不是让发现以令人困惑的传输错误失败的理由。
async fn mcp_credential_headers(
    state: &AppState,
    installation: &PluginInstallationRow,
    manifest: &Manifest,
    hook: &Hook,
) -> PluginResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    let key = format!("{}_credential", hook.key);
    let declared = manifest
        .config
        .field(&key)
        .is_some_and(|field| field.kind == CONFIG_SECRET);
    if !declared {
        return Ok(headers);
    }
    let sealed = PluginApprovalRepo::new(state.db.clone())
        .secret_ciphertext(installation.id(), &key)
        .await
        .map_err(|_| PluginError::unavailable("read the Plugin secret"))?;
    let Some(sealed) = sealed else {
        return Ok(headers);
    };
    let key = deployment_key(state);
    let Ok(plaintext) = open_secret(key.as_ref(), &sealed) else {
        return Ok(headers);
    };
    // 非 UTF-8 / 含非法头字符的「明文」不是一个能用的凭据：不带头，而不是发一段乱码。
    let Ok(text) = std::str::from_utf8(&plaintext) else {
        return Ok(headers);
    };
    if let Ok(value) = HeaderValue::from_str(text) {
        headers.insert(HeaderName::from_static("authorization"), value);
    }
    Ok(headers)
}

/// 上游 `DiscoverMCPHookTools`：问 hook 的 MCP 服务器「你现在提供什么」。
///
/// **发现不采纳任何东西** —— 采纳是 `PUT` 那一步，这一步只读。
async fn discover_hook_tools(
    state: &AppState,
    installation: &PluginInstallationRow,
    manifest: &Manifest,
    hook: &Hook,
) -> PluginResult<Discovery> {
    // 第二层：manifest 校验已经拒绝「transport URL 没被某个 `net:` scope 精确覆盖」的 hook，
    // 所以对装得干净的插件这不可达 —— 留着是因为下一条出网判据的白名单取在这里，而**空集合
    // 必须失败闭合**，读成「没有限制」就等于放行。
    let domains = net_domains(&installation.granted_scopes());
    if domains.is_empty() {
        return Err(PluginError::forbidden(
            "this Plugin was granted no net: scope, so it cannot reach an MCP server",
        ));
    }
    let headers = mcp_credential_headers(state, installation, manifest, hook).await?;
    discover(
        &hook.transport.url,
        &endpoint_policy(&domains),
        &[],
        &headers,
    )
    .await
    .map_err(|err| {
        // 上游把原因留在 `PluginError.Err`（只进日志/链路），响应里只有这一句；
        // 本仓保留同一分工，但**必须**把原因记进日志，否则超时与证书错误无从区分。
        tracing::warn!(hook = %hook.key, error = %err, "remote MCP discovery failed");
        PluginError::unavailable("could not reach the Plugin's MCP server")
    })
}

/// 上游 `pluginMCPToolResponse`。`approved` 恒在；`drifted` **只在为真时出现** ——
/// 「已批准但摘要变了」是被显式呈现的第三种状态，不是静默的重新批准。
fn mcp_tool_payload(tool: &Tool, pinned: Option<&PluginApprovedTool>) -> Value {
    let mut out = Map::new();
    out.insert("name".into(), json!(tool.name));
    out.insert("description".into(), json!(tool.description));
    out.insert("schema_digest".into(), json!(tool.schema_digest));
    out.insert("approved".into(), json!(pinned.is_some()));
    if pinned.is_some_and(|pinned| pinned.schema_digest != tool.schema_digest) {
        out.insert("drifted".into(), json!(true));
    }
    Value::Object(out)
}

/// `GET /api/workspaces/:id/plugins/:installationId/mcp/:hookKey/tools` —— **管理员可见**。
async fn list_mcp_tools(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, installation, hook_key)): Path<(String, String, String)>,
) -> Response {
    let inner = async {
        let (_, installation) =
            load_installation(&state, auth.id(), &workspace, &installation).await?;
        let manifest = parse_installation_manifest(&installation.manifest)?;
        let hook = find_mcp_hook(&manifest, &hook_key)?;
        let discovered = discover_hook_tools(&state, &installation, &manifest, hook).await?;
        // 已批准集合取自**这次读到的行**（上游 `ApprovedMCPTools(installation, hookKey)`）。
        let approved = installation.mcp_approvals();
        let pinned = approved.get(&hook_key);
        let tools = discovered
            .tools
            .iter()
            .map(|tool| {
                mcp_tool_payload(
                    tool,
                    pinned.and_then(|approval| {
                        approval
                            .tools
                            .iter()
                            .find(|candidate| candidate.name == tool.name)
                    }),
                )
            })
            .collect::<Vec<_>>();
        Ok::<Value, PluginError>(json!({ "tools": tools }))
    }
    .await;
    match inner {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

/// 上游 `approvePluginMCPToolsRequest`：`tools` 是**完整集合**，不是增量 ——
/// 删掉一个与加一个是同一种请求形状，所以「以为撤回了、其实还在」这种理解不可能发生。
#[derive(Debug, Default, Deserialize)]
struct ApproveToolsRequest {
    #[serde(default)]
    tools: Vec<String>,
}

/// 上游 `ApproveMCPHookTools` 的钉定段。
///
/// 摘要取自**这次发现的结果**（不是客户端给的）：批准的是「远端此刻的那份 schema」。
/// 空集合 = **撤回**（删掉该 hook 的键，而不是存一个读起来像「批准了、只是里面没东西」的空清单）。
fn pin_tools(
    discovered: &[Tool],
    names: &[String],
    user_id: Id,
) -> PluginResult<Option<PluginMcpApproval>> {
    if names.is_empty() {
        return Ok(None);
    }
    let mut tools = Vec::with_capacity(names.len());
    for name in names {
        let Some(tool) = discovered.iter().find(|candidate| &candidate.name == name) else {
            // 批准一个服务器此刻没提供的名字 = 钉一个没有 schema 的名字，broker 会在启动时
            // 拒掉整个连接。所以在这里就拒。
            return Err(PluginError::invalid(format!(
                "tool {name:?} is not offered by this MCP server"
            )));
        };
        tools.push(PluginApprovedTool {
            name: tool.name.clone(),
            schema_digest: tool.schema_digest.clone(),
        });
    }
    Ok(Some(PluginMcpApproval {
        tools,
        approved_at: mc_core::Timestamp::now(),
        approved_by: Some(user_id),
    }))
}

/// `PUT /api/workspaces/:id/plugins/:installationId/mcp/:hookKey/tools` —— **管理员可见**。
///
/// 响应是**整个安装行**（上游 `pluginInstallationPayload(updated)`），与 `/plugins` 列表同 DTO。
async fn approve_mcp_tools(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, installation, hook_key)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    let inner = async {
        let (workspace_id, installation) =
            load_installation(&state, auth.id(), &workspace, &installation).await?;
        let manifest = parse_installation_manifest(&installation.manifest)?;
        let hook = find_mcp_hook(&manifest, &hook_key)?;
        if body.is_empty() {
            return Err(PluginError::invalid("invalid request body"));
        }
        let request: ApproveToolsRequest = serde_json::from_slice(&body)
            .map_err(|_| PluginError::invalid("invalid request body"))?;
        let discovered = discover_hook_tools(&state, &installation, &manifest, hook).await?;
        let approval = pin_tools(&discovered.tools, &request.tools, auth.id())?;
        PluginApprovalRepo::new(state.db.clone())
            .set_hook_approval(installation.id(), &hook_key, approval.as_ref())
            .await
            .map_err(|err| match err {
                RepoError::NotFound => PluginError::not_found("plugin installation not found"),
                _ => PluginError::unavailable("store approvals"),
            })?;
        // 回读整行：`pluginInstallationPayload` 需要 config / secrets / created_at 这些
        // 本片窄投影里没有的列。
        let row = installation_repo(&state)
            .get(workspace_id, installation.id())
            .await
            .map_err(|_| PluginError::unavailable("load the Plugin"))?;
        installation_payload(&state, &row).await
    }
    .await;
    match inner {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

// ---------------------------------------------------------------------------
// 单元测试：纯判定（不接库、不出网）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mc_mcp::client::validate_pinned_tools;
    use serde_json::json;

    fn tool(name: &str, digest: &str) -> Tool {
        Tool {
            name: name.into(),
            description: format!("{name} tool"),
            input_schema: json!({ "type": "object" }),
            schema_digest: digest.into(),
            risk: String::new(),
        }
    }

    fn manifest_with(contributes: &Value) -> Manifest {
        serde_json::from_value(json!({
            "manifest_version": 1,
            "key": "com.example.demo",
            "name": "Demo",
            "version": "1.0.0",
            "author": { "name": "itest" },
            "scopes": ["net:mcp.example.com"],
            "contributes": contributes.clone(),
        }))
        .expect("fixture manifest")
    }

    fn mcp_hook(key: &str) -> Value {
        json!({ "hooks": [ { "key": key, "name": "Toolbox", "description": "d",
                             "triggers": ["agent"],
                             "transport": { "type": "mcp", "url": "https://mcp.example.com/rpc" } } ] })
    }

    #[test]
    fn unknown_hook_is_404_and_http_transport_is_400() {
        let manifest = manifest_with(&mcp_hook("toolbox"));
        assert!(matches!(
            find_mcp_hook(&manifest, "nope"),
            Err(PluginError {
                status: StatusCode::NOT_FOUND,
                ..
            })
        ));
        assert!(matches!(
            find_mcp_hook(&manifest, "toolbox"),
            Ok(hook) if hook.transport.kind == "mcp"
        ));

        let http = manifest_with(
            &json!({ "hooks": [ { "key": "sync", "name": "Sync", "description": "d",
            "triggers": ["manual"], "transport": { "type": "http", "url": "https://example.com/hook" } } ] }),
        );
        assert!(matches!(
            find_mcp_hook(&http, "sync"),
            Err(PluginError {
                status: StatusCode::BAD_REQUEST,
                ..
            })
        ));
    }

    #[test]
    fn pinning_takes_the_digest_from_discovery_and_rejects_unknown_names() {
        let user = Id::from(uuid::Uuid::new_v4());
        let discovered = vec![tool("search", "aa11"), tool("lookup", "bb22")];

        let pinned = pin_tools(&discovered, &["search".into()], user)
            .expect("approve")
            .expect("non-empty set");
        assert_eq!(pinned.tools.len(), 1);
        assert_eq!(pinned.tools[0].name, "search");
        assert_eq!(pinned.tools[0].schema_digest, "aa11");
        assert_eq!(pinned.approved_by, Some(user));

        // 客户端说了一个远端没提供的名字 —— 不能钉。
        assert!(matches!(
            pin_tools(&discovered, &["ghost".into()], user),
            Err(PluginError {
                status: StatusCode::BAD_REQUEST,
                ..
            })
        ));

        // 空集合 = 撤回，不是一个空的批准。
        assert!(pin_tools(&discovered, &[], user).expect("ok").is_none());
    }

    /// `DoD`：**manifest/schema 摘要变化 ⇒ 旧批准失效**（反例）。
    ///
    /// 三段都要成立，缺一段就会出现「静默沿用旧授权」：
    /// ① 呈现层把它标成 `drifted`（管理员看得见「这不是我批的那份」）；
    /// ② broker 侧的判据 `validate_pinned_tools` 直接拒（M6-9 的运行时效果）；
    /// ③ 摘要变了的那次**重新采纳**取的是新摘要（不是把旧的留着）。
    #[test]
    fn a_changed_schema_digest_invalidates_the_old_approval() {
        let user = Id::from(uuid::Uuid::new_v4());
        let approved = vec![tool("search", "aa11")];
        let pinned = pin_tools(&approved, &["search".into()], user)
            .expect("approve")
            .expect("non-empty");

        // 远端换了 schema ⇒ 同名、摘要不同。
        let drifted = vec![tool("search", "aa99")];
        let payload = mcp_tool_payload(&drifted[0], pinned.tools.first());
        assert_eq!(payload["approved"], json!(true));
        assert_eq!(payload["drifted"], json!(true));
        assert_eq!(payload["schema_digest"], json!("aa99"));

        // M6-9 的 broker 在启动时用同一条判据拒掉整条连接（上游 `validatePinnedRemoteMcpTools`）。
        let err = validate_pinned_tools(&approved, &drifted).expect_err("drift must refuse");
        assert!(err.to_string().contains("schema drifted"), "{err}");

        // 名字消失也一样拒（而不是「少了一个工具」被容忍）。
        assert!(validate_pinned_tools(&approved, &[]).is_err());

        // 未漂移时不得出现 `drifted` 键。
        let same = [tool("search", "aa11")];
        let payload = mcp_tool_payload(&same[0], pinned.tools.first());
        assert_eq!(payload["approved"], json!(true));
        assert!(payload.get("drifted").is_none());

        // 未批准的工具：`approved=false` 且没有 `drifted`。
        let payload = mcp_tool_payload(&same[0], None);
        assert_eq!(payload["approved"], json!(false));
        assert!(payload.get("drifted").is_none());

        // 重新采纳取新摘要。
        let repinned = pin_tools(&drifted, &["search".into()], user)
            .expect("re-approve")
            .expect("non-empty");
        assert_eq!(repinned.tools[0].schema_digest, "aa99");
    }

    #[test]
    fn page_params_default_to_the_upstream_values_and_clamp() {
        assert_eq!(page_params(None), (DEFAULT_LIMIT, 0));
        assert_eq!(page_params(Some("")), (DEFAULT_LIMIT, 0));
        assert_eq!(page_params(Some("limit=10&offset=20")), (10, 20));
        // 上限 500；0 / 负数 / 非数字一律回缺省（分页是能力，不是拒服务的理由）。
        assert_eq!(page_params(Some("limit=1000000")), (MAX_LIMIT, 0));
        assert_eq!(page_params(Some("limit=0")), (DEFAULT_LIMIT, 0));
        assert_eq!(page_params(Some("limit=-3&offset=-9")), (DEFAULT_LIMIT, 0));
        assert_eq!(
            page_params(Some("limit=abc&offset=xyz")),
            (DEFAULT_LIMIT, 0)
        );
        assert_eq!(page_params(Some("cursor=1")), (DEFAULT_LIMIT, 0));
    }

    /// `https://mcp.example.com/rpc` 的 host 是 `mcp.example.com`、不在授权域里 ⇒ 端点策略
    /// 必须拒（这条链的判据在 `mc_mcp`，这里只钉「handler 传下去的是**授权域**而不是空表」）。
    #[test]
    fn endpoint_policy_is_built_from_the_granted_domains() {
        let policy = endpoint_policy(&["mcp.example.com".to_string()]);
        assert!(policy.allows_host("mcp.example.com"));
        assert!(!policy.allows_host("evil.example.com"));
    }
}
