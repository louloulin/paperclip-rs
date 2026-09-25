//! `WeCom` 渠道面的 **wire 形状与公开小件**（写者 **M7-15**）。
//!
//! 拆出来是**门 ⑩**（单文件 800 行硬限）的要求；切点是「wire / SQL / handler」：
//! 本文件只有 DTO、错误信封、绑定链接，**没有任何** handler 与 SQL。
//! （与 `dingtalk/{dto,store}.rs`、`slack/`、`telegram/` 同手法；登记见 `docs/32` §31 的 D8。）
//!
//! - DTO 的字段名**逐字**对齐上游 `handler/wecom_web.go` 的五个结构；
//! - [`WecomInstallationResponse`] **不含** `config`（那是密文，也是服务端内部的事 ——
//!   `bot_id` 之所以可以出现，是因为它**不是**秘密：管理后台可见，任何工作区成员都能从
//!   `GET …/wecom/installations` 读回来）；
//! - [`error_with_code`] 是本仓的**嵌套**错误信封 + 上游的稳定 `code`（形状与 `mc_errors`
//!   一致；各切片各自持有本地副本，`routes/agents.rs` 的注释逐字如此）。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use mc_channel::wecom::types::Installation;
use mc_core::channel::ChannelKind;
use mc_errors::{Error, ErrorBody, ErrorResponse};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// 「未配置」的错误码（上游 `writeFeatureDisabled(w, "wecom_not_configured", …)` 的字面量）。
pub const CODE_WECOM_NOT_CONFIGURED: &str = "wecom_not_configured";

/// 绑定链接的 web 主机环境变量（上游 `MULTICA_APP_URL`，回落 `FRONTEND_ORIGIN`）。
pub const APP_URL_ENV: &str = "MULTICA_APP_URL";
/// 上一条的回落变量。
pub const FRONTEND_ORIGIN_ENV: &str = "FRONTEND_ORIGIN";

/// 绑定页路径（上游 `wecom/replier.go` 的默认 `BindingPath`，逐字）。
pub const BINDING_PATH: &str = "/wecom/bind";

/// 一次 `WeCom` JSON 端点的请求体上限（上游 `wecomBodyLimit = 16 * 1024`）。
///
/// 两个端点的主体都是几个短字段，而 `serde_json` 会把一个字符串值**整个**物化之后再返回 ⇒
/// 没有这个上限，一个已认证的调用方 POST 一个几 GB 的 token 字符串就能让 API 服务器为每个
/// 请求花掉同样多的 RSS。
pub const BODY_LIMIT: usize = 16 * 1024;

/// 本仓标准的**嵌套**错误信封 + 上游的稳定 `code`
/// （`{"error":{"code":…,"message":…}}`）。
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
        CODE_WECOM_NOT_CONFIGURED,
        "wecom integration not enabled",
    )
}

/// 本部署是否配了 `WeCom` 的落库加密密钥（`MULTICA_WECOM_SECRET_KEY`）。
pub(crate) fn configured(state: &AppState) -> bool {
    state.channel_keys.is_configured(ChannelKind::WeCom)
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
// wire 形状（上游 `handler/wecom_web.go`）
// =====================================================================

/// 一条安装的对外形状（上游 `WecomInstallationResponse`）。
///
/// **`config` 故意缺席**：它含密文。`bot_id` 在，因为它是管理后台可见的标识，不是秘密。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WecomInstallationResponse {
    pub id: String,
    pub workspace_id: String,
    pub agent_id: String,
    pub bot_id: String,
    pub installer_user_id: String,
    pub status: String,
    pub installed_at: String,
    pub created_at: String,
    pub updated_at: String,
}

impl WecomInstallationResponse {
    /// 上游 `wecomInstallationToResponse`。
    #[must_use]
    pub fn from_installation(installation: &Installation) -> Self {
        Self {
            id: installation.id.to_string(),
            workspace_id: installation.workspace_id.to_string(),
            agent_id: installation.agent_id.to_string(),
            bot_id: installation.public_bot_id().to_string(),
            installer_user_id: installation.installer_user_id.to_string(),
            status: installation.status.as_str().to_string(),
            installed_at: rfc3339(installation.installed_at),
            created_at: rfc3339(installation.created_at),
            updated_at: rfc3339(installation.updated_at),
        }
    }
}

/// `GET …/wecom/installations` 的信封（上游 `map[string]any` 的三格 —— **同生同死**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WecomInstallationsResponse {
    pub installations: Vec<WecomInstallationResponse>,
    /// 落库加密密钥存在（`MULTICA_WECOM_SECRET_KEY`）。
    pub configured: bool,
    /// 管理 UI 用的能力位；BYO 只需要落库密钥（**不**需要托管凭据）⇒ 与 `configured` 同值。
    pub install_supported: bool,
}

impl WecomInstallationsResponse {
    /// 未配置的那一版（**不**查库、**不**读身份）。
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
    pub fn configured_with(installations: Vec<WecomInstallationResponse>) -> Self {
        Self {
            installations,
            configured: true,
            install_supported: true,
        }
    }
}

/// BYO 安装的请求体（上游 `RegisterWecomBYORequest`，三个键**逐字**）。
///
/// `secret` 是**明文凭据** ⇒ 本结构**不派生 `Debug`**（`docs/60` §2.3 第 1 条：
/// 默认 `Debug` 会把它写进任何 `{:?}` 插值、`assert_eq!` 失败回显与 panic backtrace）。
#[derive(Clone, Deserialize)]
pub struct RegisterWecomByoRequest {
    /// 智能机器人的标识（管理后台可见，**不是秘密**）。
    #[serde(default)]
    pub bot_id: String,
    /// 长连接密钥（只在创建机器人时显示一次）。
    #[serde(default)]
    pub secret: String,
    /// 机器人在会话里的显示名（可选；上游 `bot_name`）。
    #[serde(default)]
    pub bot_name: String,
}

/// 兑换令牌的请求体（上游 `RedeemWecomBindingTokenRequest`）。
#[derive(Debug, Clone, Deserialize)]
pub struct RedeemWecomBindingTokenRequest {
    #[serde(default)]
    pub token: String,
}

/// 兑换成功的响应（上游 `RedeemWecomBindingTokenResponse`，键名**逐字**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedeemWecomBindingTokenResponse {
    pub workspace_id: String,
    pub installation_id: String,
    pub wecom_user_id: String,
}

/// `POST …/install/byo` 的查询参数（上游从 `r.URL.Query()` 读 `agent_id`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ByoQuery {
    #[serde(default)]
    pub agent_id: String,
}

/// 绑定页的完整 URL（出站回复器拼"点这里绑定"链接时用；M7-17 的 `replier.rs` 消费它）。
#[must_use]
pub fn binding_url(app_url: &str, token: &str) -> String {
    let base = app_url.trim_end_matches('/');
    format!("{base}{BINDING_PATH}?token={}", url_encode(token))
}

/// 绑定链接的 web 主机（`MULTICA_APP_URL` → `FRONTEND_ORIGIN` → 空串）。
///
/// **公开**是故意的：宿主装配出站回复器时要把同一个值交给 M7-17 的 replier
/// （这是本仓**唯一**读 env 的地方，`mc-channel` 不得自己 `std::env::var`）。
#[must_use]
pub fn app_url() -> String {
    for name in [APP_URL_ENV, FRONTEND_ORIGIN_ENV] {
        if let Ok(raw) = std::env::var(name) {
            let trimmed = raw.trim().trim_end_matches('/');
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

/// `url.QueryEscape` / `encodeURIComponent` 的等价物：保留字符集之外一律百分号编码。
///
/// 与 `dingtalk` / `slack` / `telegram` 各自的副本同一份实现（本仓既有约定是各切片各持一份）。
/// 空格 → `+`，逐字保留 Go 的行为。
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
            // 写进 `String` 的 `fmt::Write` 不会失败。
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}
