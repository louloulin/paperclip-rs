//! Telegram 安装与绑定面：**4 条**路由 —— 写者 **M7-5**（`docs/60-M7-PLAN.md` §1.1 / §3.3）。
//!
//! | 注册键 | 方法 | 授权层 | 未配置语义 |
//! | --- | :-: | --- | --- |
//! | `/api/workspaces/:id/telegram/installations` | GET | workspace **member** | **200** + `installations: []` / `configured:false` / `install_supported:false` |
//! | `/api/workspaces/:id/telegram/installations/:installationId` | DELETE | workspace **admin** | **403** `telegram_not_configured` |
//! | `/api/workspaces/:id/telegram/install` | POST | workspace **admin** | **403** `telegram_not_configured` |
//! | `/api/telegram/binding/redeem` | POST | 登录用户（**无** workspace 前缀） | **403** `telegram_not_configured` |
//!
//! - **上游**：`internal/handler/telegram.go`（265 行）+ `internal/integrations/telegram/{install,binding}.go`。
//! - **业务逻辑在 adapter**：安装/绑定的真正实现在 `mc_channel::telegram::{install,binding}`，
//!   本文件只做四件事 —— **鉴权层**、**wire 形状**、**未配置语义**、以及把「channel 层不碰 SQL」
//!   这条边界铁律落成两个**端口实现**（[`store::PgInstallStore`] / [`store::PgBindingStore`]）。
//! - **形态纪律**：只按上游字面量注册**那一形态**（M7 的 `dual-form required: 0` ⇒ 补尾斜杠是
//!   `EXTRA_ALIAS`、漏字面量是 `MISSING_EXACT`，两类都是硬失败）。路径参数必须写 `:name`
//!   （matchit 0.7 把 `{name}` 当字面量 ⇒ 编译通过且恒 404）。
//!
//! # 未配置语义**逐端点不同**（`docs/60` §2.4 / R-M7-3，不许"统一 503"）
//!
//! 上游的判据是 `h.TelegramInstall == nil`（= 落库加密密钥缺失）。本仓的对应物是
//! [`AppState::channel_keys`] 里 Telegram 那一格（`MULTICA_TELEGRAM_SECRET_KEY`）。两条断言：
//!
//! 1. **列表**回 200 空 + 两个 `false` —— 管理页要能渲染"未接入"这一态（**不**查库）；
//! 2. **安装 / 撤销 / 兑换**回 **403 `telegram_not_configured`**（上游 `writeFeatureDisabled`；
//!    被关掉的能力不是瞬时故障，回 503 会招来重试与告警噪音）。
//!
//! ## 未配置分支**先于**鉴权（⑨ 的判据，登记 `docs/32` §17.2）
//!
//! 上游这四条 handler 的第一句都是 `if h.TelegramInstall == nil { …; return }` —— 它**不读**任何
//! 身份。本仓的 `AuthUser` 是提取器（它在 handler 体之前跑），若照抄 slack 的"先解析 scope，
//! 再看 configured"，未配置分支会被 401 挡在前面。所以本文件用 `Option<AuthUser>`：**未配置
//! 分支先返回**（与上游同序），**已配置分支**再要求身份（缺身份 ⇒ 401）。
//! ⑨ 的 `workspaces/TestListTelegramInstallationsNotConfiguredReturnsEmpty`
//! （actor `anonymous`、期望 200）正是这条的判据。
//!
//! # 凭据纪律（`docs/60` §2.3 / §6.5 的 M7-5 行）
//!
//! - 贴进来的 bot token **只经 `secretbox` 密文入库**（`mc_channel::telegram::install` 的职责），
//!   本文件的 SQL 只搬运**已经封好的** `config`；
//! - 响应 DTO（[`TelegramInstallationResponse`]）**不含** config（它是密文，且是服务端内部的事）；
//! - 本文件**没有任何** `tracing::*` 插值 token / 密文；错误文案只带 Telegram 自己的错误码。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use mc_channel::telegram::api::JsonBotApi;
use mc_channel::telegram::binding::{RedeemOutcome, BINDING_TOKEN_TTL};
use mc_channel::telegram::install::{InstallError, InstallRecord, InstallService, InstallStore};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_errors::{Error, ErrorBody, ErrorResponse};
use mc_repos::agent::role_is_admin;
use serde::{Deserialize, Serialize};

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, forbidden, not_found, parse_uuid, workspace_role};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;
use store::{PgBindingStore, PgInstallStore};

/// 本文件的路由切片（**逐字**上游路径；见模块头的形态纪律）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/workspaces/:id/telegram/installations",
            get(list_installations),
        )
        .route(
            "/api/workspaces/:id/telegram/installations/:installationId",
            delete(revoke_installation),
        )
        .route(
            "/api/workspaces/:id/telegram/install",
            post(register_installation),
        )
        .route("/api/telegram/binding/redeem", post(redeem_binding))
}

/// 「未配置」的错误码（上游 `writeFeatureDisabled(w, "telegram_not_configured", …)` 的字面量）。
pub const CODE_TELEGRAM_NOT_CONFIGURED: &str = "telegram_not_configured";

/// 绑定链接的 web 主机环境变量（上游 `MULTICA_APP_URL`，回落 `FRONTEND_ORIGIN`）。
pub const APP_URL_ENV: &str = "MULTICA_APP_URL";
/// 上一条的回落变量。
pub const FRONTEND_ORIGIN_ENV: &str = "FRONTEND_ORIGIN";

/// 绑定页路径（上游 `BindingPath` 零值 ⇒ `/telegram/bind`）。
pub const BINDING_PATH: &str = "/telegram/bind";

/// 本仓标准的**嵌套**错误信封 + 上游的稳定 `code`
/// （`{"error":{"code":…,"message":…}}`；形状与 `mc_errors` 一致）。
///
/// 与 `routes::channels::slack::error_with_code` 同形 —— 本仓既有约定是「各切片各自持有本地
/// 副本」（`routes/agents.rs` 的注释逐字），所以这里再写一份。
pub(crate) fn error_with_code(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorResponse::new(code, message),
        }),
    )
        .into_response()
}

/// 部署密钥缺失时的统一响应（403，**不是** 503）。
fn feature_disabled() -> Response {
    error_with_code(
        StatusCode::FORBIDDEN,
        CODE_TELEGRAM_NOT_CONFIGURED,
        "telegram integration not enabled",
    )
}

/// 本部署是否配了 Telegram 的落库加密密钥。
fn configured(state: &AppState) -> bool {
    state.channel_keys.is_configured(ChannelKind::Telegram)
}

/// 缺身份时的 401（已配置的那条路径才用；未配置分支**不读**身份，见模块文档）。
fn unauthorized() -> Error {
    Error::Unauthorized {
        message: "missing X-Multica-User-Id header (M1 dev-mode auth)".to_string(),
    }
}

/// 绑定链接的 web 主机（`MULTICA_APP_URL` → `FRONTEND_ORIGIN` → 空串）。
///
/// **公开**是故意的：宿主装配出站回复器时要把同一个值交给
/// `TelegramOutboundReplier`（那是本仓**唯一**读 env 的地方，`mc-channel` 不得自己
/// `std::env::var`）。
#[must_use]
pub fn app_url() -> String {
    for name in [APP_URL_ENV, FRONTEND_ORIGIN_ENV] {
        if let Ok(raw) = std::env::var(name) {
            let trimmed = raw.trim().trim_end_matches('/').to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
    }
    String::new()
}

/// `DateTime<Utc>` → RFC3339（上游 `time.Time.UTC().Format(time.RFC3339)`）。
fn rfc3339(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// =====================================================================
// wire 形状（上游 `handler/telegram.go`）
// =====================================================================

/// 一条 Telegram 安装的对外形状（上游 `telegramInstallationToResponse`）。
///
/// **`config` 故意缺席**：它是密文，且是服务端内部的事（只有出站发送器解它）。
/// 身份列取自 config 的**非密**子集（`app_id` / `bot_username`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramInstallationResponse {
    pub id: String,
    pub workspace_id: String,
    pub agent_id: String,
    /// bot 的数值 id（上游字段名是 `bot_id`，**不是** slack 的 `team_id`）。
    pub bot_id: String,
    /// bot 用户名（`@-mention` 判定用的那个）。
    pub bot_username: String,
    pub installer_user_id: String,
    pub status: String,
    pub installed_at: String,
    pub created_at: String,
    pub updated_at: String,
}

impl TelegramInstallationResponse {
    /// 上游 `telegramInstallationToResponse`。
    #[must_use]
    pub fn from_record(record: &InstallRecord) -> Self {
        let public = record.public_config();
        Self {
            id: record.id.to_string(),
            workspace_id: record.workspace_id.to_string(),
            agent_id: record.agent_id.to_string(),
            bot_id: public.bot_id,
            bot_username: public.bot_username,
            installer_user_id: record.installer_user_id.to_string(),
            status: record.status.clone(),
            installed_at: rfc3339(record.installed_at),
            created_at: rfc3339(record.created_at),
            updated_at: rfc3339(record.updated_at),
        }
    }
}

/// `GET` 的信封（上游 `map[string]any` 的三格 —— **同生同死**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramInstallationsResponse {
    pub installations: Vec<TelegramInstallationResponse>,
    /// 落库加密密钥存在（`MULTICA_TELEGRAM_SECRET_KEY`）。
    pub configured: bool,
    /// 管理 UI 用的能力位；贴一个 token 只需要落库密钥（**不**需要托管凭据）⇒ 与 `configured` 同值。
    pub install_supported: bool,
}

impl TelegramInstallationsResponse {
    /// 未配置的那一版（**不**查库）。
    #[must_use]
    pub fn not_configured() -> Self {
        Self {
            installations: Vec::new(),
            configured: false,
            install_supported: false,
        }
    }

    /// 配好了的那一版。
    #[must_use]
    pub fn configured_with(records: &[InstallRecord]) -> Self {
        Self {
            installations: records
                .iter()
                .map(TelegramInstallationResponse::from_record)
                .collect(),
            configured: true,
            install_supported: true,
        }
    }
}

/// 安装的请求体（上游 `RegisterTelegramRequest`）：用户从 `@BotFather` 贴进来的 token。
#[derive(Debug, Clone, Deserialize)]
pub struct RegisterTelegramRequest {
    #[serde(default)]
    pub bot_token: String,
}

/// 兑换令牌的请求体（上游 `RedeemTelegramBindingTokenRequest`）。
#[derive(Debug, Clone, Deserialize)]
pub struct RedeemTelegramBindingTokenRequest {
    #[serde(default)]
    pub token: String,
}

/// 兑换成功的响应（上游 `RedeemTelegramBindingTokenResponse`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedeemTelegramBindingTokenResponse {
    pub workspace_id: String,
    pub installation_id: String,
    pub telegram_user_id: String,
}

/// `POST …/telegram/install` 的查询参数（上游从 `r.URL.Query()` 读 `agent_id`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InstallQuery {
    #[serde(default)]
    pub agent_id: String,
}

// =====================================================================
// 装配（四条 handler 共用）
// =====================================================================

/// 一次请求的 `(workspace, 调用者, 角色)`。
struct TelegramScope {
    workspace_id: Id,
    user_id: Id,
    role: String,
}

impl TelegramScope {
    /// 解析（**已配置**的那条路径才调用；未配置分支先返回，见模块文档）。
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

    /// 上游把 DELETE / POST 挂在 admin 组（`router.go:1829-1830`）；本仓没有 per-route 角色
    /// middleware ⇒ 在 handler 起点解析（与 M8-1 / M8-2 / M7-4 同款）。
    fn require_admin(&self) -> Result<(), Error> {
        if role_is_admin(&self.role) {
            Ok(())
        } else {
            Err(forbidden("insufficient permissions"))
        }
    }
}

/// 把 adapter 的安装错误映射到 HTTP（逐条对齐上游 `handler/telegram.go` 的 switch）。
fn install_error(error: &InstallError) -> Response {
    match error {
        InstallError::NotFound => {
            crate::error::ApiError::from(not_found("telegram installation")).into_response()
        }
        // 形状不对 ⇒ 400 + 上游自己的文案（提到 `123456:ABC-DEF…` 这个形状，**不含**粘贴值）。
        InstallError::InvalidBotToken => {
            error_with_code(StatusCode::BAD_REQUEST, error.code(), &error.to_string())
        }
        InstallError::CredentialsRejected => error_with_code(
            StatusCode::BAD_REQUEST,
            error.code(),
            "Telegram rejected this bot token — generate a current token in @BotFather and try again",
        ),
        // **503**：够不着 Telegram 不是用户的错，且明说"令牌没被保存"。
        InstallError::CredentialsUnverifiable => error_with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            error.code(),
            "could not reach Telegram to verify this bot — check the server network or proxy and \
             try again; the token was not saved",
        ),
        InstallError::OwnedBySameWorkspace => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "this Telegram bot is already connected to another agent in this workspace — \
             disconnect it there first, then connect it here",
        ),
        InstallError::OwnedByArchivedAgent => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "this Telegram bot is connected to an archived agent in this workspace — restore that \
             agent, or disconnect its bot, before connecting it here",
        ),
        InstallError::OwnedByAnotherWorkspace => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "this Telegram bot is already connected to a different Multica workspace — disconnect \
             it there before connecting it here",
        ),
        InstallError::WebhookConfigured => error_with_code(
            StatusCode::BAD_REQUEST,
            error.code(),
            "this Telegram bot has a webhook configured — remove the webhook before connecting it \
             with long polling",
        ),
        // 兜底 500：上游逐字 —— 且明说"令牌没被保存"（服务端问题不该让用户去轮换凭据）。
        other => error_with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            other.code(),
            "could not save this Telegram bot — something went wrong on the server; the token was \
             not saved",
        ),
    }
}

/// 装好的 BYO 服务（端口实现 + `reqwest` 的 Bot API）。
fn install_service(state: &AppState) -> Option<InstallService> {
    let boxed = state.channel_keys.get(ChannelKind::Telegram)?.clone();
    Some(InstallService::new(
        Arc::new(PgInstallStore::new(state.db.clone())),
        Arc::new(JsonBotApi::new()),
        boxed,
    ))
}

// =====================================================================
// GET /api/workspaces/:id/telegram/installations
// =====================================================================

/// 上游 `ListTelegramInstallations`：**member 可见**（非管理员也能渲染 Integrations 页）。
///
/// 未配置 ⇒ **不**查库、**不**读身份，回 200 空 + 两个 `false`（`docs/60` R-M7-3）。
async fn list_installations(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path(raw_workspace_id): Path<String>,
) -> ApiResult<Json<TelegramInstallationsResponse>> {
    if !configured(&state) {
        return Ok(Json(TelegramInstallationsResponse::not_configured()));
    }
    let scope =
        TelegramScope::resolve(&state, user.ok_or_else(unauthorized)?, &raw_workspace_id).await?;
    let store = PgInstallStore::new(state.db.clone());
    let records = InstallStore::list_by_workspace(&store, scope.workspace_id)
        .await
        .map_err(db_error)?;
    Ok(Json(TelegramInstallationsResponse::configured_with(
        &records,
    )))
}

// =====================================================================
// DELETE /api/workspaces/:id/telegram/installations/:installationId
// =====================================================================

/// 上游 `RevokeTelegramInstallation`：把状态翻成 `revoked`，**行保留**供审计
/// （聊天历史留在 Multica；重贴 token 会把状态翻回 `active`）。
async fn revoke_installation(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path((raw_workspace_id, raw_installation_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    if !configured(&state) {
        return Ok(feature_disabled());
    }
    let scope =
        TelegramScope::resolve(&state, user.ok_or_else(unauthorized)?, &raw_workspace_id).await?;
    scope.require_admin()?;
    let installation_id = Id(parse_uuid(&raw_installation_id, "installation id")?);
    let store = PgInstallStore::new(state.db.clone());
    // workspace 收窄的读：另一个 workspace 猜 id 也读不到（越权 = 与不存在同结果）。
    if InstallStore::get_in_workspace(&store, installation_id, scope.workspace_id)
        .await
        .map_err(db_error)?
        .is_none()
    {
        return Err(not_found("telegram installation").into());
    }
    InstallStore::revoke(&store, scope.workspace_id, installation_id)
        .await
        .map_err(db_error)?;
    publish_installation_event(&state, &scope.workspace_id, &installation_id, "revoked");
    Ok(StatusCode::NO_CONTENT.into_response())
}

// =====================================================================
// POST /api/workspaces/:id/telegram/install
// =====================================================================

/// 上游 `RegisterTelegramBot`：**admin only**，`agent_id` 在**查询串**里，token 在体里。
async fn register_installation(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path(raw_workspace_id): Path<String>,
    Query(query): Query<InstallQuery>,
    Json(body): Json<RegisterTelegramRequest>,
) -> ApiResult<Response> {
    let Some(service) = install_service(&state) else {
        return Ok(feature_disabled());
    };
    let scope =
        TelegramScope::resolve(&state, user.ok_or_else(unauthorized)?, &raw_workspace_id).await?;
    scope.require_admin()?;
    let raw_agent_id = query.agent_id.trim();
    if raw_agent_id.is_empty() {
        return Err(bad_request("agent_id is required").into());
    }
    let agent_id = Id(parse_uuid(raw_agent_id, "agent_id")?);
    // 边界上的所有权前置校验：错的 agent_id 是明确的 404（而不是落库时才发现）。
    let owned: Option<(i32,)> =
        sqlx::query_as("SELECT 1 FROM agent WHERE id = $1 AND workspace_id = $2")
            .bind(agent_id.0)
            .bind(scope.workspace_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|error| Error::Database(error.to_string()))?;
    if owned.is_none() {
        return Ok(error_with_code(
            StatusCode::NOT_FOUND,
            "agent_not_found",
            "agent not found in this workspace",
        ));
    }
    let params = mc_channel::telegram::install::RegisterParams::new(
        scope.workspace_id,
        agent_id,
        scope.user_id,
        body.bot_token,
    );
    match service.register(&params).await {
        Ok(record) => {
            publish_installation_event(&state, &scope.workspace_id, &record.id, "created");
            Ok(Json(TelegramInstallationResponse::from_record(&record)).into_response())
        }
        Err(error) => Ok(install_error(&error)),
    }
}

// =====================================================================
// POST /api/telegram/binding/redeem
// =====================================================================

/// 上游 `RedeemTelegramBindingToken`：**无** workspace 前缀（兑换者在拥有 workspace 上下文
/// **之前**就打它），身份来自**会话**而不是令牌。
///
/// 三种失败各有自己的状态码：`410 Gone`（令牌未知 / 已消费 / 已过期 / 属于别的 adapter）、
/// `409 Conflict`（这个 Telegram id 已属于另一个用户）、`403 Forbidden`（兑换者不是成员）。
async fn redeem_binding(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Json(body): Json<RedeemTelegramBindingTokenRequest>,
) -> ApiResult<Response> {
    if !configured(&state) {
        return Ok(feature_disabled());
    }
    let user_id = user.ok_or_else(unauthorized)?.id();
    let token = body.token.trim();
    if token.is_empty() {
        return Err(bad_request("token is required").into());
    }
    let service = mc_channel::telegram::binding::BindingTokenService::new(Arc::new(
        PgBindingStore::new(state.db.clone()),
    ));
    match service.redeem(token, user_id).await {
        Ok(RedeemOutcome::Bound(bound)) => Ok(Json(RedeemTelegramBindingTokenResponse {
            workspace_id: bound.workspace_id.to_string(),
            installation_id: bound.installation_id.to_string(),
            telegram_user_id: bound.channel_user_id,
        })
        .into_response()),
        Ok(RedeemOutcome::TokenInvalid) => Ok(error_with_code(
            StatusCode::GONE,
            "telegram_binding_token_invalid",
            "binding token invalid or expired",
        )),
        Ok(RedeemOutcome::AlreadyAssigned) => Ok(error_with_code(
            StatusCode::CONFLICT,
            "telegram_binding_already_assigned",
            "this Telegram account is already bound to a different Multica user",
        )),
        Ok(RedeemOutcome::NotMember) => Ok(error_with_code(
            StatusCode::FORBIDDEN,
            "telegram_binding_not_member",
            "binding refused (are you a workspace member?)",
        )),
        Err(error) => {
            tracing::warn!(code = error.code(), "telegram binding redeem failed");
            Err(db_error(error.to_string()).into())
        }
    }
}

/// 存储层故障 → 500（不透明：不回显 SQL / 参数）。
fn db_error(message: String) -> Error {
    Error::Database(message)
}

/// 广播一条安装事件（上游 `publishTelegramInstallationCreated` / `…Revoked`）。
fn publish_installation_event(state: &AppState, workspace_id: &Id, id: &Id, action: &str) {
    let envelope = mc_realtime::EventEnvelope::new(
        "telegram_installation",
        workspace_id.to_string(),
        None,
        serde_json::json!({ "id": id.to_string() }),
    )
    .with_type(format!("telegram_installation:{action}"));
    state.realtime.publish(envelope);
}

/// 绑定页的完整 URL（出站回复器拼链接时用；本文件只提供它给宿主 / 诊断）。
#[must_use]
pub fn binding_url(app_url: &str, token: &str) -> String {
    let base = app_url.trim_end_matches('/');
    format!(
        "{base}{BINDING_PATH}?token={}",
        mc_channel::telegram::replier::url_encode(token)
    )
}

/// 令牌寿命（HTTP 层要能在响应 / 文档里说明；唯一实现仍在 adapter）。
#[must_use]
pub fn binding_token_ttl() -> chrono::Duration {
    BINDING_TOKEN_TTL
}

/// 未配置分支的判据（用例要能直接断言"就是它"）。
#[must_use]
pub fn is_configured(state: &AppState) -> bool {
    configured(state)
}

pub mod store;

#[cfg(test)]
mod tests;
