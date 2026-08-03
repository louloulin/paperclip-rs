//! `/api/issues*` 路由：完整 issue 生命周期。
//!
//! 覆盖：CRUD / children / comments / labels / read state / inbox archive。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::issue::IssueRepo;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        // 列表 / CRUD
        .route("/api/issues", get(list).post(create))
        .route("/api/issues/:id", get(get_one).patch(update).delete(remove))
        // 子 issue
        .route(
            "/api/issues/:id/children",
            get(list_children).post(create_child),
        )
        // comments
        .route(
            "/api/issues/:id/comments",
            get(list_comments).post(add_comment),
        )
        .route(
            "/api/issues/:id/comments/:comment_id",
            patch(update_comment).delete(delete_comment),
        )
        // labels
        .route(
            "/api/companies/:company_id/labels",
            get(list_labels).post(create_label),
        )
        .route("/api/labels/:label_id", delete(remove_label))
        .route(
            "/api/issues/:id/labels/:label_id",
            post(assign_label).delete(unassign_label),
        )
        // read state
        .route("/api/issues/:id/read", get(get_read).put(upsert_read))
        // inbox archive
        .route(
            "/api/issues/:id/inbox-archive",
            get(get_inbox).put(archive_inbox).delete(unarchive_inbox),
        )
        // release
        .route("/api/issues/:id/release", post(release))
        .route("/api/issues/:id/admin/force-release", post(force_release))
        // watchdog
        .route(
            "/api/issues/:id/watchdog",
            get(get_watchdog)
                .put(upsert_watchdog)
                .delete(remove_watchdog),
        )
        // recovery actions
        .route(
            "/api/issues/:id/recovery-actions",
            get(list_recovery_actions),
        )
        .route(
            "/api/issues/:id/recovery-actions/resolve",
            post(resolve_recovery),
        )
        // work products
        .route(
            "/api/issues/:id/work-products",
            get(list_work_products).post(create_work_product),
        )
        .route(
            "/api/work-products/:id",
            get(get_work_product)
                .patch(patch_work_product)
                .delete(remove_work_product),
        )
        // documents
        .route("/api/issues/:id/documents", get(list_documents))
        .route(
            "/api/issues/:id/documents/:key",
            get(get_document)
                .put(upsert_document)
                .delete(remove_document),
        )
        .route("/api/issues/:id/documents/:key/lock", post(lock_doc))
        .route("/api/issues/:id/documents/:key/unlock", post(unlock_doc))
        .route(
            "/api/issues/:id/documents/:key/revisions",
            get(list_revisions).post(restore_revision),
        )
        .route(
            "/api/issues/:id/documents/:key/annotations",
            get(list_annotations).post(create_annotation),
        )
        .route(
            "/api/issues/:id/documents/:key/annotations/:thread_id",
            get(get_annotation_with_comments)
                .post(add_annotation_comment)
                .patch(resolve_annotation),
        )
        // issue approvals
        .route(
            "/api/issues/:id/approvals",
            get(list_issue_approvals).post(link_issue_approval),
        )
        .route(
            "/api/issues/:id/approvals/:approval_id",
            delete(unlink_issue_approval).patch(decide_issue_approval),
        )
        // thread interactions
        .route(
            "/api/issues/:id/interactions",
            get(list_interactions).post(create_interaction),
        )
        .route(
            "/api/issues/:id/interactions/:interaction_id",
            get(get_interaction).patch(resolve_interaction_route),
        )
        // feedback votes
        .route(
            "/api/issues/:id/feedback-votes",
            get(list_feedback_votes).post(create_feedback_vote),
        )
        // attachments
        .route(
            "/api/issues/:id/attachments",
            get(list_attachments).post(create_attachment),
        )
        .route(
            "/api/attachments/:attachment_id",
            get(get_attachment).delete(remove_attachment),
        )
        // external objects
        .route(
            "/api/issues/:id/external-objects",
            get(list_external_objects),
        )
        .route(
            "/api/issues/:id/external-object-summary",
            get(external_object_summary),
        )
        // diagnostics
        .route("/api/issues/:id/diagnostics/blockers", get(diag_blockers))
        .route("/api/issues/:id/diagnostics/wakes", get(diag_wakes))
        .route("/api/issues/:id/diagnostics/subtree", get(diag_subtree))
        // tree control
        .route("/api/issues/:id/monitor/check-now", post(monitor_check_now))
        .route(
            "/api/issues/:id/scheduled-retry/retry-now",
            post(scheduled_retry_now),
        )
        // company-level
        .route(
            "/api/companies/:company_id/issues/count",
            get(count_company_issues),
        )
        .route("/api/companies/:company_id/search", get(search_issues))
}

// ============================================================================
// 列表 / CRUD
// ============================================================================

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    status: Option<String>,
}

async fn list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = match q.company_id {
        Some(cid) => {
            IssueRepo::new(&state.db)
                .list_by_company(cid, q.status.as_deref())
                .await?
        }
        None => {
            IssueRepo::new(&state.db)
                .list_all(q.status.as_deref(), 200)
                .await?
        }
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    company_id: Uuid,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_priority")]
    priority: String,
    #[serde(default)]
    assignee_agent_id: Option<Uuid>,
    #[serde(default)]
    parent_id: Option<Uuid>,
}
fn default_priority() -> String {
    "medium".into()
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    if let Some(pid) = body.parent_id {
        // 子 issue：通过 create_child 路径以继承 company_id 与 request_depth。
        let parent = IssueRepo::new(&state.db)
            .get(pid)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("parent issue {pid}")))?;
        let row = IssueRepo::new(&state.db)
            .create_child(
                &parent,
                &body.title,
                body.description.as_deref(),
                &body.priority,
                body.assignee_agent_id,
            )
            .await?;
        state.realtime.publish(
            LiveEvent::new("issue.created", "issue", row.id)
                .with_company(row.company_id)
                .with_actor("system"),
        );
        return Ok((
            StatusCode::CREATED,
            Json(json!({
                "id": row.id, "company_id": row.company_id, "parent_id": row.parent_id,
                "title": row.title, "status": row.status, "priority": row.priority
            })),
        ));
    }
    let row = IssueRepo::new(&state.db)
        .create(
            body.company_id,
            &body.title,
            body.description.as_deref(),
            &body.priority,
            body.assignee_agent_id,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("issue.created", "issue", row.id)
            .with_company(row.company_id)
            .with_actor("system"),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "company_id": row.company_id, "title": row.title,
            "status": row.status, "priority": row.priority
        })),
    ))
}

#[derive(Debug, Deserialize)]
struct UpdateBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    assignee_agent_id: Option<Uuid>,
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db)
        .update(
            id,
            body.title.as_deref(),
            body.description.as_deref(),
            body.status.as_deref(),
            body.priority.as_deref(),
            Some(body.assignee_agent_id),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("issue.updated", "issue", row.id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = IssueRepo::new(&state.db).delete(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("issue {id}")))
    }
}

// ============================================================================
// Children
// ============================================================================

async fn list_children(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = IssueRepo::new(&state.db).list_children(id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct ChildBody {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_priority")]
    priority: String,
    #[serde(default)]
    assignee_agent_id: Option<Uuid>,
}

async fn create_child(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ChildBody>,
) -> ApiResult<impl IntoResponse> {
    let parent = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("parent issue {id}")))?;
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let row = IssueRepo::new(&state.db)
        .create_child(
            &parent,
            &body.title,
            body.description.as_deref(),
            &body.priority,
            body.assignee_agent_id,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("issue.created", "issue", row.id)
            .with_company(row.company_id)
            .with_actor("system"),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id,
            "company_id": row.company_id,
            "parent_id": row.parent_id,
            "title": row.title,
            "status": row.status,
            "priority": row.priority,
        })),
    ))
}

// ============================================================================
// Comments
// ============================================================================

async fn list_comments(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = IssueRepo::new(&state.db).list_comments(id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CommentBody {
    body: String,
    #[serde(default)]
    author_agent_id: Option<Uuid>,
    #[serde(default)]
    author_user_id: Option<String>,
}

async fn add_comment(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<CommentBody>,
) -> ApiResult<impl IntoResponse> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    if payload.body.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "comment body must not be empty".into(),
        ));
    }
    let author_user = payload
        .author_user_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let row = IssueRepo::new(&state.db)
        .create_comment(
            issue.company_id,
            issue.id,
            payload.author_agent_id,
            author_user,
            &payload.body,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("issue.comment.added", "issue_comment", row.id)
            .with_company(row.company_id)
            .with_actor(author_user.unwrap_or("system")),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

async fn update_comment(
    State(state): State<AppState>,
    Path((id, comment_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<CommentBody>,
) -> ApiResult<Json<Value>> {
    if payload.body.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "comment body must not be empty".into(),
        ));
    }
    let row = IssueRepo::new(&state.db)
        .update_comment(id, comment_id, &payload.body)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("comment {comment_id}")))?;
    state.realtime.publish(
        LiveEvent::new("issue.comment.updated", "issue_comment", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn delete_comment(
    State(state): State<AppState>,
    Path((id, comment_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ok = IssueRepo::new(&state.db)
        .delete_comment(id, comment_id)
        .await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("comment {comment_id}")))
    }
}

// ============================================================================
// Labels
// ============================================================================

async fn list_labels(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = IssueRepo::new(&state.db).list_labels(company_id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct LabelBody {
    name: String,
    #[serde(default = "default_color")]
    color: String,
}
fn default_color() -> String {
    "#cccccc".into()
}

async fn create_label(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<LabelBody>,
) -> ApiResult<impl IntoResponse> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("label name must not be empty".into()));
    }
    let row = IssueRepo::new(&state.db)
        .create_label(company_id, &body.name, &body.color)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

async fn remove_label(
    State(state): State<AppState>,
    Path(label_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ok = IssueRepo::new(&state.db)
        .delete_label(Uuid::nil(), label_id)
        .await
        .ok();
    // 也尝试按公司删除（label 跨公司不共享，由路由先解析 company_id）
    if ok.unwrap_or(false) {
        return Ok(StatusCode::NO_CONTENT);
    }
    Err(ApiError::NotFound(format!("label {label_id}")))
}

async fn assign_label(
    State(state): State<AppState>,
    Path((id, label_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    IssueRepo::new(&state.db)
        .assign_label(issue.company_id, id, label_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unassign_label(
    State(state): State<AppState>,
    Path((id, label_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let ok = IssueRepo::new(&state.db)
        .unassign_label(issue.company_id, id, label_id)
        .await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!(
            "label assignment for {label_id}"
        )))
    }
}

// ============================================================================
// Read state
// ============================================================================

#[derive(Debug, Deserialize)]
struct ReadBody {
    #[serde(default)]
    last_read_at: Option<pc_core::Timestamp>,
    #[serde(default)]
    user_id: Option<String>,
}

async fn get_read(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user = require_user_id(&state, &headers).await?;
    let row = IssueRepo::new(&state.db).get_read_state(id, &user).await?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn upsert_read(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ReadBody>,
) -> ApiResult<Json<Value>> {
    let user = match body.user_id.clone() {
        Some(u) if !u.trim().is_empty() => u,
        _ => require_user_id(&state, &headers).await?,
    };
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let row = IssueRepo::new(&state.db)
        .upsert_read_state(issue.company_id, id, &user, body.last_read_at)
        .await?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

// ============================================================================
// Inbox archive
// ============================================================================

async fn get_inbox(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user = require_user_id(&state, &headers).await?;
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let rows = IssueRepo::new(&state.db)
        .list_inbox_archives(issue.company_id, &user)
        .await?;
    let row = rows.into_iter().find(|r| r.issue_id == id);
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn archive_inbox(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user = require_user_id(&state, &headers).await?;
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let row = IssueRepo::new(&state.db)
        .archive_inbox(issue.company_id, id, &user)
        .await?;
    state.realtime.publish(
        LiveEvent::new("issue.inbox.archived", "issue", id).with_company(issue.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn unarchive_inbox(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<StatusCode> {
    let user = require_user_id(&state, &headers).await?;
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let ok = IssueRepo::new(&state.db)
        .unarchive_inbox(issue.company_id, id, &user)
        .await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("inbox archive for {id}")))
    }
}

// ============================================================================
// Release
// ============================================================================

#[derive(Debug, Default, Deserialize)]
struct ReleaseBody {
    #[serde(default)]
    run_id: Option<Uuid>,
}

async fn release(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ReleaseBody>,
) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db)
        .release(id, body.run_id)
        .await?
        .ok_or_else(|| ApiError::Conflict("issue not checked out by this run".into()))?;
    state
        .realtime
        .publish(LiveEvent::new("issue.released", "issue", row.id).with_company(row.company_id));
    Ok(Json(json!({
        "id": row.id,
        "status": row.status,
        "checkout_run_id": row.checkout_run_id,
        "execution_locked_at": row.execution_locked_at,
    })))
}

async fn force_release(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db)
        .force_release(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("issue.released", "issue", row.id).with_company(row.company_id));
    Ok(Json(json!({
        "id": row.id,
        "status": row.status,
        "checkout_run_id": row.checkout_run_id,
        "execution_locked_at": row.execution_locked_at,
    })))
}

// ============================================================================
// Watchdog
// ============================================================================

async fn get_watchdog(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db).get_active_watchdog(id).await?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct WatchdogBody {
    watchdog_agent_id: Uuid,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    created_by_user_id: Option<String>,
}

async fn upsert_watchdog(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<WatchdogBody>,
) -> ApiResult<Json<Value>> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let user = body.created_by_user_id.clone().or_else(|| {
        headers
            .get("x-paperclip-user-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    let user_ref = user.as_deref();
    let (row, created) = IssueRepo::new(&state.db)
        .upsert_watchdog(
            issue.company_id,
            id,
            body.watchdog_agent_id,
            body.instructions.as_deref(),
            None,
            user_ref,
            None,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new(
            if created {
                "issue.watchdog_created"
            } else {
                "issue.watchdog_updated"
            },
            "issue_watchdog",
            row.id,
        )
        .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove_watchdog(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db).disable_watchdog(id).await?;
    if let Some(ref w) = row {
        state.realtime.publish(
            LiveEvent::new("issue.watchdog_removed", "issue_watchdog", w.id)
                .with_company(w.company_id),
        );
    }
    Ok(Json(json!({ "ok": true, "disabled": row })))
}

// ============================================================================
// Recovery actions
// ============================================================================

async fn list_recovery_actions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let active = IssueRepo::new(&state.db)
        .get_active_recovery_action(id)
        .await?;
    let actions = if let Some(ref a) = active {
        vec![a.clone()]
    } else {
        Vec::new()
    };
    Ok(Json(json!({
        "active": active,
        "actions": actions,
    })))
}

#[derive(Debug, Deserialize)]
struct ResolveRecoveryBody {
    action_id: Uuid,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    resolution_note: Option<String>,
}

async fn resolve_recovery(
    State(state): State<AppState>,
    Path(_id): Path<Uuid>,
    Json(body): Json<ResolveRecoveryBody>,
) -> ApiResult<Json<Value>> {
    let outcome = body.outcome.as_deref().unwrap_or("resolved");
    let row = IssueRepo::new(&state.db)
        .resolve_recovery_action(body.action_id, body.resolution_note.as_deref(), outcome)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("recovery action {}", body.action_id)))?;
    state.realtime.publish(
        LiveEvent::new("issue.recovery.resolved", "issue_recovery_action", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

// ============================================================================
// Work products
// ============================================================================

async fn list_work_products(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = IssueRepo::new(&state.db).list_work_products(id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_work_product(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db)
        .get_work_product(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("work product {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateWorkProductBody {
    #[serde(rename = "type")]
    product_type: String,
    #[serde(default = "default_provider")]
    provider: String,
    #[serde(default)]
    external_id: Option<String>,
    title: String,
    #[serde(default = "default_wp_status")]
    status: String,
    #[serde(default = "default_wp_review")]
    review_state: String,
    #[serde(default)]
    is_primary: bool,
    #[serde(default = "default_wp_health")]
    health_status: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}
fn default_provider() -> String {
    "paperclip".into()
}
fn default_wp_status() -> String {
    "active".into()
}
fn default_wp_review() -> String {
    "pending".into()
}
fn default_wp_health() -> String {
    "unknown".into()
}

async fn create_work_product(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateWorkProductBody>,
) -> ApiResult<impl IntoResponse> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let row = IssueRepo::new(&state.db)
        .create_work_product(
            issue.company_id,
            id,
            issue.project_id,
            &body.product_type,
            &body.provider,
            body.external_id.as_deref(),
            &body.title,
            &body.status,
            &body.review_state,
            body.is_primary,
            &body.health_status,
            body.summary.as_deref(),
            body.metadata.as_ref(),
            None,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("issue.work_product.created", "issue_work_product", row.id)
            .with_company(row.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

#[derive(Debug, Deserialize, Default)]
struct UpdateWorkProductBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    review_state: Option<String>,
    #[serde(default)]
    is_primary: Option<bool>,
    #[serde(default)]
    health_status: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

async fn patch_work_product(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateWorkProductBody>,
) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db)
        .update_work_product(
            id,
            body.title.as_deref(),
            body.status.as_deref(),
            body.review_state.as_deref(),
            body.is_primary,
            body.health_status.as_deref(),
            body.summary.as_deref(),
            body.metadata.as_ref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("work product {id}")))?;
    state.realtime.publish(
        LiveEvent::new("issue.work_product.updated", "issue_work_product", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove_work_product(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ok = IssueRepo::new(&state.db).delete_work_product(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("work product {id}")))
    }
}

// ============================================================================
// Documents
// ============================================================================

async fn list_documents(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = pc_repos::document::DocumentRepo::new(&state.db)
        .list_issue_documents(id)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_document(
    State(state): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let row = pc_repos::document::DocumentRepo::new(&state.db)
        .get_issue_document_by_key(id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("document {key} for issue {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct UpsertDocumentBody {
    title: Option<String>,
    body: String,
    #[serde(default = "default_doc_format")]
    format: String,
    #[serde(default)]
    created_by_user_id: Option<String>,
}
fn default_doc_format() -> String {
    "markdown".into()
}

async fn upsert_document(
    State(state): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
    headers: axum::http::HeaderMap,
    Json(body): Json<UpsertDocumentBody>,
) -> ApiResult<Json<Value>> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let user = body.created_by_user_id.clone().or_else(|| {
        headers
            .get("x-paperclip-user-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    let row = pc_repos::document::DocumentRepo::new(&state.db)
        .upsert_issue_document(
            issue.company_id,
            id,
            &key,
            body.title.as_deref(),
            &body.body,
            &body.format,
            user.as_deref(),
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("issue.document.upserted", "document", row.id).with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove_document(
    State(state): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    let ok = pc_repos::document::DocumentRepo::new(&state.db)
        .delete_issue_document(id, &key)
        .await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("document {key}")))
    }
}

async fn lock_doc(
    State(state): State<AppState>,
    Path((_id, key)): Path<(Uuid, String)>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    // 通过 (issue_id, key) 查找 document_id
    let doc = pc_repos::document::DocumentRepo::new(&state.db)
        .get_issue_document_by_key(_id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("document {key}")))?;
    let user = headers
        .get("x-paperclip-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let row = pc_repos::document::DocumentRepo::new(&state.db)
        .lock_document(doc.id, None, user.as_deref())
        .await?
        .ok_or_else(|| ApiError::Conflict("document is already locked".into()))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn unlock_doc(
    State(state): State<AppState>,
    Path((_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let doc = pc_repos::document::DocumentRepo::new(&state.db)
        .get_issue_document_by_key(_id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("document {key}")))?;
    let row = pc_repos::document::DocumentRepo::new(&state.db)
        .unlock_document(doc.id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("document {key}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn list_revisions(
    State(state): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let doc = pc_repos::document::DocumentRepo::new(&state.db)
        .get_issue_document_by_key(id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("document {key}")))?;
    let rows = pc_repos::document::DocumentRepo::new(&state.db)
        .list_revisions(doc.id)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct RestoreRevisionBody {
    revision_number: i32,
    #[serde(default)]
    created_by_user_id: Option<String>,
}

async fn restore_revision(
    State(state): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RestoreRevisionBody>,
) -> ApiResult<Json<Value>> {
    let doc = pc_repos::document::DocumentRepo::new(&state.db)
        .get_issue_document_by_key(id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("document {key}")))?;
    let user = body.created_by_user_id.clone().or_else(|| {
        headers
            .get("x-paperclip-user-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    let rev = pc_repos::document::DocumentRepo::new(&state.db)
        .restore_revision(doc.id, body.revision_number, user.as_deref())
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("revision {}", body.revision_number)))?;
    Ok(Json(serde_json::to_value(rev).unwrap_or_default()))
}

// ============================================================================
// Annotations
// ============================================================================

async fn list_annotations(
    State(state): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let doc = pc_repos::document::DocumentRepo::new(&state.db)
        .get_issue_document_by_key(id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("document {key}")))?;
    let rows = pc_repos::document::DocumentRepo::new(&state.db)
        .list_annotation_threads(doc.id, &key)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateAnnotationBody {
    /// 必填：被标注文本范围
    selected_text: String,
    #[serde(default)]
    prefix_text: Option<String>,
    #[serde(default)]
    suffix_text: Option<String>,
    normalized_start: i32,
    normalized_end: i32,
    markdown_start: i32,
    markdown_end: i32,
    /// 可选：锚点选择器 JSON（默认 {}）
    #[serde(default)]
    anchor_selector: Option<serde_json::Value>,
    #[serde(default)]
    anchor_confidence: Option<String>,
    /// 可选：首条 comment
    #[serde(default)]
    body: Option<String>,
    /// 可选：revision 编号（默认 1）
    #[serde(default = "default_anchor_revision")]
    current_revision_number: i32,
    #[serde(default = "default_anchor_revision")]
    original_revision_number: i32,
    #[serde(default)]
    created_by_user_id: Option<String>,
}
fn default_anchor_revision() -> i32 {
    1
}

async fn create_annotation(
    State(state): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateAnnotationBody>,
) -> ApiResult<impl IntoResponse> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let doc = pc_repos::document::DocumentRepo::new(&state.db)
        .get_issue_document_by_key(id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("document {key}")))?;
    let rev_id = doc.latest_revision_id.unwrap_or_else(Uuid::new_v4);
    let user = body.created_by_user_id.clone().or_else(|| {
        headers
            .get("x-paperclip-user-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    let anchor_selector = body
        .anchor_selector
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    let anchor_confidence = body.anchor_confidence.as_deref().unwrap_or("exact");
    let prefix = body.prefix_text.as_deref().unwrap_or("");
    let suffix = body.suffix_text.as_deref().unwrap_or("");
    let thread = pc_repos::document::DocumentRepo::new(&state.db)
        .create_annotation_thread(
            issue.company_id,
            id,
            doc.id,
            &key,
            rev_id,
            body.original_revision_number,
            rev_id,
            body.current_revision_number,
            &body.selected_text,
            prefix,
            suffix,
            body.normalized_start,
            body.normalized_end,
            body.markdown_start,
            body.markdown_end,
            anchor_confidence,
            &anchor_selector,
            user.as_deref(),
        )
        .await?;
    if let Some(text) = body.body.as_deref() {
        if !text.trim().is_empty() {
            pc_repos::document::DocumentRepo::new(&state.db)
                .create_annotation_comment(
                    issue.company_id,
                    thread.id,
                    id,
                    doc.id,
                    text,
                    "user",
                    user.as_deref(),
                )
                .await?;
        }
    }
    state.realtime.publish(
        LiveEvent::new(
            "issue.annotation.created",
            "document_annotation_thread",
            thread.id,
        )
        .with_company(thread.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(thread).unwrap_or_default()),
    ))
}

async fn get_annotation_with_comments(
    State(state): State<AppState>,
    Path((_id, _key, thread_id)): Path<(Uuid, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let thread = pc_repos::document::DocumentRepo::new(&state.db)
        .get_annotation_thread(thread_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("annotation thread {thread_id}")))?;
    let comments = pc_repos::document::DocumentRepo::new(&state.db)
        .list_annotation_comments(thread_id)
        .await?;
    Ok(Json(json!({
        "thread": thread,
        "comments": comments,
    })))
}

#[derive(Debug, Deserialize)]
struct AnnotationCommentBody {
    body: String,
    #[serde(default = "default_author_type")]
    author_type: String,
    #[serde(default)]
    author_user_id: Option<String>,
}
fn default_author_type() -> String {
    "user".into()
}

async fn add_annotation_comment(
    State(state): State<AppState>,
    Path((id, _key, thread_id)): Path<(Uuid, String, Uuid)>,
    Json(body): Json<AnnotationCommentBody>,
) -> ApiResult<impl IntoResponse> {
    if body.body.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "comment body must not be empty".into(),
        ));
    }
    let thread = pc_repos::document::DocumentRepo::new(&state.db)
        .get_annotation_thread(thread_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("annotation thread {thread_id}")))?;
    let row = pc_repos::document::DocumentRepo::new(&state.db)
        .create_annotation_comment(
            thread.company_id,
            thread_id,
            id,
            thread.document_id,
            &body.body,
            &body.author_type,
            body.author_user_id.as_deref(),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

async fn resolve_annotation(
    State(state): State<AppState>,
    Path((_id, _key, thread_id)): Path<(Uuid, String, Uuid)>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user = headers
        .get("x-paperclip-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let row = pc_repos::document::DocumentRepo::new(&state.db)
        .resolve_annotation_thread(thread_id, user.as_deref())
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("annotation thread {thread_id}")))?;
    state.realtime.publish(
        LiveEvent::new(
            "issue.annotation.resolved",
            "document_annotation_thread",
            row.id,
        )
        .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

// ============================================================================
// Issue approvals
// ============================================================================

async fn list_issue_approvals(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let links = IssueRepo::new(&state.db).list_issue_approvals(id).await?;
    if links.is_empty() {
        return Ok(Json(serde_json::Value::Array(vec![])));
    }
    // 同时取 approval 详情
    let approval_repo = pc_repos::approval::ApprovalRepo::new(&state.db);
    let mut out = Vec::with_capacity(links.len());
    for link in links {
        if let Some(approval) = approval_repo.get_id_only(link.approval_id).await? {
            out.push(serde_json::json!({
                "issue_id": link.issue_id,
                "approval_id": link.approval_id,
                "linked_by_agent_id": link.linked_by_agent_id,
                "linked_by_user_id": link.linked_by_user_id,
                "created_at": link.created_at,
                "approval": approval,
            }));
        }
    }
    Ok(Json(serde_json::to_value(out).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct LinkApprovalBody {
    approval_id: Uuid,
    #[serde(default)]
    linked_by_user_id: Option<String>,
}

async fn link_issue_approval(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<LinkApprovalBody>,
) -> ApiResult<Json<Value>> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let user = body.linked_by_user_id.clone().or_else(|| {
        headers
            .get("x-paperclip-user-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    let row = IssueRepo::new(&state.db)
        .link_approval(
            issue.company_id,
            id,
            body.approval_id,
            None,
            user.as_deref(),
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("issue.approval.linked", "issue_approval", row.approval_id)
            .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn unlink_issue_approval(
    State(state): State<AppState>,
    Path((id, approval_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ok = IssueRepo::new(&state.db)
        .unlink_approval(id, approval_id)
        .await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("approval link")))
    }
}

#[derive(Debug, Deserialize)]
struct DecideApprovalBody {
    decision: String, // "approved" | "rejected"
    #[serde(default)]
    decision_note: Option<String>,
    #[serde(default)]
    decided_by_user_id: Option<String>,
}

async fn decide_issue_approval(
    State(state): State<AppState>,
    Path((_id, approval_id)): Path<(Uuid, Uuid)>,
    headers: axum::http::HeaderMap,
    Json(body): Json<DecideApprovalBody>,
) -> ApiResult<Json<Value>> {
    let user = body.decided_by_user_id.clone().or_else(|| {
        headers
            .get("x-paperclip-user-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    let row = pc_repos::approval::ApprovalRepo::new(&state.db)
        .decide_four_args(
            approval_id,
            &body.decision,
            body.decision_note.as_deref(),
            user.as_deref().unwrap_or("system"),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("approval {approval_id}")))?;
    state.realtime.publish(
        LiveEvent::new("issue.approval.decided", "approval", row.id).with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

// ============================================================================
// Thread interactions
// ============================================================================

async fn list_interactions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = IssueRepo::new(&state.db).list_interactions(id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_interaction(
    State(state): State<AppState>,
    Path((_id, interaction_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db)
        .get_interaction(interaction_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("interaction {interaction_id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateInteractionBody {
    kind: String,
    #[serde(default = "default_continuation")]
    continuation_policy: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    created_by_user_id: Option<String>,
}
fn default_continuation() -> String {
    "wake_assignee".into()
}

async fn create_interaction(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateInteractionBody>,
) -> ApiResult<impl IntoResponse> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let user = body.created_by_user_id.clone().or_else(|| {
        headers
            .get("x-paperclip-user-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    let payload = body.payload.unwrap_or_else(|| serde_json::json!({}));
    let row = IssueRepo::new(&state.db)
        .create_interaction(
            issue.company_id,
            id,
            &body.kind,
            &body.continuation_policy,
            body.title.as_deref(),
            body.summary.as_deref(),
            &payload,
            None,
            user.as_deref(),
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new(
            "issue.interaction.created",
            "issue_thread_interaction",
            row.id,
        )
        .with_company(row.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

#[derive(Debug, Deserialize)]
struct ResolveInteractionBody {
    /// 新状态：accepted / rejected / cancelled / withdrawn / responded
    status: String,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    resolved_by_user_id: Option<String>,
}

async fn resolve_interaction_route(
    State(state): State<AppState>,
    Path((_id, interaction_id)): Path<(Uuid, Uuid)>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ResolveInteractionBody>,
) -> ApiResult<Json<Value>> {
    let user = body.resolved_by_user_id.clone().or_else(|| {
        headers
            .get("x-paperclip-user-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    let row = IssueRepo::new(&state.db)
        .resolve_interaction(
            interaction_id,
            &body.status,
            body.result.as_ref(),
            user.as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("interaction {interaction_id}")))?;
    state.realtime.publish(
        LiveEvent::new(
            "issue.interaction.resolved",
            "issue_thread_interaction",
            row.id,
        )
        .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

// ============================================================================
// Feedback votes
// ============================================================================

async fn list_feedback_votes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = IssueRepo::new(&state.db).list_feedback_votes(id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateFeedbackVoteBody {
    target_type: String, // "issue" | "comment" | "work_product" | "document"
    target_id: String,
    vote: String, // "up" | "down"
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    author_user_id: Option<String>,
}

async fn create_feedback_vote(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateFeedbackVoteBody>,
) -> ApiResult<impl IntoResponse> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let user = body
        .author_user_id
        .clone()
        .or_else(|| {
            headers
                .get("x-paperclip-user-id")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .ok_or_else(|| ApiError::BadRequest("author_user_id is required".into()))?;
    let row = IssueRepo::new(&state.db)
        .create_feedback_vote(
            issue.company_id,
            id,
            &body.target_type,
            &body.target_id,
            &user,
            &body.vote,
            body.reason.as_deref(),
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("issue.feedback.created", "feedback_vote", row.id)
            .with_company(row.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

// ============================================================================
// Attachments
// ============================================================================

async fn list_attachments(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let attaches = IssueRepo::new(&state.db).list_issue_attachments(id).await?;
    let mut out = Vec::with_capacity(attaches.len());
    for a in attaches {
        let asset = IssueRepo::new(&state.db).get_asset(a.asset_id).await?;
        out.push(serde_json::json!({
            "id": a.id,
            "issue_id": a.issue_id,
            "asset_id": a.asset_id,
            "issue_comment_id": a.issue_comment_id,
            "created_at": a.created_at,
            "asset": asset,
        }));
    }
    Ok(Json(serde_json::to_value(out).unwrap_or_default()))
}

async fn get_attachment(
    State(state): State<AppState>,
    Path(attachment_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let a = IssueRepo::new(&state.db)
        .get_attachment(attachment_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("attachment {attachment_id}")))?;
    let asset = IssueRepo::new(&state.db).get_asset(a.asset_id).await?;
    Ok(Json(serde_json::json!({
        "id": a.id,
        "issue_id": a.issue_id,
        "asset_id": a.asset_id,
        "issue_comment_id": a.issue_comment_id,
        "created_at": a.created_at,
        "asset": asset,
    })))
}

#[derive(Debug, Deserialize)]
struct CreateAttachmentBody {
    provider: String,
    object_key: String,
    content_type: String,
    byte_size: i32,
    sha256: String,
    #[serde(default)]
    original_filename: Option<String>,
    #[serde(default)]
    created_by_user_id: Option<String>,
}

async fn create_attachment(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateAttachmentBody>,
) -> ApiResult<impl IntoResponse> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let user = body.created_by_user_id.clone().or_else(|| {
        headers
            .get("x-paperclip-user-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    let (attach, asset) = IssueRepo::new(&state.db)
        .create_attachment(
            issue.company_id,
            id,
            &body.provider,
            &body.object_key,
            &body.content_type,
            body.byte_size,
            &body.sha256,
            body.original_filename.as_deref(),
            user.as_deref(),
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("issue.attachment.created", "issue_attachment", attach.id)
            .with_company(attach.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": attach.id,
            "issue_id": attach.issue_id,
            "asset_id": attach.asset_id,
            "asset": asset,
        })),
    ))
}

async fn remove_attachment(
    State(state): State<AppState>,
    Path(attachment_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ok = IssueRepo::new(&state.db)
        .delete_attachment(attachment_id)
        .await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("attachment {attachment_id}")))
    }
}

// ============================================================================
// External objects
// ============================================================================

async fn list_external_objects(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let mentions = IssueRepo::new(&state.db)
        .list_external_object_mentions(id)
        .await?;
    let mut out = Vec::with_capacity(mentions.len());
    for m in mentions {
        let object = if let Some(oid) = m.object_id {
            IssueRepo::new(&state.db).get_external_object(oid).await?
        } else {
            None
        };
        out.push(serde_json::json!({
            "mention": m,
            "object": object,
        }));
    }
    Ok(Json(serde_json::to_value(out).unwrap_or_default()))
}

async fn external_object_summary(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let summary = IssueRepo::new(&state.db)
        .external_object_summary(id)
        .await?;
    Ok(Json(serde_json::to_value(summary).unwrap_or_default()))
}

// ============================================================================
// Diagnostics
// ============================================================================

async fn diag_blockers(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let blockers = IssueRepo::new(&state.db).list_blockers(id).await?;
    Ok(Json(serde_json::to_value(blockers).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct WakesQuery {
    #[serde(default = "default_wakes_limit")]
    limit: i64,
}
fn default_wakes_limit() -> i64 {
    20
}

async fn diag_wakes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<WakesQuery>,
) -> ApiResult<Json<Value>> {
    let wakes = IssueRepo::new(&state.db).list_wakes(id, q.limit).await?;
    Ok(Json(serde_json::to_value(wakes).unwrap_or_default()))
}

async fn diag_subtree(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let subtree = IssueRepo::new(&state.db).subtree_diagnostics(id).await?;
    Ok(Json(serde_json::to_value(subtree).unwrap_or_default()))
}

// ============================================================================
// Tree control
// ============================================================================

async fn monitor_check_now(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db)
        .trigger_monitor_check_now(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    state.realtime.publish(
        LiveEvent::new("issue.monitor.check_now", "issue", row.id).with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn scheduled_retry_now(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db)
        .trigger_scheduled_retry_now(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    state.realtime.publish(
        LiveEvent::new("issue.scheduled_retry.retry_now", "issue", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

// ============================================================================
// Company-level
// ============================================================================

#[derive(Debug, Deserialize)]
struct CountQuery {
    #[serde(default)]
    status: Option<String>,
}

async fn count_company_issues(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<CountQuery>,
) -> ApiResult<Json<Value>> {
    let count = IssueRepo::new(&state.db)
        .count_company_issues(company_id, q.status.as_deref())
        .await?;
    Ok(Json(
        json!({ "company_id": company_id, "count": count, "status": q.status }),
    ))
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: i64,
}
fn default_search_limit() -> i64 {
    50
}

async fn search_issues(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<SearchQuery>,
) -> ApiResult<Json<Value>> {
    if query.q.trim().is_empty() {
        return Err(ApiError::BadRequest("q must not be empty".into()));
    }
    let rows = IssueRepo::new(&state.db)
        .search_company_issues(company_id, &query.q, query.limit)
        .await?;
    Ok(Json(json!({
        "company_id": company_id,
        "query": query.q,
        "count": rows.len(),
        "results": rows,
    })))
}
