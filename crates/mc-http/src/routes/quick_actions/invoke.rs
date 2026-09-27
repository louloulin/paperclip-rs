//! issue 侧的 `render` / `run`（两条 **plain** 注册，无尾斜杠别名）。
//!
//! 上游 `RenderQuickAction`（`quick_action.go:791`）/ `RunQuickAction`（`:849`），
//! 路由在 `router.go:1995-1996`，挂 `router.go:1948` 的 `RequireWorkspaceMember` 组。
//!
//! ## 权限只在一处（上游文件头的原话："PERMISSION IS CHECKED IN EXACTLY ONE PLACE"）
//!
//! 目录读面**不**做权限工作 ⇒ 一个成员很可能点得开一个他跑不了的动作。上游对此的
//! 选择是：让它 403，并把结构化理由交给客户端渲染成对话框 —— 比一个静默消失的按钮
//! 好排查得多。本片照抄这个选择，两条都用 [`QuickActionRepo::can_invoke`]。
//!
//! ## run 做了什么 / 没做什么
//!
//! 上游的 run = 「渲染 → 落一条**普通**评论（带 `quick_action_id`）→ 交给评论触发链」。
//! 本仓**没有**那条触发链（`routes/comments/mod.rs` 的模块头把「@agent 触发、
//! realtime 广播、inbox 通知」列为别的切片）⇒ 本片只做前两步 + 计数，**不**伪造
//! `trigger_outcomes`（那会是静默假成功）。偏差登记在 `docs/32` §9.22。
//!
//! `render` 之所以**不**因为「只读」就绕过 invoke 闸：预览会把**触发所需的完整文本**
//! 交到用户手里，跳过那道闸等于把「调一个我不能调的 agent」变成一次复制粘贴。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, SecondsFormat, Utc};
use mc_core::Id;
use mc_errors::Error;
use serde::Serialize;
use uuid::Uuid;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::issues::{issue_repo, load_issue, WorkspaceQuery};
use crate::state::AppState;

use super::{
    archived_error, build_body, invocation_not_allowed, load_reachable, parse_id, repo, repo_err,
    require_workspace_member, target_unavailable, triage_blocked, workspace_of, QuickActionRow,
    QuickActionTarget,
};

/// `POST /api/issues/:id/quick-actions/:quickActionId/render`（上游 `RenderQuickAction`）。
///
/// 返回**会**发出去的那段文本（`{"content": "..."}`），不落库。
pub(crate) async fn render_action(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_issue, raw_action)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Response> {
    let (action, target) =
        prepare(&state, &headers, &query, &raw_issue, &raw_action, user.id()).await?;
    Ok(axum::Json(RenderResponse {
        content: build_body(&action, &target),
    })
    .into_response())
}

/// `POST /api/issues/:id/quick-actions/:quickActionId/run`（上游 `RunQuickAction`；201）。
pub(crate) async fn run_action(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_issue, raw_action)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Response> {
    let workspace_id = workspace_of(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let issue = load_issue(&issue_repo(&state), workspace_id, &raw_issue).await?;
    let action_id = parse_id("quick action id", &raw_action)?;
    let action = load_reachable(&state, workspace_id, action_id, user.id()).await?;
    if action.is_archived() {
        return Err(archived_error().into());
    }
    // triage 的 issue 上**不写**评论（上游同款，理由见 `triage_blocked` 的注释）：
    // quick action 既是评论又是 run，先落评论就等于留下一条写给谁的指令。
    if issue.triage_state.is_some() {
        return Err(triage_blocked().into());
    }
    let target = resolve_target(&state, workspace_id, &action).await?;
    if !repo(&state)
        .can_invoke(target.agent_id, user.id())
        .await
        .map_err(repo_err)?
    {
        return Err(invocation_not_allowed().into());
    }

    let body = sanitize_null_bytes(&build_body(&action, &target));
    let comment = insert_comment(&state, issue.id(), user.id(), &body, action.id()).await?;

    // 计数是 best effort 且**在成功路径之外**（上游逐字）：一次失败的计数器
    // 绝不能把用户刚跑成功的一次动作变成失败。
    if let Err(err) = repo(&state).touch_usage(workspace_id, action.id()).await {
        eprintln!("quick action usage touch failed: {err}");
    }

    Ok((
        StatusCode::CREATED,
        axum::Json(CommentResponse::new(&comment, action.id())),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// 内部
// ---------------------------------------------------------------------------

/// `render` 的响应（上游 `map[string]string{"content": ...}`）。
#[derive(Debug, Serialize)]
struct RenderResponse {
    content: String,
}

/// run 的响应：一条**普通**评论（上游刻意让 `type` 恒为 `comment` —— 客户端可写的
/// 泛化端点能伪造任何 `type`，而折叠卡片由**不可写**的 `quick_action_id` 驱动）。
#[derive(Debug, Serialize)]
struct CommentResponse {
    id: String,
    issue_id: String,
    parent_id: Option<String>,
    author_type: String,
    author_id: String,
    content: String,
    #[serde(rename = "type")]
    kind: &'static str,
    quick_action_id: String,
    revision: i64,
    resolved_at: Option<String>,
    created_at: String,
    updated_at: String,
    reactions: Vec<serde_json::Value>,
    attachments: Vec<serde_json::Value>,
}

/// 落库的那一行（`comment` 表的最小投影 + `quick_action_id`）。
#[derive(Debug, sqlx::FromRow)]
struct CommentRowOut {
    id: Uuid,
    issue_id: Uuid,
    parent_id: Option<Uuid>,
    author_type: String,
    author_id: Uuid,
    content: String,
    revision: i64,
    resolved_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl CommentResponse {
    fn new(row: &CommentRowOut, quick_action_id: Id) -> Self {
        fn ts(at: &DateTime<Utc>) -> String {
            at.to_rfc3339_opts(SecondsFormat::Micros, true)
        }
        Self {
            id: row.id.to_string(),
            issue_id: row.issue_id.to_string(),
            parent_id: row.parent_id.map(|p| p.to_string()),
            author_type: row.author_type.clone(),
            author_id: row.author_id.to_string(),
            content: row.content.clone(),
            kind: "comment",
            quick_action_id: quick_action_id.to_string(),
            revision: row.revision,
            resolved_at: row.resolved_at.as_ref().map(ts),
            created_at: ts(&row.created_at),
            updated_at: ts(&row.updated_at),
            reactions: Vec::new(),
            attachments: Vec::new(),
        }
    }
}

/// `render` 的那三步共用前置：issue 可见 → 动作**够得着** → 目标能 invoke。
///
/// ⚠️ 顺序照上游：`loadIssueForUser` → `loadReachableQuickAction`（404）→ 目标解析
/// （409）→ invoke 闸（403）。三者的状态码各不相同，顺序换了就把「不该知道存不存在」
/// 的那条泄露成 403。
async fn prepare(
    state: &AppState,
    headers: &HeaderMap,
    query: &WorkspaceQuery,
    raw_issue: &str,
    raw_action: &str,
    user: Id,
) -> Result<(QuickActionRow, QuickActionTarget), Error> {
    let workspace_id = workspace_of(state, headers, query).await?;
    require_workspace_member(state, workspace_id, user).await?;
    let _issue = load_issue(&issue_repo(state), workspace_id, raw_issue).await?;
    let action_id = parse_id("quick action id", raw_action)?;
    let action = load_reachable(state, workspace_id, action_id, user).await?;
    let target = resolve_target(state, workspace_id, &action).await?;
    if !repo(state)
        .can_invoke(target.agent_id, user)
        .await
        .map_err(repo_err)?
    {
        return Err(invocation_not_allowed());
    }
    Ok((action, target))
}

/// 目标解析：解析不出来 ⇒ **409**（不是 404：动作本身是可见的，坏的是它绑的东西）。
async fn resolve_target(
    state: &AppState,
    workspace_id: Id,
    action: &QuickActionRow,
) -> Result<QuickActionTarget, Error> {
    repo(state)
        .resolve_target(workspace_id, &action.assignee_type, action.assignee_id)
        .await
        .map_err(repo_err)?
        .ok_or_else(target_unavailable)
}

/// 落评论（run 的唯一写）。`quick_action_id` 由这条**专用**插入写入 ——
/// 泛化评论端点不接受它（上游 `239_comment_quick_action.up.sql` 的原话：
/// 「客户端可以写 `type`，所以不能靠 `type` 折叠」）。
async fn insert_comment(
    state: &AppState,
    issue_id: Id,
    author: Id,
    content: &str,
    quick_action_id: Id,
) -> Result<CommentRowOut, Error> {
    sqlx::query_as(
        "INSERT INTO comment (workspace_id, issue_id, author_type, author_id, content, \
             type, quick_action_id) \
         SELECT i.workspace_id, i.id, 'member', $1, $2, 'comment', $3 FROM issue i \
         WHERE i.id = $4 \
         RETURNING id, issue_id, parent_id, author_type, author_id, content, revision, \
                   resolved_at, created_at, updated_at",
    )
    .bind(author.0)
    .bind(content)
    .bind(quick_action_id.0)
    .bind(issue_id.0)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| Error::Database(e.to_string()))?
    .ok_or_else(|| Error::NotFound {
        resource: "issue".into(),
    })
}

/// 上游 `sanitizeNullBytes`：NUL 会让下游的 mention 解析与日志都难办。
fn sanitize_null_bytes(raw: &str) -> String {
    raw.replace('\0', "")
}
