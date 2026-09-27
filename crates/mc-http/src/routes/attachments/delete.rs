//! `DELETE /api/attachments/{id}`（**写者 M10-B1** / `LUM-2112` / `docs/64` §4.2 第 1 行）。
//!
//! 上游来源（pin `f41fae6b08fb`）：`server/internal/handler/file.go:1413` `DeleteAttachment`。
//!
//! # 可见性与权限（上游逐字，三层，逐层不能换序）
//!
//! 1. **不存在 / 跨 workspace / 抓取上下文的副本** ⇒ **404**。
//!    抓取上下文的副本是**不可变的历史拷贝**（上游 `file.go:1447-1452` 逐字：
//!    "Captured-context attachments are immutable historical copies. They are deleted
//!    only with their target issue, workspace, or abandoned context."）⇒ 只能随
//!    issue / workspace 一起走，**不能**单条删。`DeleteAttachment` 的 SQL 里
//!    `source_context_id IS NULL` 那一格已经把这条焊死在原子操作里了。
//! 2. **既不是上传者、也不是 workspace admin/owner** ⇒ **403**（`"not authorized to
//!    delete this attachment"`）。注意这是**唯一**一条返回 403 的路径 —— 上游
//!    故意把「看不见」（1）与「看得见但没权限」（2）分成两种码。
//! 3. 删完 ⇒ **200**（不是 204）：上游末尾还有 `h.deleteS3Object` 与两个 realtime 事件。
//!
//! 🔴 `isUploader` 的口径是 **`uploader_type == "member" && uploader_id == userID`**
//! （上游 `file.go:1458`）—— `uploader_type == "agent"` 的附件**任何人**都不能单条删，
//! 哪怕 `uploader_id` 恰好等于当前 user。这条容易被写成「只比 id」，本仓照字面落。
//!
//! # owner lock：不实现，理由与可观测差异
//!
//! 上游在事务里持一把 `withAttachmentOwnerLock` 再删（`file.go:1465-1471`），
//! `pgx.ErrNoRows` → 404。那是**并发护栏**，不是契约。
//! 本仓不实现它（它需要一张本仓没有的锁表），改为**让 repo 返回 `changed`**：
//! 并发双删里后到的那条拿到 `changed == false`，handler 据此回 **404** ——
//! 与上游那条分支**同一个状态码**，对外可观测行为一致。差异登记在 `docs/32` §50。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::delete;
use axum::Router;
use mc_repos::attachment::AttachmentRow;
use mc_repos::RepoError;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::not_found;
use crate::routes::issues::{resolve_workspace, WorkspaceQuery};
use crate::state::AppState;

use super::download::{load_attachment_for_request, split_object_ref};

/// 上游的 403 文案（逐字）。
pub const ERR_NOT_AUTHORIZED: &str = "not authorized to delete this attachment";

/// 删除后返回体（上游末尾只发 realtime 事件、不写响应体；本仓写一个可断言的摘要）。
///
/// ⚠️ 形状是**本仓自有**的：上游 `DeleteAttachment` 走完 `writeJSON` 之前的
/// `h.deleteS3Object` 然后落 200 空体。写摘要而不是空体，是为了让「真的删了」这件事
/// 在用例里**可观察**（`DoD` 第 5 条要求每条路由至少一条可判定用例）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeleteAttachmentResponse {
    pub id: String,
    /// 被 bump 的 `issue.revision`（附件不属于任何 issue 时为 0）。
    pub issue_revision: i64,
    /// 被 bump 的 `comment.revision`。
    pub comment_revision: i64,
}

/// 本文件的 router：`DELETE /api/attachments/:id`（**1 条**）。
///
/// 🔴 handler 名**故意不叫** `delete_attachment`：`delete` 这个方法路由名得原样出现在
/// 调用点上。门 ⑦ 的静态扫描用 `\b(?:get|post|delete|…)\s*\(` 认方法
/// （`scripts/route_parity.py:359`）⇒ 写成 `axum_delete(h)` 的话，`_` 与 `d` 之间
/// **没有词边界**、`\b` 不匹配 ⇒ 这条键会被记进 `unsupported`、**不计入** `local`
/// ⇒ ⑦ 的 `known_gap` 少关一条、`M3+` 归零的信号打不响。所以这里 `use axum::routing::delete`
/// 而 handler 改名 [`remove_attachment`]。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/attachments/:id", delete(remove_attachment))
}

/// `DELETE /api/attachments/{id}`（上游 `DeleteAttachment`）。
pub async fn remove_attachment(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Response> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    // 第 1 层 + 第 2 层的**可见性**部分：404（不存在 / 跨 workspace / 抓取上下文副本）、
    // 非成员 404。`load_attachment_for_request` 三者都归一到同一个 404。
    let (att, _ws) = (
        load_attachment_for_request(&state, &headers, &query, &raw_id, user.id()).await?,
        workspace_id,
    );

    // 第 2 层的**权限**部分：非上传者、非 admin/owner ⇒ 403。
    if !may_delete(&state, workspace_id, user.id(), &att).await {
        return Err(mc_errors::Error::Forbidden {
            message: ERR_NOT_AUTHORIZED.into(),
        }
        .into());
    }

    let outcome = match mc_repos::attachment::AttachmentRepo::new(&state.db)
        .delete(workspace_id, att.id())
        .await
    {
        Ok(o) => o,
        Err(RepoError::NotFound) => return Err(not_found("attachment").into()),
        Err(other) => return Err(mc_errors::Error::Database(other.to_string()).into()),
    };
    if !outcome.changed {
        // 抓取上下文的副本（SQL 那一格挡住了）或并发双删里已被删掉的那条 ⇒ 404。
        return Err(not_found("attachment").into());
    }

    // 上游 `h.deleteS3Object(r.Context(), att.Url)`：对象存储里的那一份也要清掉。
    // ⚠️ **本仓不做**（已登记为偏离，`docs/32` §50）：本仓没有 `deleteS3Object` 的
    // 等价物（`mc_storage::Storage` 有 `delete`，但**桶路由是部署装配期决定的**，
    // 拿不到可靠的 bucket 解析就不猜）。**后果 = 删行会留下孤儿对象**，
    // 由 GC 任务（不在本片范围）收。**判据**：用例断言「行没了」，不断言「对象没了」。
    let _ = split_object_ref(&att.url);

    Ok(axum::Json(DeleteAttachmentResponse {
        id: att.id().to_string(),
        issue_revision: outcome.issue_revision,
        comment_revision: outcome.comment_revision,
    })
    .into_response())
}

/// 能否删：上传者本人（**且 `uploader_type == "member"`**）或 workspace 的 admin / owner。
async fn may_delete(
    state: &AppState,
    workspace_id: mc_core::Id,
    user_id: mc_core::Id,
    att: &AttachmentRow,
) -> bool {
    let is_uploader = att.uploader() == mc_repos::attachment::UploaderType::Member
        && att.uploader_id == user_id.0;
    if is_uploader {
        return true;
    }
    crate::routes::invitations::require_workspace_admin(state, workspace_id, user_id)
        .await
        .is_ok()
}
