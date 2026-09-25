//! VCS 面的响应 DTO 与 webhook URL 派生 —— 写者 **M8-2**（`LUM-1799`）。
//!
//! # 形状（上游 `internal/handler/vcs.go` L21–L78）
//!
//! | 类型 | 上游 | 落点 |
//! | --- | --- | --- |
//! | [`VcsConnectionResponse`] | `VCSConnectionResponse` | `GET` 列表的单条、`POST`/`rotate` 的基体 |
//! | [`VcsConnectResponse`] | `VCSConnectResponse`（嵌入基体 + **一次性明文 secret**） | connect / rotate 的响应 |
//! | [`VcsConnectionsResponse`] | `ListVCSConnections` 的 map 信封 | `GET` 列表 |
//!
//! # ⚠️ write-only：响应里**不得**出现 PAT / webhook secret 的密文或明文
//!
//! 两个凭据列（`access_token_encrypted` / `webhook_secret_encrypted`）**不进任何 DTO**：
//! 本文件的 [`VcsConnectionResponse::from_row`] 只读非敏感列。唯一的例外是 connect /
//! rotate 响应里那一次性的 `webhook_secret` **明文**（上游注释逐字：`Not retrievable after`）
//! —— 它的手写 `Debug` 也要脱敏（见 [`VcsConnectResponse`] 的 `Debug` 实现）。
//!
//! # `webhook_url` 的来源（本仓的一处替代，登记 `docs/32` §9.12）
//!
//! 上游是 `h.cfg.PublicURL`（配置字段）。本仓 `ConfigSnapshot` **没有** `public_url`
//! 字段，而 `state.rs` 由 M8-0 anchor 冻结、本片不得编辑 ⇒ 退回读 `MULTICA_PUBLIC_URL`
//! 环境变量（与 `mc_autopilot::dto::public_url()` 同一口径，变量名与上游注释逐字相同）。
//! 上游在 base 为空时返回 `""`；本仓同判（`webhook_path` 仍然总是有值，客户端可以自己拼）。
//!
//! 与 `mc_autopilot` 的做法有一处刻意不同：本文件用 `Mutex<Option<String>>` 而不是
//! `OnceLock`，并提供 [`set_vcs_public_base`] / [`reset_vcs_public_base`]，**唯一**理由是让
//! 端到端测试能在不污染进程 env 的前提下断言 `webhook_url`（与
//! `mc_http::routes::github::install` 的 `GITHUB_API_BASE` 同款）。生产代码**不得**调用它们。

use std::sync::Mutex;

use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_errors::{ErrorBody, ErrorResponse};
use mc_repos::vcs::connection::VcsConnectionRow;
use serde::{Deserialize, Serialize};

/// webhook 路径前缀（上游 `vcsWebhookPathPrefix` 逐字）。
pub const VCS_WEBHOOK_PATH_PREFIX: &str = "/api/webhooks/vcs/";

/// `MULTICA_PUBLIC_URL` 的变量名（上游是 `cfg.PublicURL`；变量名与上游注释逐字相同）。
pub const PUBLIC_URL_ENV: &str = "MULTICA_PUBLIC_URL";

/// 「未配置」的错误码（上游 `ConnectVCS` / `RotateVCSConnectionWebhook` 的字面量）。
pub const CODE_VCS_NOT_CONFIGURED: &str = "vcs_not_configured";

/// 存储的连接一行 → wire 形状（上游 `vcsConnectionToResponse`）。
///
/// **不含**两个凭据列 —— 这不是"忘了加"，是 `docs/61` §2.4 的硬约束。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VcsConnectionResponse {
    pub id: String,
    pub workspace_id: String,
    /// 存储字面量（`forgejo` / `gitea` / `gitlab`）。
    pub provider: String,
    pub instance_url: String,
    pub account_login: String,
    /// `MULTICA_PUBLIC_URL` 未配置时是**空串**（上游零值）。
    pub webhook_url: String,
    /// 相对路径，总是有值（`/api/webhooks/vcs/{connectionId}`）。
    pub webhook_path: String,
    pub created_at: String,
}

impl VcsConnectionResponse {
    /// 上游 `vcsConnectionToResponse`。
    pub fn from_row(row: &VcsConnectionRow) -> Self {
        let id = row.id.to_string();
        Self {
            webhook_path: webhook_path(&id),
            webhook_url: webhook_url(&id),
            id,
            workspace_id: row.workspace_id.to_string(),
            provider: row.provider.clone(),
            instance_url: row.instance_url.clone(),
            account_login: row.account_login.clone(),
            created_at: row.created_at.to_rfc3339(),
        }
    }
}

/// connect / rotate 的响应：连接本体 + **一次性**明文 webhook secret（上游 `VCSConnectResponse`）。
///
/// # 凭据纪律
///
/// `webhook_secret` 是本仓唯一允许出现在响应体里的明文凭据（上游语义如此：用户要把它粘进
/// provider 的 webhook 配置），但**手写 `Debug`** 把它显示成 `<redacted>` —— 否则任何
/// `assert_eq!` 失败、`tracing` 插值或 panic 回显都会把它写进日志。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VcsConnectResponse {
    #[serde(flatten)]
    pub connection: VcsConnectionResponse,
    /// 只此一次：之后的任何读面都不再返回它（上游注释逐字 `Not retrievable after`）。
    pub webhook_secret: String,
}

impl std::fmt::Debug for VcsConnectResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VcsConnectResponse")
            .field("connection", &self.connection)
            .field("webhook_secret", &"<redacted>")
            .finish()
    }
}

/// `GET /api/workspaces/{id}/vcs/connections` 的信封（上游 `ListVCSConnections`）。
///
/// 四个字段**同生同死**：产品边界关闭时上游只回 `available:false` 那一版
/// （`connections: []` + 其余三格取恒定值），见 `docs/61` §2.5 的 VCS 行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VcsConnectionsResponse {
    pub connections: Vec<VcsConnectionResponse>,
    /// 产品边界：`MULTICA_VCS_INTEGRATION_ENABLED`（自建版才开；云端关）。
    pub available: bool,
    /// 密钥存在性：`MULTICA_VCS_SECRET_KEY`（**独立**于 `available`）。
    pub configured: bool,
    /// 调用者是否 owner/admin（前端据此显示管理按钮）。
    pub can_manage: bool,
}

/// 上游 `h.vcsWebhookPath(connID)`。
pub fn webhook_path(connection_id: &str) -> String {
    format!("{VCS_WEBHOOK_PATH_PREFIX}{connection_id}")
}

/// 上游 `h.vcsWebhookURL(connID)`：base 未配置 ⇒ **空串**。
pub fn webhook_url(connection_id: &str) -> String {
    match public_base() {
        Some(base) => format!("{base}{}", webhook_path(connection_id)),
        None => String::new(),
    }
}

/// 进程级 base 覆盖（**只给测试用**；`None` = 读环境变量）。
static PUBLIC_BASE: Mutex<Option<String>> = Mutex::new(None);

/// 当前的 public base（trim 掉尾斜杠；空串/未设置 ⇒ `None`）。
pub fn public_base() -> Option<String> {
    if let Some(injected) = PUBLIC_BASE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return Some(injected);
    }
    std::env::var(PUBLIC_URL_ENV)
        .ok()
        .map(|raw| raw.trim().trim_end_matches('/').to_string())
        .filter(|raw| !raw.is_empty())
}

/// 测试注入 base（生产代码**不得**调用；与 `set_github_api_base` 同款）。
pub fn set_vcs_public_base(base: impl Into<String>) {
    let base = base.into();
    let normalized = base.trim().trim_end_matches('/').to_string();
    *PUBLIC_BASE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(normalized);
}

/// 清掉注入（测试收尾）。
pub fn reset_vcs_public_base() {
    *PUBLIC_BASE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// 本仓标准的**嵌套**错误信封 + 上游的稳定 `code`
/// （`{"error":{"code":…,"message":…}}`；形状与 `mc_errors` 一致，`code` 用上游字面量）。
///
/// 与 `routes::github::install::error_with_code` 同形 —— 本仓既有约定是「各切片各自持有
/// 本地副本」（`routes/agents.rs` 的注释逐字），所以这里再写一份而不是去改别人的冻结文件。
pub(crate) fn error_with_code(
    status: axum::http::StatusCode,
    code: &str,
    message: &str,
) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorResponse::new(code, message),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    fn row() -> VcsConnectionRow {
        VcsConnectionRow {
            id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").expect("uuid"),
            workspace_id: Uuid::parse_str("66666666-7777-8888-9999-000000000000").expect("uuid"),
            provider: "forgejo".into(),
            instance_url: "https://git.test".into(),
            account_login: "acme".into(),
            access_token_encrypted: "CIPHERTEXT-PAT-DO-NOT-LOG".into(),
            webhook_secret_encrypted: "CIPHERTEXT-SECRET-DO-NOT-LOG".into(),
            connected_by_id: None,
            created_at: Utc.timestamp_opt(0, 0).single().expect("epoch"),
            updated_at: Utc.timestamp_opt(0, 0).single().expect("epoch"),
        }
    }

    /// **序列化面**的凭据纪律：响应 JSON 里一个字节的密文都不许出现
    /// （`docs/61` §6.5 的 M8-2 行「明文入库即失败」的对外一半）。
    #[test]
    fn connection_response_never_carries_credential_columns() {
        let response = VcsConnectionResponse::from_row(&row());
        let json = serde_json::to_string(&response).expect("serialize");
        assert!(!json.contains("CIPHERTEXT"), "{json}");
        assert!(!json.contains("access_token"), "{json}");
        assert!(!json.contains("encrypted"), "{json}");
        assert!(json.contains("https://git.test"));
        assert_eq!(
            response.webhook_path,
            "/api/webhooks/vcs/11111111-2222-3333-4444-555555555555"
        );
    }

    /// `webhook_secret` 的手写 `Debug` 脱敏（它是唯一允许出现在响应里的明文凭据）。
    #[test]
    fn connect_response_debug_redacts_the_one_time_secret() {
        let response = VcsConnectResponse {
            connection: VcsConnectionResponse::from_row(&row()),
            webhook_secret: "PLAINTEXT-ONE-TIME-SECRET".into(),
        };
        let rendered = format!("{response:?}");
        assert!(
            !rendered.contains("PLAINTEXT-ONE-TIME-SECRET"),
            "{rendered}"
        );
        assert!(rendered.contains("<redacted>"));
        // JSON 里**有**它（一次性交付是契约）—— 与 Debug 的脱敏不矛盾。
        let json = serde_json::to_string(&response).expect("serialize");
        assert!(json.contains("PLAINTEXT-ONE-TIME-SECRET"));
        // 嵌入基体被展平（不是 `connection: {...}` 的嵌套）。
        let value: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(value["instance_url"], "https://git.test");
        assert!(value.get("connection").is_none());
    }

    /// `webhook_url` 的两种形态：注入 base ⇒ 拼全；无 base ⇒ 空串（`webhook_path` 仍在）。
    #[test]
    fn webhook_url_follows_public_base() {
        set_vcs_public_base("https://public.test/");
        assert_eq!(
            webhook_url("abc"),
            "https://public.test/api/webhooks/vcs/abc"
        );
        reset_vcs_public_base();

        // 清掉注入后回到 env 口径：测试进程里通常没设 ⇒ 空串。
        if std::env::var(PUBLIC_URL_ENV).is_err() {
            assert_eq!(webhook_url("abc"), "");
        }
        assert_eq!(webhook_path("abc"), "/api/webhooks/vcs/abc");
    }
}
