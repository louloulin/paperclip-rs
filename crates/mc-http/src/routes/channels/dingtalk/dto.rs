//! `DingTalk` 渠道面的 **wire 形状与公开小件**（写者 **M7-9**）。
//!
//! 拆出来是**门 ⑩**（单文件 800 行硬限）的要求；切点是「wire / 装配 / handler」：
//! 本文件只有 DTO、错误信封、绑定链接与环境变量读取，**没有任何** handler 与 SQL。
//!
//! - DTO 的字段名**逐字**对齐上游 `handler/dingtalk.go` 的四个响应结构；
//! - [`DingTalkInstallationResponse`] **不含** `config`（它是密文，且是服务端内部的事）；
//! - [`error_with_code`] 是本仓的**嵌套**错误信封 + 上游的稳定 `code`（形状与 `mc_errors`
//!   一致；各切片各自持有本地副本，`routes/agents.rs` 的注释逐字）。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use mc_channel::dingtalk::install::InstallRecord;
use mc_core::channel::ChannelKind;
use mc_errors::{Error, ErrorBody, ErrorResponse};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// 「未配置」的错误码（上游 `writeFeatureDisabled(w, "dingtalk_not_configured", …)` 的字面量）。
pub const CODE_DINGTALK_NOT_CONFIGURED: &str = "dingtalk_not_configured";

/// 绑定链接的 web 主机环境变量（上游 `MULTICA_APP_URL`，回落 `FRONTEND_ORIGIN`）。
pub const APP_URL_ENV: &str = "MULTICA_APP_URL";
/// 上一条的回落变量。
pub const FRONTEND_ORIGIN_ENV: &str = "FRONTEND_ORIGIN";

/// 绑定页路径（上游 `BindingPath` 零值 ⇒ `/dingtalk/bind`）。
pub const BINDING_PATH: &str = "/dingtalk/bind";

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
pub(crate) fn feature_disabled() -> Response {
    error_with_code(
        StatusCode::FORBIDDEN,
        CODE_DINGTALK_NOT_CONFIGURED,
        "dingtalk integration not enabled",
    )
}

/// 本部署是否配了 `DingTalk` 的落库加密密钥。
pub(crate) fn configured(state: &AppState) -> bool {
    state.channel_keys.is_configured(ChannelKind::DingTalk)
}

/// 缺身份时的 401（未配置分支**不读**身份，见模块文档）。
pub(crate) fn unauthorized() -> Error {
    Error::Unauthorized {
        message: "missing X-Multica-User-Id header (M1 dev-mode auth)".to_string(),
    }
}

/// 绑定链接的 web 主机（`MULTICA_APP_URL` → `FRONTEND_ORIGIN` → 空串）。
///
/// **公开**是故意的：宿主装配出站回复器时要把同一个值交给 `DingTalkOutboundReplier`
/// （那是本仓**唯一**读 env 的地方，`mc-channel` 不得自己 `std::env::var`）。
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
pub(crate) fn rfc3339(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
// =====================================================================
// wire 形状（上游 `handler/dingtalk.go`）
// =====================================================================

/// 一条 `DingTalk` 安装的对外形状（上游 `dingTalkInstallationToResponse` + 两个附加列）。
///
/// **`config` 故意缺席**：它是密文，且是服务端内部的事（只有出站发送器解它）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DingTalkInstallationResponse {
    pub id: String,
    pub workspace_id: String,
    pub agent_id: String,
    pub installer_user_id: String,
    pub status: String,
    pub installed_at: String,
    pub created_at: String,
    pub updated_at: String,
    /// agent 行还在不在（孤儿安装 ⇒ `false`，管理员据此分清"看不见"与"agent 被删了"）。
    pub agent_available: bool,
    /// **只看自己**的 `DingTalk` 身份（上游逐字：返回每个人的 staff id 会把身份暴露得比必要更宽）。
    /// 非管理员拿到 `None` ⇒ JSON 里**缺席**（`omitempty`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_dingtalk_user_ids: Option<Vec<String>>,
}

impl DingTalkInstallationResponse {
    /// 上游 `dingtalkInstallationToResponse`（对外可见的只有安装行本身）。
    #[must_use]
    pub fn from_record(record: &InstallRecord) -> Self {
        Self {
            id: record.id.to_string(),
            workspace_id: record.workspace_id.to_string(),
            agent_id: record.agent_id.to_string(),
            installer_user_id: record.installer_user_id.to_string(),
            status: record.status.clone(),
            installed_at: rfc3339(record.installed_at),
            created_at: rfc3339(record.created_at),
            updated_at: rfc3339(record.updated_at),
            agent_available: true,
            bound_dingtalk_user_ids: None,
        }
    }
}

/// `GET` 的信封（上游 `map[string]any` 的三格 —— **同生同死**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DingTalkInstallationsResponse {
    pub installations: Vec<DingTalkInstallationResponse>,
    /// 落库加密密钥存在（`MULTICA_DINGTALK_SECRET_KEY`）。
    pub configured: bool,
    /// 管理 UI 用的能力位；BYO 只需要落库密钥（**不**需要托管凭据）⇒ 与 `configured` 同值。
    pub install_supported: bool,
}

impl DingTalkInstallationsResponse {
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
    pub fn configured_with(installations: Vec<DingTalkInstallationResponse>) -> Self {
        Self {
            installations,
            configured: true,
            install_supported: true,
        }
    }
}

/// BYO 安装的请求体（上游 `RegisterDingTalkBYORequest`，两个键**逐字**）。
#[derive(Debug, Clone, Deserialize)]
pub struct RegisterDingTalkByoRequest {
    /// `AppKey`（client id）。
    #[serde(default)]
    pub client_id: String,
    /// `AppSecret`（client secret）。
    #[serde(default)]
    pub client_secret: String,
}

/// 兑换令牌的请求体（上游 `RedeemDingTalkBindingTokenRequest`）。
#[derive(Debug, Clone, Deserialize)]
pub struct RedeemDingTalkBindingTokenRequest {
    #[serde(default)]
    pub token: String,
}

/// 兑换成功的响应（上游 `RedeemDingTalkBindingTokenResponse`，键名**逐字**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedeemDingTalkBindingTokenResponse {
    pub workspace_id: String,
    pub installation_id: String,
    pub dingtalk_user_id: String,
}

/// `POST …/install/byo` 的查询参数（上游从 `r.URL.Query()` 读 `agent_id`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ByoQuery {
    #[serde(default)]
    pub agent_id: String,
}
/// 绑定页的完整 URL（出站回复器拼链接时用；本文件只提供它给宿主 / 诊断）。
#[must_use]
pub fn binding_url(app_url: &str, token: &str) -> String {
    let base = app_url.trim_end_matches('/');
    format!("{base}{BINDING_PATH}?token={}", url_encode(token))
}

/// `url.QueryEscape` / `encodeURIComponent` 的等价物：**未保留字符集之外**一律百分号编码。
///
/// 与 `mc_channel::telegram::replier::url_encode` / `mc_channel::slack::replier::url_encode`
/// 同一份实现（两侧各自持有一份，本仓既有约定）。空格 → `+`，逐字保留 Go 的行为。
#[must_use]
pub fn url_encode(raw: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            // `write!` 到一个 `String` 永不失败（`String` 的 `fmt::Write` 是 infallible 的）。
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// 未配置分支的判据（用例要能直接断言"就是它"）。
#[must_use]
pub fn is_configured(state: &AppState) -> bool {
    configured(state)
}
