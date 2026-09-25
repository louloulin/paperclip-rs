//! GitHub 安装与仓库浏览面：**4 条**路由（`router.go:1757/1672/1758/1759`）—— 写者 **M8-1**。
//!
//! | 注册键 | 方法 | 授权层 | 未配置语义 |
//! | --- | :-: | --- | --- |
//! | `/api/workspaces/:id/github/connect` | GET | workspace **admin** | 200 + `configured:false`（**不是** 503） |
//! | `/api/workspaces/:id/github/installations` | GET | workspace **member** | 无（本地库，不依赖外部密钥） |
//! | `/api/workspaces/:id/github/installations/:installationId/repositories` | GET | workspace **admin** | **403** `github_repository_browsing_not_configured` |
//! | `/api/workspaces/:id/github/installations/:installationId` | DELETE | workspace **admin** | 无（只删本仓的行） |
//!
//! 授权层来自上游 `router.go`：member 组 1672、admin 组 1757–1759。本仓没有 per-route 的
//! workspace 角色中间件（与 M3-5 / M4-2 / M6 各面同款），所以**在 handler 起点**用
//! [`workspace_role`] 解析 —— 非成员 **404 `workspace`**、角色不够 **403
//! `insufficient permissions`**，与上游 `buildMiddleware` 逐条同形。
//!
//! # 「未配置」逐端点不同（`docs/61` §2.5）—— 不许统一 403/503
//!
//! - `connect`：用「能连接」判据（`GITHUB_APP_SLUG` + `GITHUB_WEBHOOK_SECRET`）⇒ **200 +
//!   `configured:false`**，前端据此隐藏按钮（上游 `GitHubConnect` 的第一段）；
//! - `installations`：**没有**未配置分支；
//! - `repositories`：用「能浏览仓库」判据（`GITHUB_APP_ID` + `GITHUB_APP_PRIVATE_KEY`）⇒
//!   **403 + `github_repository_browsing_not_configured`**（上游 `writeFeatureDisabled`
//!   **故意用 403 而不是 503**：被关掉的能力不是瞬时故障，回 503 会招来重试与告警噪音）；
//! - `delete`：不碰 GitHub ⇒ 没有未配置语义。
//!
//! # 离线替身（`docs/61` §4.2）
//!
//! [`github_api_base`] 是本仓对上游 `var githubAPIBase`（`github.go:34-36`，注释逐字
//! 「Mutable so tests can…」）的等价物；测试用它把整条链指到本地替身，**中间零 mock**。
//!
//! # 凭据纪律（`docs/61` §2.4）
//!
//! 响应里**永不**出现 App 私钥 / installation token；App 私钥只在 [`AppJwtSigner::from_pem`]
//! 里被读一次造签名器，出错时只留**原因名**；换来的 token 用完**尽力而为**地撤销（撤销失败
//! 不回滚响应，与上游 `defer revokeGitHubInstallationToken` 同语义）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use mc_core::Id;
use mc_errors::{Error, ErrorBody, ErrorResponse};
use mc_repos::agent::role_is_admin;
use mc_repos::github::installation::GithubInstallationRepo;
use mc_vcs_github::dto::{
    GithubConnectResponse, GithubInstallationResponse, GithubInstallationsResponse,
    GithubPageParam, GithubRepositoriesResponse,
};
use mc_vcs_github::rest::GithubClient;
use mc_vcs_github::AppJwtSigner;

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, forbidden, not_found, parse_uuid, workspace_role};
use crate::routes::auth_user::AuthUser;
use crate::routes::github::setup::{is_allowed_return_to, sign_state_for_return, RETURN_TO_GITHUB};
use crate::state::AppState;

/// 「能浏览仓库」被关掉时的稳定错误码（上游 `github.go:765` 逐字）。
pub const CODE_REPOSITORY_BROWSING_NOT_CONFIGURED: &str =
    "github_repository_browsing_not_configured";

/// 本文件的路由切片。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/workspaces/:id/github/connect", get(connect))
        .route(
            "/api/workspaces/:id/github/installations",
            get(list_installations),
        )
        .route(
            "/api/workspaces/:id/github/installations/:installationId/repositories",
            get(list_repositories),
        )
        .route(
            "/api/workspaces/:id/github/installations/:installationId",
            delete(delete_installation),
        )
}

// ---------------------------------------------------------------------------
// 调用上下文
// ---------------------------------------------------------------------------

/// 一次请求的 `(workspace, 角色)`（上游 `RequireWorkspaceMemberFromURL` 解析出来的那两样）。
///
/// `user_id` 不再单独存：本面四条路由的判定只看「是不是成员 + 是不是 owner/admin」，
/// 调用者 id 已经用在 `workspace_role` 查询里（与上游 `SetMemberContext` 后不再读 user id 同形）。
struct GithubScope {
    workspace_id: Id,
    role: String,
}

impl GithubScope {
    /// workspace id（400 `workspace id must be a valid uuid`）→ 成员身份（非成员 404
    /// `workspace`）。
    async fn resolve(
        state: &AppState,
        user: AuthUser,
        raw_workspace_id: &str,
    ) -> Result<Self, Error> {
        let workspace_id = Id(parse_uuid(raw_workspace_id, "workspace id")?);
        let user_id = user.id();
        let role = workspace_role(state, workspace_id, user_id).await?;
        Ok(Self { workspace_id, role })
    }

    /// 上游 `roleAllowed(member.Role, "owner", "admin")`。失败 ⇒ 403 `insufficient permissions`。
    fn require_admin(&self) -> Result<(), Error> {
        if self.is_admin() {
            Ok(())
        } else {
            Err(forbidden("insufficient permissions"))
        }
    }

    fn is_admin(&self) -> bool {
        role_is_admin(&self.role)
    }
}

/// 上游 `writeErrorCode` 的等价物（本仓沿用嵌套 envelope，与 `routes::auth` 的
/// `google_error` 同款）：状态码由调用方显式给出，`code` 用上游字面量。
fn error_with_code(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorResponse::new(code, message),
        }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/workspaces/{id}/github/connect
// ---------------------------------------------------------------------------

/// 上游 `GitHubConnect`（`github.go:462`）。
async fn connect(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(raw_workspace_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let scope = GithubScope::resolve(&state, user, &raw_workspace_id).await?;
    scope.require_admin()?;

    // ① 「能连接」判据（slug + webhook secret）不满足 ⇒ 200 + configured:false。
    let keys = &state.github_keys;
    if !keys.is_connectable() {
        return Ok(Json(GithubConnectResponse {
            url: String::new(),
            configured: false,
        })
        .into_response());
    }

    // ② `return_to` 的白名单（上游 `isAllowedGitHubReturnTo`）⇒ 非法是 400。
    let return_to = query
        .get("return_to")
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .unwrap_or(RETURN_TO_GITHUB);
    if !is_allowed_return_to(return_to) {
        return Err(bad_request("invalid return target").into());
    }

    // ③ 签 state（secret 在 ① 已判存在；签名失败是内部异常 ⇒ 500）。
    let secret = keys
        .webhook_secret
        .as_deref()
        .expect("is_connectable() implies webhook secret");
    let slug = keys
        .app_slug
        .as_deref()
        .expect("is_connectable() implies app slug");
    let Ok(state_token) = sign_state_for_return(secret, raw_workspace_id.trim(), return_to) else {
        return Ok(error_with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "failed to sign state",
        ));
    };

    // ④ 安装引导 URL（上游 `fmt.Sprintf` + `url.PathEscape`/`QueryEscape`）。
    let url = format!(
        "https://github.com/apps/{}/installations/new?state={}",
        percent_encode_path_segment(slug),
        percent_encode_query_value(&state_token),
    );
    Ok(Json(GithubConnectResponse {
        url,
        configured: true,
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// GET /api/workspaces/{id}/github/installations
// ---------------------------------------------------------------------------

/// 上游 `ListGitHubInstallations`（`github.go:712`）。
///
/// **member 可见**：每个角色都拿到行，但只有 owner/admin 拿到 `installation_id`
/// （管理手柄）与 `can_manage:true`。
async fn list_installations(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(raw_workspace_id): Path<String>,
) -> ApiResult<Json<GithubInstallationsResponse>> {
    let scope = GithubScope::resolve(&state, user, &raw_workspace_id).await?;
    let can_manage = scope.is_admin();
    let rows = GithubInstallationRepo::new(state.db.clone())
        .list_by_workspace(scope.workspace_id)
        .await
        .map_err(|_| Error::Database("failed to list installations".into()))?;

    let installations = rows
        .iter()
        .map(|row| {
            let response = GithubInstallationResponse::from_row(row);
            if can_manage {
                response
            } else {
                response.without_installation_id()
            }
        })
        .collect();

    Ok(Json(GithubInstallationsResponse {
        installations,
        configured: state.github_keys.is_connectable(),
        repository_browse_configured: state.github_keys.is_app_configured(),
        can_manage,
    }))
}

// ---------------------------------------------------------------------------
// GET /api/workspaces/{id}/github/installations/{installationId}/repositories
// ---------------------------------------------------------------------------

/// 上游 `ListGitHubInstallationRepositories`（`github.go:747`）。
///
/// 顺序逐条对齐上游：workspace id → installation id → 取行（**不**收窄 workspace，取不到
/// 404）→ 跨 workspace 判定 404 → 「能浏览仓库」判据 403 → 分页参数 400 → App JWT →
/// token 交换 → 列表 → **尽力而为**撤销 token。任何出站失败 ⇒ **502**
/// `failed to list github repositories`（上游对这一段只有一个错误文案）。
async fn list_repositories(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((raw_workspace_id, raw_installation_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let scope = GithubScope::resolve(&state, user, &raw_workspace_id).await?;
    scope.require_admin()?;
    let installation_id = Id(parse_uuid(&raw_installation_id, "installation id")?);

    let row = GithubInstallationRepo::new(state.db.clone())
        .find_by_id(installation_id)
        .await
        .map_err(|_| Error::Database("failed to load github installation".into()))?
        .ok_or_else(|| not_found("github installation"))?;
    // 跨 workspace 的行与「不存在」同判（上游 `uuidToString(row.WorkspaceID) != workspaceID` ⇒ 404）。
    if row.workspace_id() != scope.workspace_id {
        return Err(not_found("github installation").into());
    }

    let keys = &state.github_keys;
    if !keys.is_app_configured() {
        return Ok(error_with_code(
            StatusCode::FORBIDDEN,
            CODE_REPOSITORY_BROWSING_NOT_CONFIGURED,
            "github repository browsing is not configured",
        ));
    }

    let page = GithubPageParam::parse(
        query.get("page").map(String::as_str),
        query.get("per_page").map(String::as_str),
    )
    .map_err(|e| bad_request(e.to_string()))?;

    let app_id = keys.app_id.as_deref().unwrap_or_default();
    let pem = keys.private_key_pem.as_deref().unwrap_or_default();
    let app_jwt = match AppJwtSigner::from_pem(app_id, pem)
        .and_then(|signer| signer.sign_app_jwt(now_unix()))
    {
        Ok(token) => token,
        Err(e) => {
            // `AppJwtError` 的 Display 不含密钥材料；上游把「私钥配错」当运维可行动的错误
            // （`signGitHubAppJWT` 返回 err ⇒ 502）。
            tracing::warn!(error = %e, "github: sign App JWT failed");
            return Ok(error_with_code(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "failed to list github repositories",
            ));
        }
    };

    let client = GithubClient::new(github_api_base());
    let exchanged = match client
        .exchange_installation_token(&app_jwt, row.installation_id)
        .await
    {
        Ok(token) => token,
        Err(e) => {
            tracing::warn!(error = %e, "github: installation token exchange failed");
            return Ok(error_with_code(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "failed to list github repositories",
            ));
        }
    };

    let listed = match client
        .list_installation_repositories(exchanged.expose(), page.page, page.per_page)
        .await
    {
        Ok(listed) => listed,
        Err(e) => {
            tracing::warn!(error = %e, "github: list installation repositories failed");
            // 撤销在**返回之前**仍然执行（上游的 `defer` 与 early return 同序）。
            let _ = client.revoke_installation_token(exchanged.expose()).await;
            return Ok(error_with_code(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "failed to list github repositories",
            ));
        }
    };

    // 上游 `defer revokeGitHubInstallationToken(...)`：撤销失败**不影响**已完成的响应
    // （best-effort，`docs/61` 的 M8-1 专属 DoD 点名这一条）。
    let _ = client.revoke_installation_token(exchanged.expose()).await;

    Ok(Json(GithubRepositoriesResponse {
        repositories: listed.repositories,
        total_count: listed.total_count,
        next_page: listed.next_page,
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// DELETE /api/workspaces/{id}/github/installations/{installationId}
// ---------------------------------------------------------------------------

/// 上游 `DeleteGitHubInstallation`（`github.go:938`）。
///
/// **不**调 GitHub（上游只删本仓的行）⇒ 「撤销凭据失败不回滚删除」在本仓是**恒真**：删除
/// 路径上没有任何出站调用。成功回 **204**（删 0 行也回 204，与上游 `:exec` 不看
/// `rows_affected` 同判）。
async fn delete_installation(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((raw_workspace_id, raw_installation_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let scope = GithubScope::resolve(&state, user, &raw_workspace_id).await?;
    scope.require_admin()?;
    let installation_id = Id(parse_uuid(&raw_installation_id, "installation id")?);

    let deleted = GithubInstallationRepo::new(state.db.clone())
        .delete(installation_id, scope.workspace_id)
        .await
        .map_err(|_| Error::Database("failed to remove installation".into()))?;
    if !deleted {
        tracing::debug!(
            installation_id = %raw_installation_id,
            "github: delete matched no row (already gone / other workspace)"
        );
    }

    // 上游无条件发布 `EventGitHubInstallationDeleted`（`payload = {"id": id}`）。
    let envelope = mc_realtime::EventEnvelope::new(
        "github_installation",
        raw_workspace_id.trim(),
        None,
        serde_json::json!({ "id": raw_installation_id.trim() }),
    )
    .with_type("github_installation:deleted");
    state.realtime.publish(envelope);

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// 上游 `github_installation:created`（`pkg/protocol/events.go:170`）的广播载荷。
///
/// 与 list 端点的**最弱角色视图**同形：`installation_id` 不在里面（WS 扇出没有 per-recipient
/// 视角，admin 客户端靠重查列表拿回管理手柄 —— 上游注释逐字）。
pub(crate) fn installation_created_envelope(
    workspace_id: &str,
    installation: &GithubInstallationResponse,
) -> mc_realtime::EventEnvelope {
    mc_realtime::EventEnvelope::new(
        "github_installation",
        workspace_id,
        None,
        serde_json::json!({ "installation": installation }),
    )
    .with_type("github_installation:created")
}

/// 当前 Unix 秒（App JWT 的 `iat`/`exp`；集中一处便于测试观察）。
fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

// ---------------------------------------------------------------------------
// 上游 `githubAPIBase` 的等价物（离线替身的唯一接缝）
// ---------------------------------------------------------------------------

/// 进程级默认 base（上游 `var githubAPIBase = "https://api.github.com"`，可写）。
///
/// ⚠️ 与上游同款：这是**包级可变状态**，生产永不写它；只有测试
/// （`crates/mc-http/tests/github/`）把它指到本地替身。用 `Mutex<Option<String>>` 而不是
/// `OnceLock` —— 一个测试二进制里可能既要跑「默认 base」的用例又要跑「注入 base」的用例，
/// 而 `OnceLock` 只允许写一次。
static GITHUB_API_BASE: Mutex<Option<String>> = Mutex::new(None);

/// 当前生效的 REST base。
pub fn github_api_base() -> String {
    GITHUB_API_BASE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .unwrap_or_else(|| mc_vcs_github::port::GithubAppConfig::DEFAULT_API_BASE.to_string())
}

/// 测试注入 base（生产代码**不得**调用）。
pub fn set_github_api_base(base: impl Into<String>) {
    *GITHUB_API_BASE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(base.into());
}

/// 清掉注入（测试收尾）。
pub fn reset_github_api_base() {
    *GITHUB_API_BASE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

// ---------------------------------------------------------------------------
// 百分号编码（上游 `url.PathEscape` / `url.QueryEscape` 的等价物）
// ---------------------------------------------------------------------------

/// `url.PathEscape` 的等价物：**只**保留 RFC 3986 的 unreserved 集合
/// （`ALPHA / DIGIT / - . _ ~`），其余逐字节 `%XX`。
///
/// 上游 `PathEscape` 额外允许子分隔符（`$&+,;=:@`）不被转义；本仓更保守（多转义不改变语义，
/// 少转义会改变 URL 结构）。登记 `docs/32` §9.12。
pub(crate) fn percent_encode_path_segment(value: &str) -> String {
    percent_encode(value, false)
}

/// `url.QueryEscape` 的等价物：unreserved 之外全部转义，空格按 `+`（Go 的 `QueryEscape`
/// 就是 `+`）。
pub(crate) fn percent_encode_query_value(value: &str) -> String {
    percent_encode(value, true)
}

fn percent_encode(value: &str, space_as_plus: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~') {
            out.push(ch);
        } else if ch == ' ' && space_as_plus {
            out.push('+');
        } else {
            out.push('%');
            let _ = write!(out, "{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_matches_go_escaping_rules() {
        // unreserved 原样。
        assert_eq!(
            percent_encode_path_segment("acme-app_1.2~3"),
            "acme-app_1.2~3"
        );
        // 空格：path 段是 %20，query 值是 `+`（Go 的 `url.QueryEscape`）。
        assert_eq!(percent_encode_path_segment("a b"), "a%20b");
        assert_eq!(percent_encode_query_value("a b"), "a+b");
        // state 的实际形态（hex + `.`）在两种编码下都不变。
        let state = "8f14e45f-ceea-467e-b1d2-5a4b1b1b1b1b.9f2c1d3e4a5b6c7d.00112233";
        assert_eq!(percent_encode_query_value(state), state);
        // `/` 与 `?` 必须转义（否则会切断 URL 结构）。
        assert_eq!(percent_encode_query_value("a/b?c"), "a%2Fb%3Fc");
        assert_eq!(percent_encode_path_segment("a/b"), "a%2Fb");
    }

    #[test]
    fn api_base_defaults_to_github_and_is_overridable() {
        assert_eq!(github_api_base(), "https://api.github.com");
        set_github_api_base("http://127.0.0.1:9");
        assert_eq!(github_api_base(), "http://127.0.0.1:9");
        reset_github_api_base();
        assert_eq!(github_api_base(), "https://api.github.com");
    }
}
