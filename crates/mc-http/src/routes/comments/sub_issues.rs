//! `GET /api/comments/{commentId}/sub-issue-preview` —— **人类专属的抓取上下文预览**
//! （写者 M10-B4 / `LUM-2115` / `docs/64` §4.2 第 3 行）。
//!
//! 上游来源（pin `f41fae6b08fb`）：
//! `server/cmd/server/router.go:2160`（`r.With(handler.RequireHumanActor).Get("/sub-issue-preview", …)`，
//! 在 `Route("/api/comments/{commentId}")` 里）→ `server/internal/handler/source_context.go:406`
//! `PreviewCommentSubIssue` → `server/internal/service/source_context.go:404` `BuildSourceContext`。
//!
//! ## 这条路由在整条链上的位置
//!
//! 「从一条评论派生一个子 issue」是两段式：先 **preview**（本文件，纯读，产出
//! `capture_token`），再 **create**（`POST …/sub-issues`，**本仓仍 501**，见
//! [`super::create_comment_sub_issue`]）。preview 的产物就是 create 的输入契约：
//! 快照 + 一个把快照钉死的摘要 token。
//!
//! ## 三层判据，顺序逐字照上游
//!
//! 1. **人类闸**（[`HumanActor`] 提取器）：`X-Actor-Source ∈ {task_token, cloud_pat}` ⇒
//!    **403**。上游是 `r.With(handler.RequireHumanActor)` 这一层路由组中间件；本仓用
//!    handler 级提取器（[`require_human_actor`](crate::actor_guard::require_human_actor)
//!    的 handler 级对偶），因为本文件是**合并进** comments 子 router 的一个 `.route()`，
//    在子 router 上加 `route_layer` 会波及同一 router 的其它键。
//! 2. **成员门**：非该工作区成员 ⇒ **404**（本仓跨租户探测一律 404，见
//!    `comments/mod.rs` 模块头「避免跨租户探测评论是否存在」；上游 `workspaceMember`
//!    是 403，已登记 `docs/32` §9.23）。
//! 3. **快照自身的三类失败**（上游 `writeSourceContextError`）：
//!    - 锚评论已删 / 源 issue 已删 / 线程形状非法 ⇒ **409**（三个不同的 `code`）；
//!    - 超限（评论条数 / 正文字节 / 附件条数 / 附件字节）⇒ **422** `source_context_too_large`
//!      **且带 `limits`**（这是唯一带 `limits` 的分支）；
//!    - 其余 ⇒ **500** `source_context_capture_failed`。
//!
//! ## 摘要（digest）到底盖住了什么（上游 `sourceContextDigest`）
//!
//! **不是**「源内容变了」那么简单，它**故意**排除三类噪声：
//! - 采集元数据（`version` / `captured_by_user_id` / `captured_at`）；
//! - issue 的 `updated_at` / `revision`；
//! - 每条评论的 `author.name`（**显示名是活的身份元数据，不是源内容** —— 预览与创建
//!   之间的一次改名**不该**让一个逐字相同的采集失效；改名由 detail 渲染独立汇报）、
//!   `updated_at` / `revision`（编辑后逐字改回原文必须重新匹配预览 token）。
//!
//! ## 偏离（已登记 `docs/32` §9.23）
//!
//! - **时间戳字面量**：上游是 Go 的 `RFC3339Nano`（去掉尾部零的分数秒），本仓用
//!   chrono 的 `SecondsFormat::AutoSi`。两者对同一时刻可能**不是同一串字节**
//!   （`.729032000` vs `.729032`）⇒ 摘要的输入字节不同，但摘要**只在本仓内自洽**
//!   （消费它的 `POST …/sub-issues` 本仓仍是 501）。
//! - 上游 `anchor.Type != "comment"` ⇒ 409 `source_context_invalid_path`。本仓的
//!   `CommentRow` 不选 `type` 列（见 `comments/mod.rs` 的偏差清单），本文件**自己**
//!   查这一列并照上游判。
//!
//! ## 形态
//!
//! 上游是 `Route(...)` 里的**子路由** `Get("/sub-issue-preview", …)`（不是 `Get("/")`）
//! ⇒ **只注册无尾斜杠形态**。与同一 `Route` 里的 `sub-issues` / `resolve` /
//! `reactions` 同款（`docs/37` §15.1）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use mc_core::Id;
use mc_errors::Error;

use crate::actor_guard::HumanActor;
use crate::error::ApiError;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 上游 `SourceContextMaxComments`。
pub const MAX_COMMENTS: i64 = 256;
/// 上游 `SourceContextMaxTextBytes`。
pub const MAX_TEXT_BYTES: usize = 1 << 20;
/// 上游 `SourceContextMaxAttachments`。
pub const MAX_ATTACHMENTS: usize = 100;
/// 上游 `SourceContextMaxAttachmentBytes`。
pub const MAX_ATTACHMENT_BYTES: i64 = 500 << 20;

/// 本文件 router：**1 条**注册键（合并进 [`super::router`]，不自带 `Router::new`）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/api/comments/:commentId/sub-issue-preview",
        get(preview_sub_issue),
    )
}

// ---------------------------------------------------------------------------
// 响应形状（上游 `sourceContextPreviewResponse` + `service` 的快照类型，逐字）
// ---------------------------------------------------------------------------

/// 上游 `SourceContextLimitUsage`。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LimitUsage {
    pub comment_count: usize,
    pub text_bytes: usize,
    pub attachment_count: usize,
    pub attachment_bytes: i64,
}

/// 上游 `SourceContextAttachment`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentSnapshot {
    pub id: String,
    pub owner_type: String,
    pub owner_id: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub created_at: String,
}

/// 上游 `SourceContextAuthor`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorSnapshot {
    #[serde(rename = "type")]
    pub author_type: String,
    pub id: String,
    pub name: String,
}

/// 上游 `SourceContextIssueSnapshot`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueSnapshot {
    pub id: String,
    pub identifier: String,
    pub number: i32,
    pub title: String,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
    pub attachments: Vec<AttachmentSnapshot>,
}

/// 上游 `SourceContextCommentSnapshot`（`deleted` 是 `omitempty` 的墓碑标记）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentSnapshot {
    pub id: String,
    pub parent_id: Option<String>,
    #[serde(rename = "type")]
    pub comment_type: String,
    pub content: String,
    pub author: AuthorSnapshot,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
    pub attachments: Vec<AttachmentSnapshot>,
    /// 墓碑：回复还在、自身已软删的评论只为让回复保住 `parent_id` 而留在快照里，
    /// 它的 `content` 是空的。`false` 时**整个键缺席**（`omitempty`）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
}

/// `serde` 的 `skip_serializing_if` 只能接 `fn(&字段类型) -> bool`（**引用**是硬要求），
/// 而本仓开 `clippy::pedantic`（含 `trivially_copy_pass_by_ref` / `ptr_arg`）⇒ 这三个
/// 小判定函数逐个豁免，形状与上游的 `omitempty` 一一对应。
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(v: &bool) -> bool {
    !*v
}

/// 上游 `SourceContextSnapshot`。
///
/// `version` / `captured_by_user_id` / `captured_at` 是**采集时**才填的三个键，
/// 预览这一段恒为空 ⇒ 照上游的 `omitempty` 逐字缺席（这同时让
/// [`snapshot_digest`] 的规范化投影不必真的去改这三处）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    #[serde(default, skip_serializing_if = "is_zero_i16")]
    pub version: i16,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub captured_by_user_id: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub captured_at: String,
    pub source_issue: IssueSnapshot,
    pub comment_thread: Vec<CommentSnapshot>,
    pub anchor_comment_id: String,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero_i16(v: &i16) -> bool {
    *v == 0
}

#[allow(clippy::ptr_arg)] // `serde` 传进来的是 `&String`（见上面 `is_false` 的理由）。
const fn is_empty_str(v: &String) -> bool {
    v.is_empty()
}

/// 上游 `sourceContextPreviewResponse`。
#[derive(Debug, Clone, Serialize)]
pub struct PreviewResponse {
    pub source_issue: IssueSnapshot,
    pub comment_thread: Vec<CommentSnapshot>,
    pub anchor_comment_id: String,
    pub capture_token: String,
    pub limits: LimitUsage,
}

// ---------------------------------------------------------------------------
// 错误面（上游 `writeSourceContextError` 的 code 词表）
// ---------------------------------------------------------------------------

/// 预览构建的失败（每一支都带上游的 `code` 与状态码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewError {
    /// **404**：非法 id / 评论不存在 / 非工作区成员。
    ///
    /// 不是一个上游 `code`：上游这一层是 `parseUUIDOrBadRequest`（400）与
    /// `workspaceMember`（403），本仓把它们**合并成 404**（与 `comments/mod.rs`
    /// 的跨租户口径一致，不泄露评论是否存在）。已登记 `docs/32` §9.23。
    Missing,
    /// 409 `anchor_comment_deleted`
    AnchorCommentDeleted,
    /// 409 `source_issue_deleted`
    SourceIssueDeleted,
    /// 409 `source_context_invalid_path`
    InvalidPath,
    /// 422 `source_context_too_large`（**唯一**带 `limits` 的分支）
    TooLarge(LimitUsage),
    /// 500 `source_context_capture_failed`
    CaptureFailed,
}

impl PreviewError {
    fn status(&self) -> StatusCode {
        match self {
            Self::Missing => StatusCode::NOT_FOUND,
            Self::TooLarge(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::CaptureFailed => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::CONFLICT,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Missing => "not_found",
            Self::AnchorCommentDeleted => "anchor_comment_deleted",
            Self::SourceIssueDeleted => "source_issue_deleted",
            Self::InvalidPath => "source_context_invalid_path",
            Self::TooLarge(_) => "source_context_too_large",
            Self::CaptureFailed => "source_context_capture_failed",
        }
    }

    fn message(&self) -> &'static str {
        match self {
            Self::Missing => "comment not found",
            Self::AnchorCommentDeleted => "anchor comment deleted",
            Self::SourceIssueDeleted => "source issue deleted",
            Self::InvalidPath => "source context invalid comment thread",
            Self::TooLarge(_) => "source context too large",
            Self::CaptureFailed => "failed to capture source context",
        }
    }

    /// 上游的 `{"code": …, "error": …}` 载荷（`too_large` 时多一个 `limits`）。
    pub fn into_response(self) -> Response {
        if matches!(self, Self::Missing) {
            return ApiError(Error::NotFound {
                resource: "comment".into(),
            })
            .respond_with(StatusCode::NOT_FOUND);
        }
        let mut body = serde_json::Map::new();
        body.insert("code".into(), self.code().into());
        body.insert("error".into(), self.message().into());
        if let Self::TooLarge(limits) = &self {
            body.insert(
                "limits".into(),
                serde_json::to_value(limits).unwrap_or(serde_json::Value::Null),
            );
        }
        (self.status(), axum::Json(serde_json::Value::Object(body))).into_response()
    }
}

mod snapshot;

use self::snapshot::build_snapshot;

// ---------------------------------------------------------------------------
// GET /api/comments/{commentId}/sub-issue-preview
// ---------------------------------------------------------------------------

/// 上游 `PreviewCommentSubIssue`。
///
/// `HumanActor` 提取器就是上游 `r.With(handler.RequireHumanActor)` 的本仓形态
/// （[`HumanActor`] 未通过时其 rejection 已是 403，body 是全仓统一错误体）。
pub async fn preview_sub_issue(
    State(state): State<Arc<AppState>>,
    _human: HumanActor,
    user: AuthUser,
    Path(comment_id): Path<String>,
) -> Response {
    match build_preview(&state, user.id(), &comment_id).await {
        Ok(resp) => (StatusCode::OK, axum::Json(resp)).into_response(),
        Err(err) => err.into_response(),
    }
}

/// 预览的骨架：**解析 id → 锚评论 → 成员门 → 快照 → 摘要 → token**。
async fn build_preview(
    state: &AppState,
    user_id: Id,
    raw_id: &str,
) -> Result<PreviewResponse, PreviewError> {
    let comment_id = Id::parse(raw_id).map_err(|_| missing())?;
    let repo = mc_repos::comment::CommentRepo::new(&state.db);
    // 跨租户读一律 NotFound（`CommentRepo::get` 不带 workspace 条件 ⇒ 这里先取行
    // 拿 workspace，再做成员门；两者顺序与上游 `resolveWorkspaceID` +
    // `workspaceMember` 一致）。
    let anchor = repo.get(comment_id).await.map_err(|_| missing())?;
    if !is_member(state, anchor.workspace_id(), user_id).await {
        return Err(missing());
    }
    let snapshot = build_snapshot(state, comment_id).await?;
    let digest = snapshot_digest(&snapshot)?;
    let limits = limit_usage(&snapshot);
    Ok(PreviewResponse {
        source_issue: snapshot.source_issue.clone(),
        comment_thread: snapshot.comment_thread.clone(),
        anchor_comment_id: snapshot.anchor_comment_id.clone(),
        // 上游 `Token` = `"sha256:" + digest + ":" + <源 issue 的 uuid>`。
        capture_token: capture_token(&digest, &snapshot.source_issue.id),
        limits,
    })
}

// ---------------------------------------------------------------------------
// 摘要 / 用量（纯函数，逐字照上游）
// ---------------------------------------------------------------------------

/// 上游 `sourceContextDigest`：**规范化**投影之后取 SHA-256 的十六进制。
///
/// 规范化排除的字段见模块头（采集元数据 + issue 的 `updated_at`/`revision` +
/// 每条评论的 `author.name`/`updated_at`/`revision`）。
/// `attachments` **不**被排除 —— 附件集合变了就是源内容变了。
///
/// 投影方式是「克隆后清零」而不是「就地改」：[`Snapshot`] 的克隆与上游
/// `append([]T(nil), …)` 同款，避免把采集元数据从**将被持久化的**快照里抹掉。
pub fn snapshot_digest(snapshot: &Snapshot) -> Result<String, PreviewError> {
    let mut canonical = snapshot.clone();
    // 采集元数据：上游逐字清零这三处（清零后它们被 `omitempty` 抹掉，等价于「不参与」）。
    canonical.version = 0;
    canonical.captured_by_user_id = String::new();
    canonical.captured_at = String::new();
    canonical.source_issue.updated_at = String::new();
    canonical.source_issue.revision = 0;
    for comment in &mut canonical.comment_thread {
        comment.author.name = String::new();
        comment.updated_at = String::new();
        comment.revision = 0;
    }
    let payload = serde_json::to_vec(&canonical).map_err(|_| PreviewError::CaptureFailed)?;
    Ok(hex(&Sha256::digest(payload)))
}

/// 上游的 `Limits` 计算：正文字节 = **整份快照**的 JSON 长度。
pub fn limit_usage(snapshot: &Snapshot) -> LimitUsage {
    let mut usage = LimitUsage {
        comment_count: snapshot.comment_thread.len(),
        text_bytes: serde_json::to_vec(snapshot).map_or(0, |v| v.len()),
        attachment_count: snapshot.source_issue.attachments.len(),
        attachment_bytes: snapshot
            .source_issue
            .attachments
            .iter()
            .map(|a| a.size_bytes)
            .sum(),
    };
    for comment in &snapshot.comment_thread {
        usage.attachment_count += comment.attachments.len();
        usage.attachment_bytes += comment
            .attachments
            .iter()
            .map(|a| a.size_bytes)
            .sum::<i64>();
    }
    usage
}

/// 上游 `ParseSourceContextToken` 的逆（预览侧只需要「铸」）。
#[must_use]
pub fn capture_token(digest: &str, source_issue_id: &str) -> String {
    format!("sha256:{digest}:{source_issue_id}")
}

// ---------------------------------------------------------------------------
// 成员门
// ---------------------------------------------------------------------------

/// 成员门（上游 `h.workspaceMember(w, r, workspaceID)`）：本仓跨租户探测一律 404。
async fn is_member(state: &AppState, workspace_id: Id, user_id: Id) -> bool {
    mc_repos::member::MemberRepo::new(state.db.clone())
        .get_for_user(workspace_id, user_id)
        .await
        .is_ok()
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// 上游 `sourceContextTime`：UTC 的 RFC3339（本仓用 `AutoSi`，见模块头的偏离登记）。
fn rfc3339(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// 404：非法 id、评论不存在、跨租户 —— **同一形状**（不泄露存在性）。
fn missing() -> PreviewError {
    PreviewError::Missing
}

// 本片的证据面（门 ⑤ 零库）。
#[cfg(test)]
#[path = "sub_issues/tests.rs"]
mod tests;
