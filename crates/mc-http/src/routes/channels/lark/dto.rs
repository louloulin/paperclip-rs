//! lark 渠道面的 **wire 形状与公开小件**（写者 **M7-14**）。
//!
//! 拆出来是**门 ⑩**（单文件 800 行硬限）的要求；切点是「wire / SQL / handler」：
//! 本文件只有 DTO、错误信封与未配置语义，**没有任何** handler 与 SQL
//! （与 `wecom/{dto,store}.rs`、`telegram/`、`dingtalk/{dto,store}.rs` 同手法；登记见
//! `docs/32` §30 的 **D10**）。
//!
//! - DTO 的字段名**逐字**对齐上游 `handler/lark.go` 的四个结构；
//! - [`LarkInstallationResponse`] **不含** `app_secret_encrypted`（上游逐字：*"the encrypted
//!   blob is server-internal and there is no product reason to expose it"*）—— 也不含
//!   `ws_lease_*`（运行时状态不是 API 面）；
//! - `app_id` **在**（`cli_…`：管理后台可见，不是秘密；上游日志逐字打印它）；
//! - [`error_with_code`] 是本仓的**嵌套**错误信封 + 上游的稳定 `code`（形状与 `mc_errors`
//!   一致；各切片各自持有本地副本，`routes/agents.rs` 的注释逐字如此）。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use mc_channel::lark::installation::Installation;
use mc_channel::lark::registration::{InstallSessionState, SessionStatus};
use mc_core::channel::ChannelKind;
use mc_errors::{Error, ErrorBody, ErrorResponse};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// 「未配置」的错误码（上游 `writeFeatureDisabled(w, "lark_not_configured", …)` 的字面量）。
pub const CODE_LARK_NOT_CONFIGURED: &str = "lark_not_configured";

/// 一次 lark 安装端点请求体的上限（上游那两个 JSON 端点都是几个短字段）。
///
/// `serde_json` 会把一个字符串值**整个**物化之后再返回 ⇒ 没有这个上限，一个已认证的调用方
/// POST 一个几 GB 的 token 字符串就能让 API 服务器为每个请求花掉同样多的 RSS。
pub const BODY_LIMIT: usize = 16 * 1024;

/// 本仓标准的**嵌套**错误信封 + 上游的稳定 `code`（`{"error":{"code":…,"message":…}}`）。
pub(crate) fn error_with_code(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorResponse::new(code, message),
        }),
    )
        .into_response()
}

/// 部署密钥缺失时的统一响应（**403**，不是 503 —— 被关掉的能力不是瞬时故障，
/// 回 503 会招来重试与告警噪音）。
pub(crate) fn feature_disabled() -> Response {
    error_with_code(
        StatusCode::FORBIDDEN,
        CODE_LARK_NOT_CONFIGURED,
        "lark integration not configured",
    )
}

/// 本部署是否配了 lark 的落库加密密钥（`MULTICA_LARK_SECRET_KEY`）。
pub(crate) fn configured(state: &AppState) -> bool {
    state.channel_keys.is_configured(ChannelKind::Lark)
}

/// 未配置分支的判据（用例要能直接断言"就是它"）。
#[must_use]
pub fn is_configured(state: &AppState) -> bool {
    configured(state)
}

/// 缺身份时的 401。
pub(crate) fn unauthorized() -> Error {
    Error::Unauthorized {
        message: "missing X-Multica-User-Id header (M1 dev-mode auth)".to_string(),
    }
}

/// `DateTime<Utc>` → RFC3339（上游 `time.Time.UTC().Format(time.RFC3339)`）。
pub(crate) fn rfc3339(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// =====================================================================
// wire 形状（上游 `handler/lark.go`）
// =====================================================================

/// 一条安装的对外形状（上游 `LarkInstallationResponse`）。
///
/// **`app_secret_encrypted` 故意缺席**：它是服务端内部的密文，没有产品理由暴露它
/// （唯一需要明文的是 WS hub，它在服务端调解密）。`ws_lease_*` 同样不出现（运行时状态）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LarkInstallationResponse {
    pub id: String,
    pub workspace_id: String,
    pub agent_id: String,
    pub app_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_key: Option<String>,
    pub bot_open_id: String,
    pub installer_user_id: String,
    pub status: String,
    /// 该安装所在的云：`feishu`（大陆）或 `lark`（国际）。UI 用它渲染徽标并拼对
    /// "在 Lark 里管理"的开发台主机。
    pub region: String,
    pub installed_at: String,
    pub created_at: String,
    pub updated_at: String,
}

impl LarkInstallationResponse {
    /// 上游 `larkInstallationToResponse`。
    #[must_use]
    pub fn from_installation(installation: &Installation) -> Self {
        Self {
            id: installation.id.to_string(),
            workspace_id: installation.workspace_id.to_string(),
            agent_id: installation.agent_id.to_string(),
            app_id: installation.app_id.clone(),
            tenant_key: installation.tenant_key.clone(),
            bot_open_id: installation.bot_open_id.as_str().to_string(),
            installer_user_id: installation.installer_user_id.to_string(),
            status: installation.status.clone(),
            region: installation.region.as_str().to_string(),
            installed_at: rfc3339(installation.installed_at),
            created_at: rfc3339(installation.created_at),
            updated_at: rfc3339(installation.updated_at),
        }
    }
}

/// `GET …/lark/installations` 的信封（上游 `map[string]any` 的三格 —— **同生同死**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LarkInstallationsResponse {
    pub installations: Vec<LarkInstallationResponse>,
    /// 落库加密密钥存在（`MULTICA_LARK_SECRET_KEY`）。
    pub configured: bool,
    /// 设备流安装**端到端**接好了：注册服务在**且** `ApiClient.is_configured()` 为真
    /// （真 HTTP 客户端在位的信号 —— 替身完不成 poll 之后的 `GetBotInfo`）。
    pub install_supported: bool,
}

impl LarkInstallationsResponse {
    /// 未配置的那一版：**两个 `false`**，且**不**查库、**不**读身份
    /// （上游那个 handler 的第一句就是 `if h.LarkInstallations == nil`）。
    ///
    /// ⚠️ 这一格正是 ⑨ 那三条 `TestListLarkInstallations_NotConfigured*` 钉的形状：
    /// **200** + 空列表 + `configured:false` + `install_supported:false`，**不是** 403/503。
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
    pub fn configured_with(
        installations: Vec<LarkInstallationResponse>,
        install_supported: bool,
    ) -> Self {
        Self {
            installations,
            configured: true,
            install_supported,
        }
    }
}

/// `POST …/lark/install/begin` 的响应（上游 `BeginLarkInstallResponse`，四个键**逐字**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeginLarkInstallResponse {
    /// 浏览器用它轮询 status 的不透明句柄（**不是** `device_code`）。
    pub session_id: String,
    /// QR 目标（已带 SDK 遥测参数）。
    pub qr_code_url: String,
    pub expires_in_seconds: u64,
    pub poll_interval_seconds: u64,
}

/// `GET …/lark/install/:sessionId/status` 的轮询载荷（上游 `LarkInstallStatusResponse`）。
///
/// `status` 是三态之一（`pending` / `success` / `error`）；成功时带 `installation_id`，
/// 失败时 `error_reason` 是一个**稳定码**（[`mc_channel::lark::registration::reason`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LarkInstallStatusResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl LarkInstallStatusResponse {
    /// 从一个会话状态投影（成功带 `installation_id`；失败带稳定码与文案；`pending` 两个都不带）。
    #[must_use]
    pub fn from_session(state: &InstallSessionState) -> Self {
        let success = state.status == SessionStatus::Success;
        let failed = state.status == SessionStatus::Error;
        Self {
            status: state.status.as_str().to_string(),
            installation_id: success
                .then(|| state.installation_id.map(|id| id.to_string()))
                .flatten(),
            error_reason: (failed && !state.error_reason.is_empty())
                .then(|| state.error_reason.clone()),
            error_message: (failed && !state.error_message.is_empty())
                .then(|| state.error_message.clone()),
        }
    }
}

/// 兑换令牌的请求体（上游 `RedeemLarkBindingTokenRequest`）。
#[derive(Debug, Clone, Deserialize)]
pub struct RedeemLarkBindingTokenRequest {
    #[serde(default)]
    pub token: String,
}

/// 兑换成功的响应（上游 `RedeemLarkBindingTokenResponse`，键名**逐字**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedeemLarkBindingTokenResponse {
    pub workspace_id: String,
    pub installation_id: String,
    pub lark_open_id: String,
}

/// `POST …/lark/install/begin` 的查询参数（上游从 `r.URL.Query()` 读 `agent_id` 与 `region`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BeginInstallQuery {
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub region: String,
}

impl BeginInstallQuery {
    /// `region` 的三档判据（上游 `switch` 逐字）：空、`feishu`、`lark` 之外**一律 400**。
    ///
    /// 空 ⇒ 回落飞书（`Region::or_default`）。未知值**不**静默归一 —— 那会掩盖一个"用户选了
    /// 一个不存在的云"的前端回归（上游注释逐字：*"the service would normalize an unknown
    /// value to Feishu silently and that would mask a frontend regression"*）。
    #[must_use]
    pub fn region_is_acceptable(&self) -> bool {
        matches!(
            self.region.trim().to_ascii_lowercase().as_str(),
            "" | "feishu" | "lark"
        )
    }
}
