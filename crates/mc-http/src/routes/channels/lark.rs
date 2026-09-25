//! Feishu / Lark 安装与扫码会话面：**5 条**路由（写者 **M7-14**；
//! `docs/60-M7-PLAN.md` §1.1 / §3.3）。
//!
//! | 注册键 | `router.go` | 授权层 | 未配置语义 |
//! | --- | ---: | --- | --- |
//! | `GET /api/workspaces/:id/lark/installations` | 1783 | workspace **member** | **200** + `installations: []` / `configured:false` / `install_supported:false` |
//! | `DELETE /api/workspaces/:id/lark/installations/:installationId` | 1784 | 绑定 agent 的 owner **或** workspace owner/admin | **403** `lark_not_configured` |
//! | `POST /api/workspaces/:id/lark/install/begin` | 1790 | 目标 agent 的 owner **或** workspace owner/admin | **403** `lark_not_configured` |
//! | `GET /api/workspaces/:id/lark/install/:sessionId/status` | 1791 | 会话发起人 **或** workspace owner/admin | **403** `lark_not_configured` |
//! | `POST /api/lark/binding/redeem` | 1841 | 登录用户（**无** workspace 前缀） | **403** `lark_not_configured` |
//!
//! - **业务逻辑在 adapter**：安装 / 设备流 / 绑定令牌的判决在
//!   `mc_channel::lark::{installation,registration,binding}`，本文件只做四件事 ——
//!   **鉴权层**、**wire 形状**、**未配置语义**、把「channel 层不碰 SQL」这条边界铁律落成端口
//!   实现（[`store`]）。
//! - **形态纪律**：只按上游字面量注册**那一形态**（M7 的 `dual-form required: 0` ⇒ 补尾斜杠是
//!   `EXTRA_ALIAS`、漏字面量是 `MISSING_EXACT`，两类都是硬失败）。路径参数必须写 `:name`
//!   （matchit 0.7 把 `{name}` 当字面量 ⇒ 编译通过且恒 404）。第 5 条**没有** workspace 前缀
//!   （上游三簇之一）：兑换者在拥有 workspace 上下文**之前**就打它，身份来自会话。
//! - **授权按上游逐条复刻**（`MUL-4213`）：list 只要成员；revoke 与 begin 认「绑定 agent 的
//!   owner **或** workspace owner/admin」；status 认「会话发起人 **或** workspace
//!   owner/admin」，其余成员回 **404**（不泄露会话存在性）；孤儿安装（agent 已被硬删）的
//!   revoke 回落到 workspace owner/admin（清理入口不能因为 agent 没了就失效）。
//!
//! # 未配置语义（`docs/60` §2.4 / R-M7-3，不许"统一 503"）
//!
//! 上游的装配判据是 `MULTICA_LARK_SECRET_KEY` 能解出一把 32 字节的密钥
//! （`router.go` 的 `secretbox.LoadKey(...)` 成功才进整块）⇒ 本仓的对应物是
//! [`AppState::channel_keys`] 里 `Lark` 那一格。五条路由的未配置行为**逐条不同**（这张表
//! 就是 ⑨ 的 7 条 lark fixture 钉住的东西）：
//!
//! 1. **列表**回 200 + 空 + 两个 `false`（**不**查库、**不**读身份 —— 上游那个 handler 的
//!    第一句就是 `if h.LarkInstallations == nil`）：正是
//!    `TestListLarkInstallations_NotConfiguredReturnsEmpty` /
//!    `…_HardCodedInstallSupportedFalse` / `…_StubClientReportsInstallNotSupported` 三条；
//! 2. **其余四条**回 **403 `lark_not_configured`**（上游 `writeFeatureDisabled` ⇒
//!    `http.StatusForbidden`）：`TestRevokeLarkInstallation_NotConfigured` /
//!    `TestBeginLarkInstall_NotConfigured` / `TestGetLarkInstallStatus_NotConfigured` /
//!    `TestRedeemLarkBindingToken_NotConfigured` 四条。
//!
//! ⚠️ 上游注释里 `begin` / `status` 写着"Returns 503 when the integration is not wired" ——
//! **那句注释与代码不符**：两个 handler 的第一句都是 `writeFeatureDisabled`（403）。
//! 本仓按**代码**落（`docs/32` §30 的 **D11**），7 条 fixture 的 `status_expected` 也是 403。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! - 明文 `app_secret` / `client_secret` 只经 `mc_channel` 的 [`InstallationService`] 与
//!   [`RegistrationService::boxed`] 封好；本文件**不解密**、不回显；
//! - 响应 DTO 不含 `app_secret_encrypted` / `ws_lease_*`（`dto.rs` 的模块文档）；
//! - 本文件**没有**任何 `tracing::*` 插值凭据；唯一的两条 `tracing::*` 只带错误码与
//!   `installation_id`；
//! - 「错误路径不回显凭据」有专门用例（`lark/tests.rs`）。

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use mc_channel::lark::binding::{BindingError, BindingTokenService};
use mc_channel::lark::client::{ApiClient, TokenCacheInvalidator};
use mc_channel::lark::http_client::{HttpApiClient, HttpClientConfig};
use mc_channel::lark::installation::{InstallError, InstallationService};
use mc_channel::lark::registration::{
    HttpFormPoster, InstallSessionStore, MemoryInstallSessionStore, RegistrationClient,
    RegistrationConfig, RegistrationService, RegistrationServiceConfig, SessionNotFound,
};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_errors::Error;
use mc_repos::agent::{role_is_admin, AgentRepo};

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, forbidden, not_found, parse_uuid};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;
use dto::{
    configured, error_with_code, feature_disabled, unauthorized, BeginInstallQuery,
    BeginLarkInstallResponse, LarkInstallStatusResponse, LarkInstallationResponse,
    LarkInstallationsResponse, RedeemLarkBindingTokenRequest, RedeemLarkBindingTokenResponse,
    BODY_LIMIT,
};
use store::{PgBindingStore, PgLarkInstallStore, PgRegistrationStore};

pub mod dto;
pub mod store;

#[cfg(test)]
mod tests;

/// 本平台的路由切片（**逐字**上游路径；见模块头的形态纪律）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/workspaces/:id/lark/installations",
            get(list_installations),
        )
        .route(
            "/api/workspaces/:id/lark/installations/:installationId",
            delete(revoke_installation),
        )
        .route(
            "/api/workspaces/:id/lark/install/begin",
            post(begin_install).layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .route(
            "/api/workspaces/:id/lark/install/:sessionId/status",
            get(install_status),
        )
        .route(
            "/api/lark/binding/redeem",
            post(redeem_binding).layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
}

// =====================================================================
// 鉴权与装配
// =====================================================================

/// 解析"调用者是这个 workspace 的成员"（上游 `RequireWorkspaceMemberFromURL`）。
///
/// 非成员 ⇒ **404**（不是 403：上游那条中间件用 `requireWorkspaceRole` 的"workspace not
/// found"文案 —— 一个外人**不该**从错误码里学到这个 workspace 存在）。返回
/// `(workspace_id, user_id, role)`。
async fn resolve_member(
    state: &AppState,
    raw_workspace_id: &str,
    user: AuthUser,
    resource: &'static str,
) -> Result<(Id, Id, String), Error> {
    let workspace_id = Id(parse_uuid(raw_workspace_id, "workspace id")?);
    let user_id = user.id();
    let role: Option<(String,)> =
        sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.0)
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|error| Error::Database(error.to_string()))?;
    let role = role
        .map(|(role,)| role)
        .ok_or_else(|| not_found(resource))?;
    Ok((workspace_id, user_id, role))
}

/// workspace owner/admin（上游 `RequireWorkspaceRoleFromURL(…, "owner", "admin")`）。
fn require_admin(role: &str) -> Result<(), Error> {
    if role_is_admin(role) {
        Ok(())
    } else {
        Err(forbidden("insufficient permissions"))
    }
}

/// 「能管这个 agent 吗」—— workspace owner/admin **或** 该 agent 的 owner（上游
/// `canManageAgent`，`MUL-4213` 把 begin/revoke 的授权从"管理员"放宽到"agent 的 owner"）。
///
/// `agent_missing` 是 revoke 的**孤儿**分支：agent 被硬删时退化成"只要 workspace
/// owner/admin"，于是断连入口仍然可用（上游逐字：*"the cleanup entry point keeps working
/// without handing a plain member orphan-row rights"*）。
async fn can_manage_agent(
    state: &Arc<AppState>,
    workspace_id: Id,
    agent_id: Id,
    user_id: Id,
    role: &str,
    agent_missing: bool,
) -> Result<(), Error> {
    // workspace owner/admin 一路放行（上游 `canManageAgent` 的第一支）。
    require_admin(role)?;
    if agent_missing {
        return Err(forbidden("insufficient permissions"));
    }
    let owner: Option<(Option<uuid::Uuid>,)> =
        sqlx::query_as("SELECT owner_id FROM agent WHERE id = $1 AND workspace_id = $2")
            .bind(agent_id.0)
            .bind(workspace_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|error| Error::Database(error.to_string()))?;
    match owner {
        // `owner_id` 可空（`NULL` = 无主）⇒ 无主 agent 只有 workspace 管理员能管。
        Some((Some(owner_id),)) if owner_id == user_id.0 => Ok(()),
        _ => Err(forbidden("insufficient permissions")),
    }
}

/// 本部署的 lark 出站客户端。
///
/// 生产装配造的是真 [`HttpApiClient`]（`is_configured() == true`）；造不出来时回
/// [`StubApiClient`]（每个传输调用都回 `NotConfigured`，于是"没接线"是**响亮**的，
/// 而不是悄悄丢卡片）。上游把同一件事表达成 `h.LarkAPIClient.IsConfigured()` 那个布尔位。
fn api_client() -> Arc<HttpApiClient> {
    Arc::new(HttpApiClient::new(HttpClientConfig::default()))
}

/// lark 的落库盒（部署密钥的唯一读口是 [`AppState::channel_keys`]）。
///
/// `None` = 该平台的部署密钥没配（该渠道**整体不装配**，`docs/60` §2.6 第 3 条）。
fn install_service(state: &Arc<AppState>) -> Option<InstallationService> {
    let boxed = state.channel_keys.get(ChannelKind::Lark)?.clone();
    Some(InstallationService::new(
        Arc::new(PgLarkInstallStore::new(Arc::clone(state))),
        boxed,
    ))
}

/// 设备流注册服务（上游 `h.LarkRegistration`；`None` = 未装配）。
fn registration_service(state: &Arc<AppState>) -> Option<Arc<RegistrationService>> {
    let installs = Arc::new(install_service(state)?);
    let poster = HttpFormPoster::new().ok()?;
    let client = RegistrationClient::new(RegistrationConfig::default(), Arc::new(poster));
    // 同一个真客户端同时充当两条角色：`ApiClient`（取 Bot 信息）与
    // `TokenCacheInvalidator`（重装换密钥后令牌缓存必须作废）。类型擦除之后拿不回后者，
    // 所以先留一份具体类型的 `Arc`。
    let concrete = api_client();
    let invalidator: Arc<dyn TokenCacheInvalidator + Send + Sync> = concrete.clone();
    let api: Arc<dyn ApiClient> = concrete;
    let sessions: Arc<dyn InstallSessionStore> = Arc::new(MemoryInstallSessionStore::new());
    Some(Arc::new(RegistrationService::new(
        RegistrationServiceConfig::default(),
        client,
        api,
        Some(invalidator),
        Arc::new(PgRegistrationStore::new(Arc::clone(state))),
        installs,
        sessions,
    )))
}

/// 依赖是否齐（判据与 [`registration_service`] 同一条，只是不建实例）。
fn registration_available(state: &AppState) -> bool {
    configured(state) && HttpFormPoster::new().is_ok()
}

/// 把 adapter 的安装错误映射到 HTTP（上游 `writeError` / `writeFeatureDisabled` 的文案族）。
fn install_error(error: &InstallError) -> Response {
    match error {
        InstallError::NotFound => {
            crate::error::ApiError::from(not_found("lark installation")).into_response()
        }
        InstallError::InvalidParams { .. } => error_with_code(
            StatusCode::BAD_REQUEST,
            error.code(),
            "lark install rejected: a required installation field is missing",
        ),
        InstallError::OwnedByAnotherWorkspace => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "This Feishu app is already connected to a different Multica workspace. Disconnect it \
             there before connecting it here.",
        ),
        InstallError::OwnedByArchivedAgent => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "This Feishu app is connected to an archived agent in this workspace. Restore that \
             agent, or disconnect its bot, before connecting it here.",
        ),
        InstallError::OwnedBySameWorkspace => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "This Feishu app is already connected to another agent in this workspace. Disconnect \
             it there first, then connect it here.",
        ),
        InstallError::ConflictUnclassified => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "This Feishu app is already connected elsewhere. Disconnect it there first, then \
             connect it here.",
        ),
        InstallError::AlreadyAssigned => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "this Lark account is already bound to a different Multica user",
        ),
        InstallError::NotWorkspaceMember => error_with_code(
            StatusCode::FORBIDDEN,
            error.code(),
            "binding refused (are you a workspace member?)",
        ),
        // 兜底 500：上游逐字 —— 加密 / 落库 / 意外都是**服务端**问题，不该被说成"你的凭据不对"。
        other => {
            tracing::error!(code = other.code(), "lark install failed");
            error_with_code(
                StatusCode::INTERNAL_SERVER_ERROR,
                other.code(),
                "could not save this lark installation — something went wrong on our side",
            )
        }
    }
}

/// 绑定失败的响应矩阵（逐条对齐上游 `RedeemLarkBindingToken` 的 switch）。
///
/// 状态码与错误码都取自 adapter（[`BindingError::http_status`] / [`BindingError::code`]），
/// 这里只补上游那句人话文案 —— 于是两条来源不会各自漂。
fn binding_error(error: &BindingError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let message = match error {
        BindingError::TokenInvalid => "binding token invalid or expired",
        BindingError::AlreadyAssigned => {
            "this Lark account is already bound to a different Multica user"
        }
        BindingError::NotWorkspaceMember => "binding refused (are you a workspace member?)",
        BindingError::Store { .. } => "failed to redeem token",
    };
    error_with_code(status, error.code(), message)
}

/// 广播一条安装事件（上游 `EventLarkInstallation{Revoked}` / `…{Created}`，
/// 两个 `type` **逐字**：`lark_installation:{created,revoked}`）。
fn publish_event(state: &AppState, workspace_id: Id, action: &str, id: Id) {
    let kind = "lark_installation";
    let envelope = mc_realtime::EventEnvelope::new(
        kind,
        workspace_id.to_string(),
        None,
        serde_json::json!({ "id": id.to_string() }),
    )
    .with_type(format!("{kind}:{action}"));
    state.realtime.publish(envelope);
}

// =====================================================================
// GET /api/workspaces/:id/lark/installations
// =====================================================================

/// 上游 `ListLarkInstallations`：**member 可见**（非管理员也要能渲染 Integrations 页）。
///
/// 未配置 ⇒ **不**查库、**不**读身份，回 200 空 + 两个 `false`。
async fn list_installations(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path(raw_workspace_id): Path<String>,
) -> ApiResult<Json<LarkInstallationsResponse>> {
    if !configured(&state) {
        return Ok(Json(LarkInstallationsResponse::not_configured()));
    }
    let (workspace_id, _user_id, _role) = resolve_member(
        &state,
        &raw_workspace_id,
        user.ok_or_else(unauthorized)?,
        "workspace",
    )
    .await?;
    let Some(service) = install_service(&state) else {
        return Ok(Json(LarkInstallationsResponse::not_configured()));
    };
    let installations = service
        .list_by_workspace(workspace_id)
        .await
        .map_err(|error| Error::Database(error.to_string()))?;
    // `install_supported` = 设备流**端到端**接好了：注册服务在 且 出站客户端真够得着 Lark
    // （替身完不成 poll 之后的 `GetBotInfo`，所以它必须把这一位压回 `false` ——
    // 上游 `TestListLarkInstallations_StubClientReportsInstallNotSupported`）。
    let install_supported =
        registration_available(&state) && ApiClient::is_configured(&*api_client());
    Ok(Json(LarkInstallationsResponse::configured_with(
        installations
            .iter()
            .map(LarkInstallationResponse::from_installation)
            .collect(),
        install_supported,
    )))
}

// =====================================================================
// DELETE /api/workspaces/:id/lark/installations/:installationId
// =====================================================================

/// 上游 `RevokeLarkInstallation`：状态翻成 `revoked`，**行保留**供审计；重装把状态翻回
/// `active`。授权 = 绑定 agent 的 owner **或** workspace owner/admin；agent 被硬删的孤儿
/// 回落到 workspace owner/admin。
async fn revoke_installation(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path((raw_workspace_id, raw_installation_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    if !configured(&state) {
        return Ok(feature_disabled());
    }
    let (workspace_id, user_id, role) = resolve_member(
        &state,
        &raw_workspace_id,
        user.ok_or_else(unauthorized)?,
        "workspace",
    )
    .await?;
    let installation_id = Id(parse_uuid(&raw_installation_id, "installation id")?);
    let Some(service) = install_service(&state) else {
        return Ok(feature_disabled());
    };
    // workspace 收窄的读：另一个 workspace 猜 id 也读不到 ⇒ 与不存在**同**结果。
    let installation = match service
        .get_in_workspace(installation_id, workspace_id)
        .await
    {
        Ok(installation) => installation,
        Err(InstallError::NotFound) => return Err(not_found("lark installation").into()),
        Err(error) => return Ok(install_error(&error)),
    };
    // agent 还在吗？在 ⇒ 按它的 owner 判权；不在（硬删的孤儿）⇒ 退化成 workspace admin。
    let agent_missing = AgentRepo::new(state.db.clone())
        .get_in_workspace(workspace_id, installation.agent_id)
        .await
        .is_err();
    can_manage_agent(
        &state,
        workspace_id,
        installation.agent_id,
        user_id,
        &role,
        agent_missing,
    )
    .await?;

    match service.revoke(workspace_id, installation_id).await {
        Ok(true) => {
            publish_event(&state, workspace_id, "revoked", installation_id);
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        Ok(false) => Err(not_found("lark installation").into()),
        Err(error) => Ok(install_error(&error)),
    }
}

// =====================================================================
// POST /api/workspaces/:id/lark/install/begin
// =====================================================================

/// 上游 `BeginLarkInstall`：开一个设备流扫码会话。
///
/// 授权 = 目标 agent 的 owner **或** workspace owner/admin（`?agent_id=` 选哪个 agent 被绑）；
/// `?region=` 只认 `feishu` / `lark` / 空，其余 **400**（静默归一化会掩盖前端回归）。
async fn begin_install(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path(raw_workspace_id): Path<String>,
    Query(query): Query<BeginInstallQuery>,
) -> ApiResult<Response> {
    if !configured(&state) {
        return Ok(feature_disabled());
    }
    let (workspace_id, user_id, role) = resolve_member(
        &state,
        &raw_workspace_id,
        user.ok_or_else(unauthorized)?,
        "workspace",
    )
    .await?;

    let raw_agent_id = query.agent_id.trim();
    if raw_agent_id.is_empty() {
        return Err(bad_request("agent_id is required").into());
    }
    let agent_id = Id(parse_uuid(raw_agent_id, "agent_id")?);
    if !query.region_is_acceptable() {
        return Err(bad_request("region must be 'feishu' or 'lark'").into());
    }

    // agent↔workspace 的**边界**预检：agent 不属于这个 workspace ⇒ 404 而不是落库时才炸。
    match AgentRepo::new(state.db.clone())
        .get_in_workspace(workspace_id, agent_id)
        .await
    {
        Ok(_) => {}
        Err(mc_repos::RepoError::NotFound) => {
            return Err(not_found("agent not found in this workspace").into())
        }
        Err(error) => return Err(Error::Database(error.to_string()).into()),
    }
    can_manage_agent(&state, workspace_id, agent_id, user_id, &role, false).await?;

    let Some(service) = registration_service(&state) else {
        return Ok(feature_disabled());
    };
    let region =
        mc_channel::lark::types::Region::or_default(&query.region.trim().to_ascii_lowercase());
    match service
        .begin_install(workspace_id, agent_id, user_id, region)
        .await
    {
        Ok(result) => Ok(Json(BeginLarkInstallResponse {
            session_id: result.session_id,
            qr_code_url: result.qr_code_url,
            expires_in_seconds: result.expires_in_seconds,
            poll_interval_seconds: result.poll_interval_seconds,
        })
        .into_response()),
        // 上游在这里回 502（"failed to start install: …"）：够不着 Lark 是**上游**的问题，
        // 不是调用方的输入问题。
        Err(error) => {
            tracing::warn!(code = %error.code, "lark begin install failed");
            Ok(error_with_code(
                StatusCode::BAD_GATEWAY,
                "lark_install_failed",
                "failed to start install",
            ))
        }
    }
}

// =====================================================================
// GET /api/workspaces/:id/lark/install/:sessionId/status
// =====================================================================

/// 上游 `GetLarkInstallStatus`：轮询一个在飞会话。
///
/// 读权限 = 会话发起人 **或** workspace owner/admin；其余成员（以及未知 / 跨 workspace /
/// 已被 GC 的会话）一律 **404** —— 前端把它当"会话丢了，请重开"。
/// 成功时**不**清理会话（前端可能再轮一次确认；读是幂等的）。
async fn install_status(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path((raw_workspace_id, raw_session_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    if !configured(&state) {
        return Ok(feature_disabled());
    }
    let (workspace_id, user_id, role) = resolve_member(
        &state,
        &raw_workspace_id,
        user.ok_or_else(unauthorized)?,
        "workspace",
    )
    .await?;
    let session_id = raw_session_id.trim();
    if session_id.is_empty() {
        return Err(bad_request("session id is required").into());
    }
    let Some(service) = registration_service(&state) else {
        return Ok(feature_disabled());
    };
    let session = match service.get_session(workspace_id, session_id).await {
        Ok(session) => session,
        Err(SessionNotFound) => return Err(not_found("install session").into()),
    };
    // 只有发起人或 workspace owner/admin 能读；会话 id 只交给过发起人，所以把别人当成
    // "会话丢了"（404，不泄露存在性）与上面的跨 workspace 情形一致。
    if session.initiator_id != user_id && !role_is_admin(&role) {
        return Err(not_found("install session").into());
    }
    Ok(Json(LarkInstallStatusResponse::from_session(&session)).into_response())
}

// =====================================================================
// POST /api/lark/binding/redeem
// =====================================================================

/// 上游 `RedeemLarkBindingToken`：**无** workspace 前缀，身份来自**会话**而不是令牌 ——
/// 偷来的令牌绑不到攻击者的账号上。
///
/// 三种失败各有自己的状态码（`binding.rs` 的 `http_status`）：**410 Gone**（令牌未知 /
/// 已消费 / 已过期）、**409 Conflict**（这个 `open_id` 已属于另一个用户）、
/// **403 Forbidden**（兑换者不是该 workspace 的成员）。
async fn redeem_binding(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Json(body): Json<RedeemLarkBindingTokenRequest>,
) -> ApiResult<Response> {
    if !configured(&state) {
        return Ok(feature_disabled());
    }
    let user_id = user.ok_or_else(unauthorized)?.id();
    let token = body.token.trim();
    if token.is_empty() {
        return Err(bad_request("token is required").into());
    }
    let service = BindingTokenService::new(Arc::new(PgBindingStore::new(Arc::clone(&state))));
    match service.redeem_and_bind(token, user_id).await {
        Ok(bound) => Ok(Json(RedeemLarkBindingTokenResponse {
            workspace_id: bound.workspace_id.to_string(),
            installation_id: bound.installation_id.to_string(),
            lark_open_id: bound.lark_open_id,
        })
        .into_response()),
        Err(error) => {
            if matches!(error, BindingError::Store { .. }) {
                tracing::warn!(code = error.code(), "lark binding redeem failed");
            }
            Ok(binding_error(&error))
        }
    }
}
