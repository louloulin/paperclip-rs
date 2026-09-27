//! 附件面的**元数据读**两键：`GET /api/attachments/{id}` + `GET /api/issues/{id}/attachments`
//! （**写者 M10-B1** / `LUM-2112` / `docs/64` §4.2 第 1 行）。
//!
//! 上游来源（pin `f41fae6b08fb`）：
//! - `server/internal/handler/file.go:671` `GetAttachmentByID`
//! - `server/internal/handler/file.go:642` `ListAttachments`
//!
//! # 🔴 两条键的**注册位置**与本片的两处「只加不改」（写集偏离登记，`docs/32` §50）
//!
//! | 键 | 注册在 | 原因 |
//! |---|---|---|
//! | `GET /api/attachments/{id}` | **本文件** `router()` | 新键（缺口） |
//! | `GET /api/issues/{id}/attachments` | **既有** `routes/issues/mod.rs` 的**那一行** | **占位升级**，不是新增键 |
//!
//! 第二条是本片唯一的「占位升级」：`base` 上它已经是
//! `.route("/api/issues/:id/attachments", get(not_implemented))`（`issues/mod.rs:195`），
//! 本片**只把那一个 handler 换成 [`list_issue_attachments`]**，其余行逐字只读。
//!
//! 为什么**不**把它也搬进本目录（那才是 M9-0 对 `timeline` 做的事）：
//! ① 搬走会**动到 anchor 冻结文件 `issues/mod.rs` 的删除**（本片对那个文件只有一行写权限）；
//! ② ⑦ 的 `local` 与 `implemented_placeholder` 只认「同一个 key 的 handler 换了名字」，
//! 原地换正是**占位升级**的最小位移（`local` 不变、`implemented_real +1`、
//! `implemented_placeholder 3 → 2`）。见 `docs/32` §50 的 ⑦ delta 表。
//!
//! # 形态
//!
//! 上游两条都是 plain 注册（`router.go:2149` / `2001`）⇒ **只注册无尾斜杠**那一形态。
//!
//! # `GetAttachmentByID` 为什么**总是**签发能力链接
//!
//! 上游注释逐字（`file.go:672-680`）："Always signed, regardless of what the caller
//! advertised: this endpoint is the single source of fresh, natively-loadable URLs."
//! 稳定形态的调用方（CLI `attachment download`、web 内联媒体重签钩子）在这里
//! **用一个稳定路径换一枚签名** ⇒ 若这里尊重 capability，恰好会废掉「稳定模式何以安全」
//! 的那条流。本仓照搬：`download_url` = 60 秒能力链接，`attachment_download_url` =
//! `dl=1` 的姊妹链接；**两者都只出现在这一个响应里**（列表响应永不带能力链接 ——
//! 列表的持有时间远长于 TTL，嵌进去等于发一张已过期的票）。
//!
//! 🔴 两处互为断言（`docs/64` §2.2 第 2 行）：本片用 `mc-storage` 自己的 HMAC、
//! **不引 `CloudFront`** ⇒ `/api/config` 的 `cdn_signed` 必须**恒 false**、且因
//! `omitempty` **键不出现**。`config.rs:341` 已如此实现；本片的
//! `cdn_signed_stays_absent_because_we_sign_locally`（`tests/read.rs`）钉住另一半。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use mc_repos::attachment::AttachmentRow;
use serde::Serialize;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::issues::{issue_repo, load_issue, resolve_workspace, WorkspaceQuery};
use crate::state::AppState;

use super::download::{
    attachment_download_path, capability_key, capability_path, download_capability_path,
    load_attachment_for_request,
};

// ---------------------------------------------------------------------------
// 响应形状（上游 `file.go:61-95` 的 `AttachmentResponse`）
// ---------------------------------------------------------------------------

/// `AttachmentResponse`（逐字段对齐上游；`omitempty` 的两格也照搬）。
///
/// ⚠️ `issue_id` / `comment_id` / `chat_session_id` / `chat_message_id` 在上游是
/// `*string` 且**没有** `omitempty` ⇒ 上游对 `null` 也会写出这个键。本仓用
/// `Option<String>` + `#[serde(skip_serializing_if = "Option::is_none")]` 走**另一条**路：
/// 可空列**不出现**。已登记为偏离（`docs/32` §50）—— 理由见那个文件：本仓全仓的
/// 「可空列不写键」惯例（`docs/32` 的既有偏离表）比逐字复刻四个 `null` 更一致。
#[derive(Debug, Clone, Serialize)]
pub struct AttachmentResponse {
    pub id: String,
    pub workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_message_id: Option<String>,
    pub uploader_type: String,
    pub uploader_id: String,
    pub filename: String,
    pub url: String,
    /// load 意图的下载 URL（proxy 模式下 = 60 秒能力链接）。
    pub download_url: String,
    /// download 意图（`dl=1`）的姊妹链接；**只**由单附件端点发出。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub attachment_download_url: String,
    /// 该端点**不**产出 `markdown_url` —— 见 [`markdown_url`] 的说明。
    pub markdown_url: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub created_at: String,
}

impl AttachmentResponse {
    /// 行 → 响应（**稳定形态**：`download_url` = 稳定下载路径，不签发）。
    ///
    /// `markdown_url` 的口径：上游 `buildMarkdownURL` 在 `CdnDomain() == ""`（本仓**恒**，
    /// 因为没有 CDN 概念）时**永远**返回 `publicURL + relPath` 或 `relPath`。
    /// 本仓 `ConfigSnapshot` **没有** `public_url` 字段 ⇒ 落在上游的 `publicURL == ""`
    /// 那一支 ⇒ 站点相对路径。这不是简化，是**上游在无 public URL 部署下的逐字行为**。
    #[must_use]
    pub fn stable(row: &AttachmentRow) -> Self {
        let id = row.id.to_string();
        Self {
            id: id.clone(),
            workspace_id: row.workspace_id.to_string(),
            issue_id: row.issue_id.map(|v| v.to_string()),
            comment_id: row.comment_id.map(|v| v.to_string()),
            chat_session_id: row.chat_session_id.map(|v| v.to_string()),
            chat_message_id: row.chat_message_id.map(|v| v.to_string()),
            uploader_type: row.uploader_type.clone(),
            uploader_id: row.uploader_id.to_string(),
            filename: row.filename.clone(),
            url: row.url.clone(),
            download_url: attachment_download_path(&id),
            attachment_download_url: String::new(),
            markdown_url: attachment_download_path(&id),
            content_type: row.content_type.clone(),
            size_bytes: row.size_bytes,
            created_at: format_rfc3339(row.created_at),
        }
    }
}

/// 该端点**恒**返回空串的能力链接（proxy 模式），**只有** [`signed_response`] 会填。
#[must_use]
pub fn signed_response(row: &AttachmentRow, now_unix: i64) -> AttachmentResponse {
    let id = row.id.to_string();
    let key = capability_key();
    let mut resp = AttachmentResponse::stable(row);
    // 稳定路径 → 能力链接（load 意图），以及 download 意图的姊妹链接。
    resp.download_url = capability_path(key.as_ref(), &id, now_unix);
    resp.attachment_download_url = download_capability_path(key.as_ref(), &id, now_unix);
    resp
}

/// RFC3339（本仓全仓约定，见 `docs/63` §4 第 4 条）；上游是 `2006-01-02T15:04:05Z07:00`。
fn format_rfc3339(ts: chrono::DateTime<chrono::Utc>) -> String {
    ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// router（1 条新键；第二条在 `issues/mod.rs`）
// ---------------------------------------------------------------------------

/// 本文件的 router：`GET /api/attachments/:id`（**1 条**）。
///
/// ⚠️ `GET /api/issues/:id/attachments` **不在这里** —— 它是 `issues/mod.rs` 里那一行的
/// **占位升级**（模块头第 2 条）。若两条都注册，axum **启动时 panic**（同 path+method）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/attachments/:id", get(get_attachment_by_id))
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `GET /api/attachments/{id}`（上游 `GetAttachmentByID`）。
///
/// 恒签发形态（proxy ⇒ 两枚 60 秒能力链接）；跨 workspace / 非成员 ⇒ **404**。
pub async fn get_attachment_by_id(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Response> {
    let row = load_attachment_for_request(&state, &headers, &query, &raw_id, user.id()).await?;
    let now = chrono::Utc::now().timestamp();
    Ok(axum::Json(signed_response(&row, now)).into_response())
}

/// `GET /api/issues/{id}/attachments`（上游 `ListAttachments`）—— **占位升级**的那一条。
///
/// 顺序由 `ListAttachmentsByIssue` 的 `ORDER BY created_at ASC` 承载（repo 层，不在 handler）。
/// 跨 workspace 的 issue ⇒ **404**（不是 403、也不是空列表）。
pub async fn list_issue_attachments(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_issue_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Response> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    if crate::routes::invitations::require_workspace_member(&state, workspace_id, user.id())
        .await
        .is_err()
    {
        return Err(crate::routes::invitations::not_found("issue").into());
    }
    let issue = load_issue(&issue_repo(&state), workspace_id, &raw_issue_id).await?;
    let rows = mc_repos::attachment::AttachmentRepo::new(&state.db)
        .list_by_issue(issue.id(), workspace_id)
        .await
        .map_err(|e| mc_errors::Error::Database(e.to_string()))?;
    // 🔴 **稳定形态**：`attachmentToResponse(a, attachmentURLModeStable)`。
    // 能力链接**绝不**进列表响应（TTL 60s ≪ 列表的持有时间）。
    let body: Vec<AttachmentResponse> = rows.iter().map(AttachmentResponse::stable).collect();
    Ok(axum::Json(body).into_response())
}
