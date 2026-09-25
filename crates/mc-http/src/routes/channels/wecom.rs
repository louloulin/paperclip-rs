//! `WeCom` 契约 / 凭据 / 安装与绑定面：**4 条**路由（写者 **M7-15**；
//! `docs/60-M7-PLAN.md` §1.1 / §3.3）。
//!
//! | 注册键 | `router.go` | 授权层 | 未配置语义 |
//! | --- | ---: | --- | --- |
//! | `GET /api/workspaces/:id/wecom/installations` | 1802 | workspace **member** | **200** + `installations: []` / `configured:false` / `install_supported:false` |
//! | `DELETE /api/workspaces/:id/wecom/installations/:installationId` | 1808 | workspace **owner/admin** | **403** `wecom_not_configured` |
//! | `POST /api/workspaces/:id/wecom/install/byo` | 1809 | workspace **owner/admin** | **403** `wecom_not_configured` |
//! | `POST /api/wecom/binding/redeem` | 1854 | 登录用户（**无** workspace 前缀） | **403** `wecom_not_configured` |
//!
//! - **业务逻辑在 adapter**：安装 / 绑定 / 凭据的判决在
//!   `mc_channel::wecom::{installation,binding,credentials,types}`，本文件只做四件事 ——
//!   **鉴权层**、**wire 形状**、**未配置语义**、把「channel 层不碰 SQL」这条边界铁律落成端口
//!   实现（[`store`]）。
//! - **形态纪律**：只按上游字面量注册**那一形态**（M7 的 `dual-form required: 0` ⇒ 补尾斜杠是
//!   `EXTRA_ALIAS`、漏字面量是 `MISSING_EXACT`，两类都是硬失败）。路径参数必须写 `:name`
//!   （matchit 0.7 把 `{name}` 当字面量 ⇒ 编译通过且恒 404）。第 4 条**没有** workspace 前缀
//!   （上游三簇之一）：兑换者在拥有 workspace 上下文**之前**就打它，身份来自会话。
//! - **授权**：上游把 list 注册在 `RequireWorkspaceMemberFromURL` 里，把 revoke / BYO 注册在
//!   `RequireWorkspaceRoleFromURL(…, "owner", "admin")` 里，redeem 只要登录 ⇒ 本文件用
//!   [`resolve_member`] + [`require_admin`] 逐条复刻（非成员 404、成员但非管理员 403）。
//!
//! # 未配置语义（`docs/60` §2.4 / R-M7-3，不许"统一 503"）
//!
//! 上游的装配判据是 `MULTICA_WECOM_SECRET_KEY` 能解出一把 32 字节的密钥
//! （`router.go:920` 的 `secretbox.LoadKey(...)` 成功才进整块）⇒ 本仓的对应物是
//! [`AppState::channel_keys`] 里 `WeCom` 那一格。四条路由的未配置行为**逐条不同**：
//!
//! 1. **列表**回 200 + 空 + 两个 `false`（**不**查库、**不**读身份 —— 上游那个 handler 的
//!    第一句就是 `wecomIntegrationConfigured()`），这条形状是 ⑨ 的
//!    `TestListLarkInstallations_NotConfiguredReturnsEmpty` 那一族判据；
//! 2. **其余三条**回 **403 `wecom_not_configured`**（上游 `writeFeatureDisabled`，
//!    `handler.go:578` ⇒ `http.StatusForbidden`）。
//!
//! ⚠️ **口径更正**（登记 `docs/32` §31 的 **D4**）：`docs/60` §8 的 R-M7-3 写「wecom 端点
//! 503」，`router.go:917` 的注释也写「return 503」——**两处都不是代码**：那条注释描述的是
//! `writeWecomInstallError` 里**够不着 `WeCom`** 的那一格（凭据没能验证 ⇒ 503），与"没配部署
//! 密钥"是两件事。未配置分支的逐字实现是 403，本文件的用例钉住它。
//!
//! # 凭据纪律（`docs/60` §2.3 / §6.5 的 M7-15 行）
//!
//! - BYO 贴进来的密钥**只经 `secretbox` 密文入库**（`mc_channel::wecom::installation` 的职责）；
//!   本文件的 SQL 只搬运**已经封好的** `config`；
//! - 响应 DTO 不含 `config`（`dto.rs` 的模块文档）；错误文案只带**静态句子**与
//!   `WeCom` 自己的 errcode（`install_error` 那张矩阵）；
//! - 本文件**没有**任何 `tracing::*` 插值密钥 / 密文；唯一的 `tracing` 在绑定失败那条
//!   （只带错误码）。
//!
//! # 与上游的落点差异（逐条登记 `docs/32` §31，**不是**静默略过）
//!
//! 1. **六条上游语句没有泛化仓储**（见 `store.rs` 的模块文档）⇒ 以**端口实现**的形态落在
//!    本目录的 [`store`]；2. **`mod.rs` 补 7 行 `pub mod`**（D1）；3. **探针的 wire 一半**
//!    归 M7-16（D3 / D9）；4. **本目录的 `{dto,store,tests}.rs` 是描述写集之外的新文件**（D8，
//!    门 ⑩ 的 800 行硬上限 + 上游同款三分法）。

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use mc_channel::wecom::binding::{BindingError, BindingTokenService, RedeemOutcome};
use mc_channel::wecom::credentials::{
    CredentialProbe, HandshakeProbe, PlaintextSecret, ProbeTransport, TransportError,
};
use mc_channel::wecom::installation::{InstallError, InstallationParams, InstallationService};
use mc_channel::wecom::store::InstallationStore;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_errors::Error;
use mc_repos::agent::{role_is_admin, AgentRepo};

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, forbidden, not_found, parse_uuid};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;
use dto::{
    configured, error_with_code, feature_disabled, unauthorized, ByoQuery,
    RedeemWecomBindingTokenRequest, RedeemWecomBindingTokenResponse, RegisterWecomByoRequest,
    WecomInstallationResponse, WecomInstallationsResponse, BODY_LIMIT,
};
use store::{PgBindingStore, PgInstallStore};

pub mod dto;
pub mod store;

#[cfg(test)]
mod tests;

/// 本平台的路由切片（**逐字**上游路径；见模块头的形态纪律）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/workspaces/:id/wecom/installations",
            get(list_installations),
        )
        .route(
            "/api/workspaces/:id/wecom/installations/:installationId",
            delete(revoke_installation),
        )
        .route(
            "/api/workspaces/:id/wecom/install/byo",
            post(register_byo).layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .route(
            "/api/wecom/binding/redeem",
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

/// 装好的安装服务（落库密钥 + PG 端口 + 探针）。
///
/// 返回 `None` = 该平台的部署密钥没配（该渠道**整体不装配**，`docs/60` §2.6 第 3 条）。
fn install_service(state: &AppState) -> Option<InstallationService> {
    let boxed = state.channel_keys.get(ChannelKind::WeCom)?.clone();
    Some(InstallationService::new(
        Arc::new(PgInstallStore::new(state.db.clone())) as Arc<dyn InstallationStore>,
        Arc::new(HandshakeProbe::new(Arc::new(PendingWsTransport))) as Arc<dyn CredentialProbe>,
        boxed,
    ))
}

/// 探针的 **wire 一半**在 M7-16（`crates/mc-channel/src/wecom/{ws_frame.rs,ws_sender.rs,
/// stream_store.rs}`）—— 本片写集不含那三个文件。
///
/// 在那之前，生产接线拿到的是这一个**fail-closed** 的传输：它永远回
/// [`TransportError::Failed`]，于是 BYO 安装得到 **503 `wecom_credentials_unverifiable`**
/// （"凭据没被改动，稍后再试"）。这**不是**"先放行、以后再验"：证明控制权是安全不变式
/// （`credential_probe.go` 的整段注释就是讲为什么），没有探针就**不能**落一行凭据。
/// 登记为 `docs/32` §31 的 **D9**（M7-16 落传输、M7-21 复核该端点转绿）。
#[derive(Debug, Clone, Copy)]
struct PendingWsTransport;

#[async_trait::async_trait]
impl ProbeTransport for PendingWsTransport {
    async fn subscribe_ack(
        &self,
        _bot_id: &str,
        _secret: &PlaintextSecret,
    ) -> Result<i32, TransportError> {
        Err(TransportError::Failed {
            stage: "ws-transport-not-wired",
        })
    }
}

/// 把 adapter 的安装错误映射到 HTTP（逐条对齐上游 `writeWecomInstallError`）。
fn install_error(error: &InstallError) -> Response {
    match error {
        InstallError::NotFound => {
            crate::error::ApiError::from(not_found("wecom installation")).into_response()
        }
        // 调用方补一个字段就能成 ⇒ 400。上游把它与"凭据被拒"分开（两码两文案），
        // 因为二者对管理员是**两个不同的下一步**。
        InstallError::InvalidParams { .. } => error_with_code(
            StatusCode::BAD_REQUEST,
            error.code(),
            "could not connect the WeCom bot — check the Bot ID and secret from the WeCom admin \
             console, and that the bot is a smart bot with the long connection enabled",
        ),
        InstallError::CredentialsRejected { .. } => error_with_code(
            StatusCode::BAD_REQUEST,
            error.code(),
            "WeCom rejected this Bot ID and secret — check both on the WeCom admin console, and \
             that the bot is a smart bot with the long connection enabled",
        ),
        InstallError::CredentialsUnverifiable { .. } => {
            tracing::warn!(code = error.code(), "wecom install could not verify the bot");
            error_with_code(
                StatusCode::SERVICE_UNAVAILABLE,
                error.code(),
                "could not reach WeCom to verify this bot — the credentials were not changed; try \
                 again in a moment",
            )
        }
        InstallError::OwnedBySameWorkspace => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "this bot is already connected to another agent in this workspace — disconnect it \
             there first, then connect it here",
        ),
        InstallError::OwnedByArchivedAgent => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "this bot is connected to an archived agent in this workspace — restore that agent, or \
             disconnect its bot, before connecting it here",
        ),
        InstallError::OwnedByAnotherWorkspace => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "this bot is already connected to a different Multica workspace — disconnect it there \
             before connecting it here",
        ),
        // 兜底 500：上游逐字 —— 数据库 / 加密 / 意外都是**服务端**问题，不该被说成"你的凭据不对"。
        other => {
            tracing::error!(code = other.code(), "wecom install failed");
            error_with_code(
                StatusCode::INTERNAL_SERVER_ERROR,
                other.code(),
                "could not save this bot — something went wrong on our side. Your credentials \
                 were not changed; please try again, and contact support if it keeps failing",
            )
        }
    }
}

// =====================================================================
// GET /api/workspaces/:id/wecom/installations
// =====================================================================

/// 上游 `ListWecomInstallations`：**member 可见**（非管理员也要能渲染 Integrations 页）。
///
/// 未配置 ⇒ **不**查库、**不**读身份，回 200 空 + 两个 `false`。
async fn list_installations(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path(raw_workspace_id): Path<String>,
) -> ApiResult<Json<WecomInstallationsResponse>> {
    if !configured(&state) {
        return Ok(Json(WecomInstallationsResponse::not_configured()));
    }
    let (workspace_id, _user_id, _role) = resolve_member(
        &state,
        &raw_workspace_id,
        user.ok_or_else(unauthorized)?,
        "workspace",
    )
    .await?;
    let Some(service) = install_service(&state) else {
        return Ok(Json(WecomInstallationsResponse::not_configured()));
    };
    let installations = service
        .list_by_workspace(workspace_id)
        .await
        .map_err(|error| Error::Database(error.to_string()))?;
    Ok(Json(WecomInstallationsResponse::configured_with(
        installations
            .iter()
            .map(WecomInstallationResponse::from_installation)
            .collect(),
    )))
}

// =====================================================================
// DELETE /api/workspaces/:id/wecom/installations/:installationId
// =====================================================================

/// 上游 `RevokeWecomInstallation`：状态翻成 `revoked`，**行保留**供审计；重装把状态翻回
/// `active`。**owner/admin only**（上游在 router 的中间件里）。
async fn revoke_installation(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path((raw_workspace_id, raw_installation_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    if !configured(&state) {
        return Ok(feature_disabled());
    }
    let (workspace_id, _user_id, role) = resolve_member(
        &state,
        &raw_workspace_id,
        user.ok_or_else(unauthorized)?,
        "workspace",
    )
    .await?;
    require_admin(&role)?;
    let installation_id = Id(parse_uuid(&raw_installation_id, "installation id")?);
    let Some(service) = install_service(&state) else {
        return Ok(feature_disabled());
    };
    // workspace 收窄的读：另一个 workspace 猜 id 也读不到 ⇒ 与不存在**同**结果。
    match service
        .get_in_workspace(installation_id, workspace_id)
        .await
    {
        Ok(_) => {}
        Err(InstallError::NotFound) => return Err(not_found("wecom installation").into()),
        Err(error) => return Ok(install_error(&error)),
    }
    if let Err(error) = service.revoke(workspace_id, installation_id).await {
        return Ok(install_error(&error));
    }
    publish_event(&state, workspace_id, "revoked", installation_id);
    Ok(StatusCode::NO_CONTENT.into_response())
}

// =====================================================================
// POST /api/workspaces/:id/wecom/install/byo
// =====================================================================

/// 上游 `RegisterWecomBYO`（`?agent_id=…`）：**owner/admin**；`agent_id` 在**查询串**里，
/// 两个凭据在体里。边界上先把"这个 agent 在不在这个 workspace"问清楚（404 而不是落库时才发现）。
async fn register_byo(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path(raw_workspace_id): Path<String>,
    Query(query): Query<ByoQuery>,
    Json(body): Json<RegisterWecomByoRequest>,
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
    require_admin(&role)?;

    let raw_agent_id = query.agent_id.trim();
    if raw_agent_id.is_empty() {
        return Err(bad_request("agent_id is required").into());
    }
    let agent_id = Id(parse_uuid(raw_agent_id, "agent_id")?);
    let agents = AgentRepo::new(state.db.clone());
    match agents.get_in_workspace(workspace_id, agent_id).await {
        Ok(_) => {}
        Err(mc_repos::RepoError::NotFound) => {
            return Err(not_found("agent not found in this workspace").into())
        }
        Err(error) => return Err(Error::Database(error.to_string()).into()),
    }

    let Some(service) = install_service(&state) else {
        return Ok(feature_disabled());
    };
    let params = InstallationParams::new(
        workspace_id,
        agent_id,
        user_id,
        body.bot_id,
        body.secret,
        body.bot_name,
    );
    match service.upsert(params).await {
        Ok(installation) => {
            publish_event(&state, workspace_id, "created", installation.id);
            Ok(Json(WecomInstallationResponse::from_installation(&installation)).into_response())
        }
        Err(error) => Ok(install_error(&error)),
    }
}

// =====================================================================
// POST /api/wecom/binding/redeem
// =====================================================================

/// 上游 `RedeemWecomBindingToken`：**无** workspace 前缀，身份来自**会话**而不是令牌 ——
/// 偷来的令牌绑不到攻击者的账号上。
///
/// 三种失败各有自己的状态码（`binding.rs` 的 `http_status`）：**410 Gone**（令牌未知 /
/// 已消费 / 已过期 / 属于别的 adapter）、**409 Conflict**（这个 `WeCom` userid 已属于另一个
/// 用户）、**403 Forbidden**（兑换者不是该 workspace 的成员）。
async fn redeem_binding(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Json(body): Json<RedeemWecomBindingTokenRequest>,
) -> ApiResult<Response> {
    if !configured(&state) {
        return Ok(feature_disabled());
    }
    let user_id = user.ok_or_else(unauthorized)?.id();
    let token = body.token.trim();
    if token.is_empty() {
        return Err(bad_request("token is required").into());
    }
    let service = BindingTokenService::new(Arc::new(PgBindingStore::new(state.db.clone()))
        as Arc<dyn mc_channel::wecom::binding::BindingStore>);
    match service.redeem(token, user_id).await {
        Ok(RedeemOutcome::Bound(bound)) => Ok(Json(RedeemWecomBindingTokenResponse {
            workspace_id: bound.workspace_id.to_string(),
            installation_id: bound.installation_id.to_string(),
            wecom_user_id: bound.channel_user_id,
        })
        .into_response()),
        Ok(outcome) => {
            // 非 `Bound` 的判决都有错误映射（`from_redeem` 的契约）。
            let error = BindingError::from_redeem(&outcome).unwrap_or(BindingError::TokenInvalid);
            Ok(binding_error(&error))
        }
        Err(error) => {
            tracing::warn!(code = error.code(), "wecom binding redeem failed");
            Err(Error::Database(error.to_string()).into())
        }
    }
}

// =====================================================================
// 共享小件
// =====================================================================

/// 绑定失败的响应矩阵（逐条对齐上游 `RedeemWecomBindingToken` 的 switch）。
///
/// 状态码与错误码都取自 adapter（[`BindingError::http_status`] / [`BindingError::code`]），
/// 这里只补上游那句人话文案 —— 于是两条来源不会各自漂。
fn binding_error(error: &BindingError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let message = match error {
        BindingError::TokenInvalid => "binding token invalid or expired",
        BindingError::AlreadyAssigned => {
            "this WeCom user is already bound to a different Multica user"
        }
        BindingError::NotMember => "binding refused (are you a workspace member?)",
        BindingError::Store { .. } => "failed to redeem token",
    };
    error_with_code(status, error.code(), message)
}

/// 广播一条安装事件（上游 `EventWecomInstallationCreated` / `EventWecomInstallationRevoked`，
/// 两个 `type` **逐字**：`wecom_installation:{created,revoked}`）。
fn publish_event(state: &AppState, workspace_id: Id, action: &str, id: Id) {
    let kind = "wecom_installation";
    let envelope = mc_realtime::EventEnvelope::new(
        kind,
        workspace_id.to_string(),
        None,
        serde_json::json!({ "id": id.to_string() }),
    )
    .with_type(format!("{kind}:{action}"));
    state.realtime.publish(envelope);
}
