//! `/api/issues/{id}/comments` 与 `/api/comments/{commentId}*` 系列路由（M2-B / LUM-1350）。
//!
//! 对应 multica server `internal/handler/comment.go` + `reaction.go`。
//!
//! 路由表（与上游 1:1，除声明的偏差外）：
//!
//! | method | path | 上游 handler |
//! |---|---|---|
//! | GET/POST | `/api/issues/{id}/comments` | `ListComments` / `CreateComment` |
//! | PUT/DELETE | `/api/comments/{commentId}` | `UpdateComment` / `DeleteComment` |
//! | PUT/DELETE | `/api/comments/{commentId}/` | 同上（chi `Mount` 的第二种形态，LUM-1458 补）|
//! | DELETE | `/api/comments/{commentId}/keep-replies` | `DeleteComment`（兼容路径） |
//! | POST/DELETE | `/api/comments/{commentId}/resolve` | `ResolveComment` / `UnresolveComment` |
//! | POST/DELETE | `/api/comments/{commentId}/reactions` | `AddReaction` / `RemoveReaction` |
//! | POST | `/api/comments/{commentId}/sub-issues` | `CreateCommentSubIssue`（M2-B 返回 501） |
//!
//! **尾斜杠双形态（LUM-1458）**：上面第 2 行的两个形态必须**同时**注册 —— 上游
//! `router.go` 是 `Route("/api/comments/{commentId}") + Put("/")/Delete("/")`，chi 的
//! `Mount` 两种形态都服务；axum 0.7 不做归一化 ⇒ 少注册一个就是 404（不是 307）。
//! 其余 `keep-replies` / `resolve` / `reactions` / `sub-issues` 是 plain 子路由，
//! 上游只有**一个**形态，不要加别名（`docs/37` §15.1、本片记录见其 §21）。
//!
//! 文件布局（gate ⑩ 单文件 800 行硬上限）：DTO / 请求体在 `comments/dto.rs`，
//! 本文件只留路由表 + handler。**不要为了省行数让两个 `.route()` 共用一个
//! `MethodRouter` 变量** —— ⑦ 的抽取器会把那个键当成没注册（`docs/37` §15.6 实测）。
//!
//! 鉴权（M2 阶段，与 M1 一致的 dev-mode 简化）：
//! - `X-Multica-User-Id` header 提供当前用户（`AuthUser`）
//! - workspace 归属**从评论 / issue 行反查**（M1 的 dev-mode 没有 workspace header）
//! - 非 workspace 成员一律 404（而不是 403），避免跨租户探测评论是否存在
//! - 评论编辑 / 删除：作者或 admin，否则 403（对齐上游 `only comment author or admin can edit|delete`）
//!
//! 注意（axum 0.7 / matchit 0.7）：路径参数必须写 `:commentId` / `:id`，
//! `{commentId}` 会被当字面量段——编译通过但恒 404（docs/09 §7.4）。
//!
//! 偏差与简化（完整清单见 `docs/12-M2-COMMENT.md`）：
//! - `sub-issues` 依赖 source-context token 基础设施（`mc-source-context` crate，本仓尚未建立）→ 501
//!   （M2-A 已并入，`IssueRepo` 可用；集成仲裁见 `docs/12-M2-COMMENT.md` §6.1）
//! - `recent` / `tail` / `summary` / `fold` 读模式未实现 → 400（明确报错，不静默降级）
//! - 单线程唯一 resolution（上游 `ClearOtherThreadResolutions`）未实现 → TODO
//! - `comment.type` / `resolved_by_*` / `quick_action_id` 列在本仓库 0001 里不存在 → 响应省略
//! - 附件（M3）、@agent 触发、realtime 广播、inbox 通知不在本切片

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};

use mc_core::comment::CommentAuthorType;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::comment::{CommentFilter, CommentRepo, CommentRow, NewComment};
use mc_repos::RepoError;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::not_found;
use crate::state::AppState;

mod dto;

use self::dto::ts;
pub use self::dto::{
    CommentDto, CreateCommentRequest, NotImplementedBody, ReactionDto, ReactionRequest,
    UpdateCommentRequest,
};

/// `GET /api/issues/:id/comments` 的 next-cursor 响应头（对齐上游）。
pub const NEXT_BEFORE_HEADER: &str = "x-multica-next-before";
/// 同上，游标 id 部分。
pub const NEXT_BEFORE_ID_HEADER: &str = "x-multica-next-before-id";

/// 本切片已实现、但上游存在而此处**未**实现的读模式（显式 400，不静默降级）。
const UNSUPPORTED_QUERY_PARAMS: &[&str] = &["recent", "tail", "summary", "fold"];

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/issues/:id/comments",
            get(list_comments).post(create_comment),
        )
        .route(
            "/api/comments/:commentId",
            put(update_comment).delete(delete_comment),
        )
        // 上游 `router.go` 是 `Route("/api/comments/{commentId}") + Put("/")/Delete("/")`
        // ⇒ chi `Mount` 同时服务带/不带尾斜杠两种形态（docs/37 §15.1）；axum 少注册
        // 一个就是 404（不是 307）。两个形态的方法集合逐字相同。
        .route(
            "/api/comments/:commentId/",
            put(update_comment).delete(delete_comment),
        )
        // 上游把 `DELETE /` 与 `DELETE /keep-replies` 交给同一个 handler
        // （router.go L2169 "Same handler under a path servers from before #8296
        // do not route"）。本切片同样两者等价：只软删评论自身，回复保留。
        .route(
            "/api/comments/:commentId/keep-replies",
            delete(delete_comment),
        )
        .route(
            "/api/comments/:commentId/resolve",
            post(resolve_comment).delete(unresolve_comment),
        )
        .route(
            "/api/comments/:commentId/reactions",
            post(add_reaction).delete(remove_reaction),
        )
        .route(
            "/api/comments/:commentId/sub-issues",
            post(create_comment_sub_issue),
        )
}

// ---------------------------------------------------------------------------
// DTO：见 `comments/dto.rs`（LUM-1458 拆出，⑩ 的 831 行基线）
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 查询参数
// ---------------------------------------------------------------------------

/// `GET /api/issues/{id}/comments` 解析后的查询条件。
#[derive(Debug, Default)]
struct ListQuery {
    limit: Option<u32>,
    since: Option<DateTime<Utc>>,
    before: Option<DateTime<Utc>>,
    before_id: Option<Id>,
    roots_only: bool,
    thread: Option<Id>,
}

fn bad_request(message: impl Into<String>) -> Error {
    Error::Validation {
        message: message.into(),
        details: vec![],
    }
}

/// 严格布尔解析：只有 `true` / `false` 合法（对齐上游 `invalid roots_only parameter`）。
fn parse_strict_bool(raw: &str, field: &str) -> Result<bool, Error> {
    match raw {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(bad_request(format!(
            "invalid {field} parameter; expected boolean"
        ))),
    }
}

fn parse_rfc3339(raw: &str, field: &str) -> Result<DateTime<Utc>, Error> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| {
            bad_request(format!(
                "invalid {field} parameter; expected RFC3339 format"
            ))
        })
}

fn parse_id_param(raw: &str, field: &str) -> Result<Id, Error> {
    Id::parse(raw).map_err(|_| bad_request(format!("invalid {field}: not a uuid")))
}

/// 解析并校验列表查询参数。
///
/// 上游已实现但本切片未实现的读模式（`recent` / `tail` / `summary` / `fold`）
/// 一律 400 —— 静默忽略会让调用方以为拿到了"完整线程 / 折叠投影"。
fn parse_list_query(raw: &HashMap<String, String>) -> Result<ListQuery, Error> {
    for key in UNSUPPORTED_QUERY_PARAMS {
        if raw.contains_key(*key) {
            return Err(bad_request(format!(
                "query parameter {key} is not implemented yet (M2-B slice)"
            )));
        }
    }

    let mut q = ListQuery::default();

    if let Some(limit) = raw.get("limit") {
        q.limit = Some(
            limit
                .parse::<u32>()
                .map_err(|_| bad_request("invalid limit parameter; expected integer"))?,
        );
    }
    if let Some(since) = raw.get("since") {
        q.since = Some(parse_rfc3339(since, "since")?);
    }
    if let Some(before) = raw.get("before") {
        q.before = Some(parse_rfc3339(before, "before")?);
    }
    // 上游接受 CLI 风格的连字符别名（`before-id` / `roots-only`）。
    if let Some(before_id) = raw.get("before_id").or_else(|| raw.get("before-id")) {
        q.before_id = Some(parse_id_param(before_id, "before_id")?);
    }
    if let Some(roots_only) = raw.get("roots_only").or_else(|| raw.get("roots-only")) {
        q.roots_only = parse_strict_bool(roots_only, "roots_only")?;
    }
    if let Some(thread) = raw.get("thread") {
        q.thread = Some(parse_id_param(thread, "thread")?);
    }
    // `before` 与 `before_id` 是同一个游标的两半，缺一不可（上游同样成对使用）。
    if (q.before.is_some()) != (q.before_id.is_some()) {
        return Err(bad_request(
            "before and before_id must be provided together",
        ));
    }
    Ok(q)
}

// ---------------------------------------------------------------------------
// 共享前置检查
// ---------------------------------------------------------------------------

/// 评论所属 workspace 的成员角色；非成员返回 `None`（调用方决定 404 / 403）。
async fn workspace_role(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> Result<Option<String>, Error> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.0)
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
    Ok(role)
}

/// issue 属于哪个 workspace（不存在 → 404）。
async fn issue_workspace_id(state: &AppState, issue_id: Id) -> Result<Id, Error> {
    let ws: Option<uuid::Uuid> = sqlx::query_scalar("SELECT workspace_id FROM issue WHERE id = $1")
        .bind(issue_id.0)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|e| Error::Database(e.to_string()))?;
    ws.map(Id).ok_or_else(|| not_found("issue"))
}

/// 加载评论 + 校验调用者是该 workspace 成员。
///
/// 非成员 / 不存在 / tombstone 一律 404（不泄露跨租户存在性；tombstone 不可变更）。
async fn load_live_comment_for_member(
    state: &AppState,
    repo: &CommentRepo,
    comment_id: Id,
    user_id: Id,
) -> Result<(CommentRow, String), Error> {
    let row = repo.get(comment_id).await.map_err(comment_err)?;
    if row.is_deleted() {
        return Err(not_found("comment"));
    }
    let ws = row.workspace_id();
    let role = workspace_role(state, ws, user_id).await?;
    match role {
        Some(role) => Ok((row, role)),
        None => Err(not_found("comment")),
    }
}

fn is_admin(role: &str) -> bool {
    matches!(role, "owner" | "admin")
}

/// 作者或 admin 才能改动（对齐上游 403 语义）。
fn ensure_author_or_admin(
    row: &CommentRow,
    role: &str,
    user_id: Id,
    action: &str,
) -> Result<(), Error> {
    let is_author =
        row.author_type() == CommentAuthorType::User && row.author_id == user_id.to_string();
    if is_author || is_admin(role) {
        return Ok(());
    }
    Err(Error::Forbidden {
        message: format!("only comment author or admin can {action}"),
    })
}

fn comment_err(e: RepoError) -> Error {
    match e {
        RepoError::NotFound => not_found("comment"),
        RepoError::Conflict => Error::Conflict {
            message: "comment revision conflict".into(),
        },
        RepoError::Db(e) => Error::Database(e.clone()),
    }
}

/// 校验 content：剥离 PG TEXT 拒绝的 NUL 字节，再要求非空（对齐上游 `sanitizeNullBytes`）。
fn sanitize_and_require_content(raw: &str) -> Result<String, Error> {
    let content: String = raw.chars().filter(|c| *c != '\0').collect();
    if content.is_empty() {
        return Err(bad_request("content is required"));
    }
    Ok(content)
}

/// 校验可写入的评论类型。
///
/// 上游允许 `comment` / `progress_update`；本仓库 0001 的 `comment` 表没有 `type` 列，
/// 所以只接受默认 `comment` —— `progress_update` 被明确 400（而不是静默丢类型）。
fn ensure_comment_type(kind: Option<&str>) -> Result<(), Error> {
    match kind.unwrap_or("comment") {
        "comment" => Ok(()),
        "progress_update" => Err(bad_request(
            "comment type progress_update is not supported yet (comment.type column TODO)",
        )),
        _ => Err(bad_request("invalid comment type")),
    }
}

// ---------------------------------------------------------------------------
// Handlers：issue 下的评论
// ---------------------------------------------------------------------------

/// GET /api/issues/{id}/comments
async fn list_comments(
    State(state): State<Arc<AppState>>,
    Path(issue_id): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
    user: AuthUser,
) -> ApiResult<Response> {
    let issue_id = parse_id_param(&issue_id, "issue id")?;
    let query = parse_list_query(&raw)?;

    let workspace_id = issue_workspace_id(&state, issue_id).await?;
    // 非成员 404（不泄露 issue 存在性）。
    if workspace_role(&state, workspace_id, user.id())
        .await?
        .is_none()
    {
        return Err(not_found("issue").into());
    }

    let repo = CommentRepo::new(&state.db);
    let mut filter = CommentFilter::for_issue(issue_id);
    filter.roots_only = query.roots_only;
    filter.thread = query.thread;
    filter.since = query.since;
    if let (Some(before), Some(before_id)) = (query.before, query.before_id) {
        filter.before = Some(mc_repos::comment::CommentCursor {
            created_at: before,
            id: before_id,
        });
    }
    if let Some(limit) = query.limit {
        filter.limit = limit;
    }

    let list = repo.list_for_issue(filter).await.map_err(comment_err)?;

    let ids: Vec<Id> = list.comments.iter().map(CommentRow::id).collect();
    let mut grouped: HashMap<String, Vec<ReactionDto>> = HashMap::new();
    for row in repo.list_reactions(&ids).await.map_err(comment_err)? {
        grouped
            .entry(row.comment_id().to_string())
            .or_default()
            .push(ReactionDto::from(&row));
    }

    let mut headers = HeaderMap::new();
    // 窗口外还有更早的根评论 → 回传下一页游标（上游放在响应头里，保持默认 body 形状）。
    if list.has_more {
        if let Some(oldest_root) = list
            .comments
            .iter()
            .filter(|c| c.is_root())
            .min_by_key(|c| (c.created_at, c.id))
        {
            if let Ok(v) = ts(&oldest_root.created_at).parse() {
                headers.insert(NEXT_BEFORE_HEADER, v);
            }
            if let Ok(v) = oldest_root.id().to_string().parse() {
                headers.insert(NEXT_BEFORE_ID_HEADER, v);
            }
        }
    }

    let body: Vec<CommentDto> = list
        .comments
        .iter()
        .map(|row| {
            let reactions = grouped.remove(&row.id().to_string()).unwrap_or_default();
            CommentDto::new(row, reactions)
        })
        .collect();

    Ok((headers, Json(body)).into_response())
}

/// POST /api/issues/{id}/comments
async fn create_comment(
    State(state): State<Arc<AppState>>,
    Path(issue_id): Path<String>,
    user: AuthUser,
    Json(req): Json<CreateCommentRequest>,
) -> ApiResult<(StatusCode, Json<CommentDto>)> {
    let issue_id = parse_id_param(&issue_id, "issue id")?;
    let content = sanitize_and_require_content(&req.content)?;
    ensure_comment_type(req.kind.as_deref())?;

    let workspace_id = issue_workspace_id(&state, issue_id).await?;
    if workspace_role(&state, workspace_id, user.id())
        .await?
        .is_none()
    {
        return Err(not_found("issue").into());
    }

    let parent_id = match req.parent_id.as_deref() {
        Some(raw) if !raw.is_empty() => Some(parse_id_param(raw, "parent_id")?),
        _ => None,
    };

    let repo = CommentRepo::new(&state.db);
    let input = NewComment {
        workspace_id,
        issue_id,
        parent_id,
        // dev-mode：actor 恒为当前 user（上游的 X-Agent-ID agent 路径未实现）。
        author_type: CommentAuthorType::User,
        author_id: user.id().to_string(),
        body: content,
        // 上游从 `X-Task-ID` 取；本切片不接受该 header。
        source_task_id: None,
    };
    let row = repo.create(input).await.map_err(|e| match e {
        RepoError::NotFound => {
            // issue 已校验存在，唯一可能是 parent 不属于该 issue / 已被软删。
            bad_request("invalid parent comment")
        }
        other => comment_err(other),
    })?;

    Ok((StatusCode::CREATED, Json(CommentDto::bare(&row))))
}

// ---------------------------------------------------------------------------
// Handlers：单条评论
// ---------------------------------------------------------------------------

/// PUT /api/comments/{commentId}
async fn update_comment(
    State(state): State<Arc<AppState>>,
    Path(comment_id): Path<String>,
    user: AuthUser,
    Json(req): Json<UpdateCommentRequest>,
) -> ApiResult<Json<CommentDto>> {
    let comment_id = parse_id_param(&comment_id, "comment id")?;
    let content = sanitize_and_require_content(&req.content)?;
    if let Some(expected) = req.expected_revision {
        if expected < 1 {
            return Err(bad_request("expected_revision must be a positive integer").into());
        }
    }

    let repo = CommentRepo::new(&state.db);
    let (row, role) = load_live_comment_for_member(&state, &repo, comment_id, user.id()).await?;
    ensure_author_or_admin(&row, &role, user.id(), "edit")?;

    let updated = repo
        .update(
            comment_id,
            mc_repos::comment::CommentPatch {
                body: content,
                expected_revision: req.expected_revision,
            },
        )
        .await
        .map_err(|e| match e {
            RepoError::Conflict => Error::Conflict {
                message: "comment was modified by someone else (expected_revision mismatch)".into(),
            },
            other => comment_err(other),
        })?;

    let reactions = repo
        .list_reactions(&[updated.id()])
        .await
        .map_err(comment_err)?
        .iter()
        .map(ReactionDto::from)
        .collect();
    Ok(Json(CommentDto::new(&updated, reactions)))
}

/// DELETE /api/comments/{commentId}（= `/keep-replies`）
///
/// 只软删评论自身（tombstone），回复保留 —— 对齐上游 `DeleteComment`：
/// 有回复时留 tombstone，无回复时上游物理删除、本切片留不可见的 tombstone。
/// 上游**从不**连带删除回复，所以这里也不级联（`soft_delete(id, false)` 的级联能力
/// 只通过 repo API 暴露，留给 M3 的 purge / 清理流程）。
async fn delete_comment(
    State(state): State<Arc<AppState>>,
    Path(comment_id): Path<String>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let comment_id = parse_id_param(&comment_id, "comment id")?;
    let repo = CommentRepo::new(&state.db);
    let (row, role) = load_live_comment_for_member(&state, &repo, comment_id, user.id()).await?;
    ensure_author_or_admin(&row, &role, user.id(), "delete")?;

    repo.soft_delete(comment_id, true)
        .await
        .map_err(comment_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/comments/{commentId}/resolve
async fn resolve_comment(
    State(state): State<Arc<AppState>>,
    Path(comment_id): Path<String>,
    user: AuthUser,
) -> ApiResult<Json<CommentDto>> {
    let comment_id = parse_id_param(&comment_id, "comment id")?;
    let repo = CommentRepo::new(&state.db);
    let (_row, _role) = load_live_comment_for_member(&state, &repo, comment_id, user.id()).await?;
    // TODO(M2-B)：上游在同一事务里跑 `ClearOtherThreadResolutions`，保证一个线程
    // 至多一条 resolution。本仓库暂不实现（docs/12-M2-COMMENT.md 已记录）。
    let updated = repo.resolve(comment_id).await.map_err(comment_err)?;
    Ok(Json(CommentDto::bare(&updated)))
}

/// DELETE /api/comments/{commentId}/resolve
async fn unresolve_comment(
    State(state): State<Arc<AppState>>,
    Path(comment_id): Path<String>,
    user: AuthUser,
) -> ApiResult<Json<CommentDto>> {
    let comment_id = parse_id_param(&comment_id, "comment id")?;
    let repo = CommentRepo::new(&state.db);
    let (_row, _role) = load_live_comment_for_member(&state, &repo, comment_id, user.id()).await?;
    let updated = repo.unresolve(comment_id).await.map_err(comment_err)?;
    Ok(Json(CommentDto::bare(&updated)))
}

/// POST /api/comments/{commentId}/reactions
async fn add_reaction(
    State(state): State<Arc<AppState>>,
    Path(comment_id): Path<String>,
    user: AuthUser,
    Json(req): Json<ReactionRequest>,
) -> ApiResult<(StatusCode, Json<ReactionDto>)> {
    let comment_id = parse_id_param(&comment_id, "comment id")?;
    if req.emoji.is_empty() {
        return Err(bad_request("emoji is required").into());
    }
    let repo = CommentRepo::new(&state.db);
    let (_row, _role) = load_live_comment_for_member(&state, &repo, comment_id, user.id()).await?;

    let reaction = repo
        .add_reaction(comment_id, "user", &user.id().to_string(), &req.emoji)
        .await
        .map_err(comment_err)?;
    Ok((StatusCode::CREATED, Json(ReactionDto::from(&reaction))))
}

/// DELETE /api/comments/{commentId}/reactions
async fn remove_reaction(
    State(state): State<Arc<AppState>>,
    Path(comment_id): Path<String>,
    user: AuthUser,
    Json(req): Json<ReactionRequest>,
) -> ApiResult<StatusCode> {
    let comment_id = parse_id_param(&comment_id, "comment id")?;
    if req.emoji.is_empty() {
        return Err(bad_request("emoji is required").into());
    }
    let repo = CommentRepo::new(&state.db);
    let (_row, _role) = load_live_comment_for_member(&state, &repo, comment_id, user.id()).await?;

    // 幂等：没删到任何行也是 204（对齐上游 `RemoveReaction`）。
    repo.remove_reaction(comment_id, "user", &user.id().to_string(), &req.emoji)
        .await
        .map_err(comment_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/comments/{commentId}/sub-issues
///
/// 上游 `CreateCommentSubIssue` 走 source-context token 流程：
/// `ParseSourceContextToken` → `BuildSourceContext` → digest 比对（不符 409
/// `source_context_changed`）→ `CreateIssue`（mode `manual` / `agent`）。
/// 其中 issue 域 repo（M2-A）已并入，但 source-context token 基础设施属于
/// `mc-source-context`——本仓尚未建立，所以这里返回 **501 Not Implemented**
/// （鉴权前置照常执行，权限边界不放松）。集成仲裁见 `docs/12-M2-COMMENT.md` §6.1。
async fn create_comment_sub_issue(
    State(state): State<Arc<AppState>>,
    Path(comment_id): Path<String>,
    user: AuthUser,
) -> ApiResult<Response> {
    let comment_id = parse_id_param(&comment_id, "comment id")?;
    let repo = CommentRepo::new(&state.db);
    let _ = load_live_comment_for_member(&state, &repo, comment_id, user.id()).await?;

    Ok((
        StatusCode::NOT_IMPLEMENTED,
        Json(NotImplementedBody {
            code: "not_implemented",
            message: "POST /api/comments/{commentId}/sub-issues is not implemented in the M2-B slice",
            todo: "needs mc-source-context (ParseSourceContextToken/BuildSourceContext) — not part of the M2 slices",
        }),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn list_query_defaults_are_empty() {
        let parsed = parse_list_query(&q(&[])).expect("empty query parses");
        assert!(parsed.since.is_none());
        assert!(parsed.before.is_none());
        assert!(parsed.thread.is_none());
        assert!(!parsed.roots_only);
        assert!(parsed.limit.is_none());
    }

    #[test]
    fn list_query_accepts_hyphen_aliases() {
        let id = Id::new();
        let parsed = parse_list_query(&q(&[
            ("roots-only", "true"),
            ("before-id", &id.to_string()),
            ("before", "2026-09-22T00:00:00Z"),
            ("thread", &id.to_string()),
            ("limit", "7"),
        ]))
        .expect("aliases parse");
        assert!(parsed.roots_only);
        assert_eq!(parsed.before_id, Some(id));
        assert_eq!(parsed.thread, Some(id));
        assert_eq!(parsed.limit, Some(7));
    }

    #[test]
    fn list_query_rejects_bad_input() {
        // 严格布尔（上游 "invalid roots_only parameter; expected boolean"）
        assert!(parse_list_query(&q(&[("roots_only", "1")])).is_err());
        // 非 RFC3339 时间
        assert!(parse_list_query(&q(&[("since", "yesterday")])).is_err());
        // 非 uuid
        assert!(parse_list_query(&q(&[("thread", "not-a-uuid")])).is_err());
        // limit 非整数
        assert!(parse_list_query(&q(&[("limit", "many")])).is_err());
        // 游标两半必须成对
        assert!(parse_list_query(&q(&[("before", "2026-09-22T00:00:00Z")])).is_err());
    }

    #[test]
    fn list_query_rejects_unimplemented_modes() {
        for param in UNSUPPORTED_QUERY_PARAMS {
            let err =
                parse_list_query(&q(&[(param, "true")])).expect_err("unimplemented mode must 400");
            assert!(err.to_string().contains(param), "{err}");
        }
    }

    #[test]
    fn content_sanitization() {
        assert_eq!(
            sanitize_and_require_content("a\0b").expect("nul stripped"),
            "ab"
        );
        assert!(sanitize_and_require_content("").is_err());
        assert!(sanitize_and_require_content("\0\0").is_err());
    }

    #[test]
    fn comment_type_gate() {
        assert!(ensure_comment_type(None).is_ok());
        assert!(ensure_comment_type(Some("comment")).is_ok());
        assert!(ensure_comment_type(Some("progress_update")).is_err());
        assert!(ensure_comment_type(Some("status_change")).is_err());
        assert!(ensure_comment_type(Some("system")).is_err());
    }

    #[test]
    fn author_or_admin_gate() {
        let me = Id::new();
        let other = Id::new();
        let row = CommentRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            issue_id: Uuid::new_v4(),
            parent_id: None,
            author_type: "user".into(),
            author_id: other.to_string(),
            body: "x".into(),
            source_task_id: None,
            routing_escalation: None,
            revision: 1,
            resolved_at: None,
            deleted_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        // 非作者 + member → 403
        assert!(ensure_author_or_admin(&row, "member", me, "edit").is_err());
        // 非作者 + admin → 放行（上游 roleAllowed(owner, admin)）
        assert!(ensure_author_or_admin(&row, "admin", me, "edit").is_ok());
        assert!(ensure_author_or_admin(&row, "owner", me, "edit").is_ok());
        // 作者本人 → 放行
        assert!(ensure_author_or_admin(&row, "member", other, "delete").is_ok());
        // guest 不是 admin
        assert!(ensure_author_or_admin(&row, "guest", me, "delete").is_err());
    }
}
