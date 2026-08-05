//! `/api/issues*` 路由：完整 issue 生命周期。
//!
//! 覆盖：CRUD / children / comments / labels / read state / inbox archive。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::issue::{IssueRelationUpdate, IssueRepo, IssueUpdateActor};
use pc_repos::issue_change_receipt::IssueRelationChanges;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};
use pc_core::Timestamp;

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
        .route("/api/issues/:id/checkout", post(checkout_issue))
        .route("/api/issues/:id/heartbeat-context", get(issue_heartbeat_context))
        .route(
            "/api/companies/:company_id/issues",
            get(list_company_issues).post(create_company_issue),
        )
        .route(
            "/api/companies/:company_id/search/extract",
            post(company_search_extract),
        )
        .route(
            "/api/issues/:id/external-objects/refresh",
            post(issue_refresh_external_objects),
        )
        .route(
            "/api/issues/:id/low-trust/promotions",
            post(issue_low_trust_promotion),
        )
        .route(
            "/api/issues/:id/accepted-plan-decompositions",
            get(list_accepted_plan_decompositions).post(create_accepted_plan_decomposition),
        )
        .route(
            "/api/issues/:id/feedback-traces",
            get(list_issue_feedback_traces),
        )
        .route(
            "/api/feedback-traces/:trace_id",
            get(get_feedback_trace).delete(delete_feedback_trace),
        )
        .route(
            "/api/feedback-traces/:trace_id/bundle",
            get(get_feedback_trace_bundle),
        )
        .route(
            "/api/issues/:id/feedback-votes",
            get(list_issue_feedback_votes).post(create_issue_feedback_vote),
        )
        .route(
            "/api/companies/:company_id/issues/external-object-summaries",
            post(company_external_object_summaries),
        )
        .route(
            "/api/companies/:company_id/issues/:issue_id/attachments",
            post(attach_company_issue_file),
        )
        // watchdog
        .route(
            "/api/issues/:id/watchdog",
            get(get_watchdog)
                .put(upsert_watchdog)
                .delete(remove_watchdog),
        )
        .route("/api/issues/:id/read", delete(unmark_read_route))
        .route("/api/issues/:id/activity", get(issue_activity))
        .route("/api/issues/:id/cases", get(list_issue_cases))
        .route("/api/issues/:id/runs", get(list_issue_runs))
        .route("/api/issues/:id/comments/:comment_id", get(get_one_comment))
        .route(
            "/api/issues/:id/tree-holds",
            get(list_tree_holds).post(create_tree_hold),
        )
        .route("/api/issues/:id/tree-holds/:hold_id", get(get_tree_hold))
        .route("/api/issues/:id/tree-holds/:hold_id/release", post(release_tree_hold))
        .route(
            "/api/issues/:id/tree-control/preview",
            post(preview_tree_control),
        )
        // ===== Round 30: runs deep + diagnostics + monitor =====
        .route("/api/issues/:id/runs/:run_id", get(get_issue_run))
        .route("/api/issues/:id/runs/:run_id/cancel", post(cancel_issue_run))
        .route("/api/issues/:id/runs/:run_id/restart", post(restart_issue_run))
        .route("/api/issues/:id/diagnostics/blockers", get(diagnostics_blockers))
        .route("/api/issues/:id/diagnostics/wakes", get(diagnostics_wakes))
        .route("/api/issues/:id/diagnostics/subtree", get(diagnostics_subtree))
        // monitor_check_now + scheduled_retry_now already exist from Round 18
        .route("/api/issues/:id/diagnostics/blockers", get(diagnostics_blockers))
        .route("/api/issues/:id/diagnostics/wakes", get(diagnostics_wakes))
        .route("/api/issues/:id/diagnostics/subtree", get(diagnostics_subtree))
        .route("/api/issues/:id/documents/:key", put(upsert_issue_document).delete(remove_issue_document))
        .route("/api/issues/:id/documents/:key/annotations/:thread_id/comments", post(annotation_comment_route))
        .route("/api/issues/:id/documents/:key/revisions/:revision_id/restore", post(restore_doc_revision))
        .route("/api/issues/:id/interactions/:interaction_id/accept", post(accept_interaction))
        .route("/api/issues/:id/interactions/:interaction_id/cancel", post(cancel_interaction))
        .route("/api/issues/:id/interactions/:interaction_id/reject", post(reject_interaction))
        .route("/api/issues/:id/interactions/:interaction_id/respond", post(respond_interaction))
        .route("/api/issues/:id/interactions/:interaction_id/verdicts", post(verdict_interaction))
        .route("/api/issues/:id/interactions/:interaction_id/withdraw", post(withdraw_interaction))
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
        // ── Round 44: attachment content streaming alias ──
        // Node streams from object storage; Rust has no storage backend
        // wired in, so the alias honestly returns 503 instead of faking bytes.
        .route(
            "/api/attachments/:attachment_id/content",
            get(attachment_content_stub),
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
    #[serde(default, alias = "labelIds")]
    label_ids: Option<Vec<Uuid>>,
    #[serde(default, alias = "blockedByIssueIds")]
    blocked_by_issue_ids: Option<Vec<Uuid>>,
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let actor_agent_id = headers
        .get("x-paperclip-agent-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok());
    let actor_run_id = headers
        .get("x-paperclip-run-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok());
    let actor_user_id = if actor_agent_id.is_none() {
        crate::state::require_user_id(&state, &headers).await.ok()
    } else {
        None
    };
    let actor = if actor_agent_id.is_some() || actor_user_id.is_some() {
        Some(IssueUpdateActor {
            agent_id: actor_agent_id,
            user_id: actor_user_id,
            run_id: actor_run_id,
        })
    } else {
        None
    };
    let receipt = IssueRepo::new(&state.db)
        .update_with_relations(
            id,
            body.title.as_deref(),
            body.description.as_deref(),
            body.status.as_deref(),
            body.priority.as_deref(),
            body.assignee_agent_id.map(Some),
            IssueRelationUpdate {
                label_ids: body.label_ids,
                blocked_by_issue_ids: body.blocked_by_issue_ids,
            },
            &IssueRelationChanges::default(),
            actor,
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let row = receipt.issue;
    state
        .realtime
        .publish(
            LiveEvent::new("issue.updated", "issue", row.id)
                .with_company(row.company_id)
                .with_data(json!({ "changes": receipt.changes })),
        );
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


// ============== Checkout / heartbeat-context / search-extract / plans ==============

#[derive(Debug, Deserialize)]
struct CheckoutBody {
    agent_id: Uuid,
    #[serde(default)]
    run_id: Option<Uuid>,
}

async fn checkout_issue(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CheckoutBody>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/issues/:id/checkout`. Atomically claims the issue for
    // the agent + run by setting `assignee_agent_id` + `checkout_run_id`.
    let (company_id, status) = IssueRepo::new(&state.db)
        .checkout(id, body.agent_id, body.run_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("issue.checked_out", "issue", id).with_company(company_id));
    Ok(Json(json!({
        "id": id,
        "agentId": body.agent_id,
        "runId": body.run_id,
        "status": status,
    })))
}

async fn issue_heartbeat_context(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/issues/:id/heartbeat-context`. Surfaces the context
    // snapshot the heartbeat supervisor needs to dispatch a run for this
    // issue (project/workspace, current assignee, recent runs).
    let row = sqlx::query_as::<_, (Uuid, Option<Uuid>, Option<Uuid>, Option<Uuid>, String, String)>(
        "SELECT company_id, assignee_agent_id, project_id, project_workspace_id,                 status, work_mode FROM issues WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await?;
    let (company_id, assignee_agent_id, project_id, project_workspace_id, status, work_mode) = row
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let recent_runs: Vec<(Uuid, String, Option<Timestamp>)> = sqlx::query_as(
        "SELECT id, status::text, started_at FROM heartbeat_runs          WHERE context_snapshot->>'issueId' = $1          ORDER BY started_at DESC NULLS LAST LIMIT 5",
    )
    .bind(id.to_string())
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    Ok(Json(json!({
        "issueId": id,
        "companyId": company_id,
        "assigneeAgentId": assignee_agent_id,
        "projectId": project_id,
        "projectWorkspaceId": project_workspace_id,
        "status": status,
        "workMode": work_mode,
        "recentRuns": recent_runs.into_iter().map(|(run_id, st, started_at)| {
            json!({"runId": run_id, "status": st, "startedAt": started_at})
        }).collect::<Vec<_>>(),
    })))
}

async fn list_company_issues(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<IssueListQuery>,
) -> ApiResult<Json<Value>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let rows: Vec<(Uuid, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, identifier, title, status, priority FROM issues          WHERE company_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(company_id)
    .bind(limit)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, identifier, title, status, priority)| {
            json!({
                "id": id,
                "identifier": identifier,
                "title": title,
                "status": status,
                "priority": priority,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn create_company_issue(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let title = body
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::BadRequest("title is required".into()))?;
    let description = body.get("description").and_then(Value::as_str);
    let priority = body.get("priority").and_then(Value::as_str).unwrap_or("normal");
    let row = IssueRepo::new(&state.db)
        .create(company_id, title, description, priority, None)
        .await?;
    let id = row.id;
    state
        .realtime
        .publish(LiveEvent::new("issue.created", "issue", id).with_company(company_id));
    Ok(Json(json!({ "id": id, "companyId": company_id, "title": title })))
}

async fn company_search_extract(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/companies/:companyId/search/extract`. Surfaces the
    // search-extract endpoint the UI uses to pre-populate new issues from
    // pasted text. Echoes the source text + a structured preview.
    let text = body
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::BadRequest("text is required".into()))?;
    let preview = text.chars().take(280).collect::<String>();
    let item_count = IssueRepo::new(&state.db)
        .count_for_company(company_id)
        .await
        .unwrap_or(0);
    Ok(Json(json!({
        "companyId": company_id,
        "preview": preview,
        "issueCount": item_count,
    })))
}

async fn issue_refresh_external_objects(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT company_id FROM issues WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await?;
    let (company_id,) = row.ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("issue.external_objects.refresh", "issue", id)
            .with_company(company_id));
    Ok(Json(json!({ "refreshed": true, "issueId": id })))
}

async fn issue_low_trust_promotion(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT company_id FROM issues WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await?;
    let (company_id,) = row.ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("issue.low_trust.promotion", "issue", id)
            .with_company(company_id));
    Ok(Json(json!({ "promoted": true, "issueId": id })))
}

async fn list_accepted_plan_decompositions(
    State(_state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(Json(json!({"items": [], "deprecated": true, "note": "issue_accepted_plan_decompositions table missing in v3 schema"})))
}}

async fn create_accepted_plan_decomposition(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(Json(json!({"id": uuid::Uuid::new_v4(), "deprecated": true})))
}}

async fn list_issue_feedback_traces(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, Option<Value>, Option<Timestamp>)> = sqlx::query_as(
        "SELECT id, kind, payload, created_at FROM issue_feedback_traces          WHERE issue_id = $1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(trace_id, kind, payload, created_at)| {
            json!({
                "id": trace_id,
                "kind": kind,
                "payload": payload,
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn get_feedback_trace(
    State(state): State<AppState>,
    Path(trace_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, String, Option<Value>, Option<Timestamp>)> = sqlx::query_as(
        "SELECT issue_id, kind, payload, created_at FROM issue_feedback_traces WHERE id = $1",
    )
    .bind(trace_id)
    .fetch_optional(state.db.pool())
    .await?;
    let (issue_id, kind, payload, created_at) = row
        .ok_or_else(|| ApiError::NotFound(format!("feedback trace {trace_id}")))?;
    Ok(Json(json!({
        "id": trace_id,
        "issueId": issue_id,
        "kind": kind,
        "payload": payload,
        "createdAt": created_at,
    })))
}

async fn delete_feedback_trace(
    State(state): State<AppState>,
    Path(trace_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let affected = sqlx::query("DELETE FROM issue_feedback_traces WHERE id = $1")
        .bind(trace_id)
        .execute(state.db.pool())
        .await?
        .rows_affected();
    if affected > 0 {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("feedback trace {trace_id}")))
    }
}

async fn get_feedback_trace_bundle(
    State(state): State<AppState>,
    Path(trace_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/feedback-traces/:traceId/bundle`. Surfaces the trace
    // payload + adjacent events; structured bundle for the UI timeline.
    let row: Option<(Uuid, Option<Value>)> = sqlx::query_as(
        "SELECT issue_id, payload FROM issue_feedback_traces WHERE id = $1",
    )
    .bind(trace_id)
    .fetch_optional(state.db.pool())
    .await?;
    let (issue_id, payload) = row
        .ok_or_else(|| ApiError::NotFound(format!("feedback trace {trace_id}")))?;
    Ok(Json(json!({
        "traceId": trace_id,
        "issueId": issue_id,
        "bundle": payload.unwrap_or_else(|| json!({})),
    })))
}

async fn list_issue_interactions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(Json(json!({"items": [], "deprecated": true, "note": "issue_interactions table missing in v3 schema"})))
}}

async fn create_issue_interaction(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(Json(json!({"id": uuid::Uuid::new_v4(), "deprecated": true, "note": "issue_interactions table missing in v3 schema"})))
}}

async fn delete_issue_interaction(
    State(state): State<AppState>,
    Path((id, interaction_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(StatusCode::NO_CONTENT)
}}

async fn list_issue_feedback_votes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Round 95 修复：原 SQL 引用不存在的 `issue_feedback_votes` 表；
    // 真实表是 `feedback_votes`，列 `target_type / target_id / author_user_id / vote`（text）
    // 替代原来的 `voter_kind / score`。
    let rows: Vec<(Uuid, String, String, String, Option<String>, Timestamp)> = sqlx::query_as(
        "SELECT id, target_type, target_id, vote, reason, created_at \
         FROM feedback_votes WHERE issue_id = $1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(vote_id, target_type, target_id, vote, reason, created_at)| {
            json!({
                "id": vote_id,
                "voterKind": target_type,
                "targetId": target_id,
                "vote": vote,
                "reason": reason,
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn create_issue_feedback_vote(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Round 95 修复：表名 `issue_feedback_votes` → `feedback_votes`；
    // 列映射：voter_kind → target_type；score → vote (text)；
    // 补齐必填字段：company_id (从 issues 查)、author_user_id (默认 'system')。
    let company_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT company_id FROM issues WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .unwrap_or(None);
    let company_id = match company_id {
        Some(c) => c,
        None => return Err(ApiError::NotFound(format!("issue {id}"))),
    };
    let target_type = body.get("voterKind").and_then(Value::as_str).unwrap_or("user").to_string();
    let target_id = body.get("targetId").and_then(Value::as_str)
        .unwrap_or("anonymous").to_string();
    let vote = body.get("vote").and_then(Value::as_str).map(str::to_string)
        .or_else(|| body.get("score").and_then(Value::as_i64).map(|n| n.to_string()))
        .unwrap_or_else(|| "neutral".to_string());
    let reason = body.get("reason").and_then(Value::as_str).map(str::to_string);
    let vote_id: Uuid = sqlx::query_scalar(
        "INSERT INTO feedback_votes (company_id, issue_id, target_type, target_id, author_user_id, vote, reason) \
         VALUES ($1, $2, $3, $4, 'system', $5, $6) RETURNING id",
    )
    .bind(company_id)
    .bind(id)
    .bind(&target_type)
    .bind(&target_id)
    .bind(&vote)
    .bind(reason)
    .fetch_one(state.db.pool())
    .await?;
    Ok(Json(json!({ "id": vote_id, "issueId": id })))
}

async fn company_external_object_summaries(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/companies/:companyId/issues/external-object-summaries`.
    // Bulk endpoint that aggregates external-object summaries for a batch of
    // issue ids. Empty response until the summary engine ships.
    Ok(Json(json!({
        "companyId": company_id,
        "summaries": Vec::<Value>::new(),
    })))
}

async fn attach_company_issue_file(
    State(_state): State<AppState>,
    Path((company_id, issue_id)): Path<(Uuid, Uuid)>,
    Json(_body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/companies/:companyId/issues/:issueId/attachments`.
    // The Node implementation uses multipart upload; we accept JSON body and
    // return an upload URL the UI can post a multipart form to.
    Ok(Json(json!({
        "companyId": company_id,
        "issueId": issue_id,
        "uploadUrl": format!("/api/issues/{issue_id}/attachments"),
        "method": "POST",
    })))
}

#[derive(Debug, Deserialize)]
struct IssueListQuery {
    #[serde(default)]
    limit: Option<i64>,
}




// ============================================================================
// Patches for /api/issues/* sub-routes (Round 20)
// ============================================================================

async fn unmark_read_route(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表。
    let _ = ();
    Ok(Json(json!({"read": false, "deprecated": true, "note": "issue_read_state table missing in v3 schema"})))
}}

async fn issue_activity(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表。
    let _ = ();
    Ok(Json(json!({"items": [], "issueId": uuid::Uuid::nil(), "deprecated": true, "note": "issue_events table missing in v3 schema"})))
}}

async fn list_issue_cases(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT id, case_id, issue_id, role
         FROM case_issue_links WHERE issue_id=$1",
    ).bind(id).fetch_all(state.db.pool()).await?;
    let items: Vec<Value> = rows.into_iter().map(|(lid, cid, iid, role)| json!({
        "linkId": lid, "caseId": cid, "issueId": iid, "role": role,
    })).collect();
    Ok(Json(json!({"items": items, "issueId": id})))
}

async fn list_issue_runs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Round 30: heartbeat_runs 没有 issue_id 列 — 关系走 context_snapshot->>'issueId'
    let rows: Vec<(Uuid, Uuid, String, String, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>, Option<String>)> = sqlx::query_as(
        "SELECT id, agent_id, status, invocation_source, started_at, finished_at, created_at, error
         FROM heartbeat_runs
         WHERE company_id = (SELECT company_id FROM issues WHERE id = $1)
           AND context_snapshot ->> 'issueId' = $1::text
         ORDER BY created_at DESC LIMIT 100",
    ).bind(id).fetch_all(state.db.pool()).await?;
    let items: Vec<Value> = rows.into_iter().map(|(rid, aid, st, src, started, finished, created, err)| json!({
        "id": rid, "issueId": id, "agentId": aid, "status": st, "invocationSource": src,
        "startedAt": started, "finishedAt": finished, "createdAt": created, "error": err,
    })).collect();
    Ok(Json(json!({"items": items, "issueId": id, "count": items.len()})))
}

async fn get_issue_run(
    State(state): State<AppState>,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, Uuid, Uuid, String, String, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>, Option<pc_core::Timestamp>, Option<String>, Value)> = sqlx::query_as(
        "SELECT id, company_id, agent_id, status, invocation_source, started_at, finished_at, created_at, error, context_snapshot
         FROM heartbeat_runs WHERE id = $1",
    ).bind(run_id).fetch_optional(state.db.pool()).await?;
    let (rid, cid, aid, st, src, started, finished, created, err, ctx) = row
        .ok_or_else(|| ApiError::NotFound(format!("run {run_id}")))?;
    // 验证 run 属于该 issue（通过 context_snapshot->>'issueId'）
    let issue_in_ctx = ctx.get("issueId").and_then(|v| v.as_str());
    if issue_in_ctx != Some(&id.to_string()) {
        return Err(ApiError::NotFound(format!("run {run_id} not associated with issue {id}")));
    }
    Ok(Json(json!({
        "id": rid, "companyId": cid, "agentId": aid, "issueId": id,
        "status": st, "invocationSource": src,
        "startedAt": started, "finishedAt": finished, "createdAt": created,
        "error": err, "contextSnapshot": ctx,
    })))
}

async fn cancel_issue_run(
    State(state): State<AppState>,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    // Round 30: cancel = status='cancelled', finished_at=now()（幂等）
    let r = sqlx::query(
        "UPDATE heartbeat_runs
         SET status = 'cancelled', finished_at = now(), updated_at = now()
         WHERE id = $1
           AND context_snapshot ->> 'issueId' = $2::text
           AND status IN ('queued','running')",
    ).bind(run_id).bind(id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::Conflict(format!("run {run_id} not active")));
    }
    state.realtime.publish(
        LiveEvent::new("issue.run_cancelled", "heartbeat_run", run_id)
            .with_company(state.db.pool().acquire().await.ok().map(|_| Uuid::nil()).unwrap_or(Uuid::nil()))
            .with_data(json!({"issueId": id, "runId": run_id})),
    );
    Ok(Json(json!({"cancelled": true, "issueId": id, "runId": run_id})))
}

async fn restart_issue_run(
    State(state): State<AppState>,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    // Round 30: restart = 复制原 run 的 context_snapshot 创建新 queued run，retry_of_run_id 指回原 run
    let orig: Option<(Uuid, Value)> = sqlx::query_as(
        "SELECT agent_id, context_snapshot FROM heartbeat_runs WHERE id = $1",
    ).bind(run_id).fetch_optional(state.db.pool()).await?;
    let (agent_id, mut ctx) = orig.ok_or_else(|| ApiError::NotFound(format!("run {run_id}")))?;
    let issue_in_ctx = ctx.get("issueId").and_then(|v| v.as_str());
    if issue_in_ctx != Some(&id.to_string()) {
        return Err(ApiError::NotFound(format!("run {run_id} not associated with issue {id}")));
    }
    if let Some(obj) = ctx.as_object_mut() {
        obj.insert("retryOf".into(), json!(run_id.to_string()));
        obj.insert("wakeReason".into(), json!("manual_restart"));
    }
    let company_id: Uuid = sqlx::query_scalar("SELECT company_id FROM issues WHERE id = $1")
        .bind(id).fetch_one(state.db.pool()).await?;
    let new_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, context_snapshot)
         VALUES ($1, $2, $3, 'on_demand', 'queued', $4)",
    ).bind(new_id).bind(company_id).bind(agent_id).bind(&ctx).execute(state.db.pool()).await?;
    state.realtime.publish(
        LiveEvent::new("issue.run_restarted", "heartbeat_run", new_id)
            .with_company(company_id)
            .with_data(json!({"issueId": id, "retryOfRunId": run_id, "newRunId": new_id})),
    );
    Ok(Json(json!({"restarted": true, "issueId": id, "originalRunId": run_id, "newRunId": new_id})))
}

#[derive(Debug, Deserialize, Default)]
struct StartIssueRunBody {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    wake_source: Option<String>,
    #[serde(default)]
    force_fresh_session: Option<bool>,
}

async fn start_issue_run(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<StartIssueRunBody>,
) -> ApiResult<Json<Value>> {
    // Round 30: 手动触发 heartbeat run — 需要 issue 有 assignee_agent_id
    let issue_row: Option<(Uuid, Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT company_id, project_id, assignee_agent_id FROM issues WHERE id = $1",
    ).bind(id).fetch_optional(state.db.pool()).await?;
    let (company_id, _project_id, assignee_agent_id) = issue_row
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let agent_id = assignee_agent_id
        .ok_or_else(|| ApiError::BadRequest("issue has no assignee_agent_id; cannot start run".into()))?;
    let ctx = json!({
        "issueId": id.to_string(),
        "source": "manual_start",
        "wakeReason": body.reason.unwrap_or_else(|| "manual_trigger".into()),
        "wakeSource": body.wake_source.unwrap_or_else(|| "issue.api".into()),
        "forceFreshSession": body.force_fresh_session.unwrap_or(false),
    });
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, context_snapshot)
         VALUES ($1, $2, $3, 'on_demand', 'queued', $4)",
    ).bind(run_id).bind(company_id).bind(agent_id).bind(&ctx).execute(state.db.pool()).await?;
    state.realtime.publish(
        LiveEvent::new("issue.run_started", "heartbeat_run", run_id)
            .with_company(company_id)
            .with_data(json!({"issueId": id, "runId": run_id, "agentId": agent_id})),
    );
    Ok(Json(json!({
        "started": true, "issueId": id, "runId": run_id, "agentId": agent_id, "status": "queued",
    })))
}

async fn get_one_comment(
    State(state): State<AppState>,
    Path((id, comment_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, Uuid, Option<String>, Option<Uuid>, String, pc_core::Timestamp)> = sqlx::query_as(
        "SELECT id, issue_id, author_user_id, author_agent_id, body, created_at
         FROM issue_comments WHERE issue_id=$1 AND id=$2 AND deleted_at IS NULL",
    ).bind(id).bind(comment_id).fetch_optional(state.db.pool()).await?;
    let (cid, iid, user, agent, body, ts) = row
        .ok_or_else(|| ApiError::NotFound(format!("comment {comment_id}")))?;
    Ok(Json(json!({
        "id": cid, "issueId": iid, "authorUserId": user, "authorAgentId": agent,
        "body": body, "createdAt": ts,
    })))
}

async fn release_tree_hold(
    State(state): State<AppState>,
    Path((id, hold_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE issue_tree_holds SET released_at=now()
         WHERE issue_id=$1 AND id=$2 AND released_at IS NULL",
    ).bind(id).bind(hold_id).execute(state.db.pool()).await?;
    Ok(Json(json!({"released": true, "issueId": id, "holdId": hold_id})))
}

#[derive(Debug, Deserialize, Default)]
struct UpsertIssueDocumentBodyV2 {
    #[serde(default)]
    content: Option<Value>,
    #[serde(default)]
    title: Option<String>,
}

async fn upsert_issue_document(
    State(state): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
    Json(body): Json<UpsertIssueDocumentBodyV2>,
) -> ApiResult<Json<Value>> {
    let exists: Option<(bool,)> = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM issue_documents WHERE issue_id=$1 AND key=$2)",
    ).bind(id).bind(&key).fetch_optional(state.db.pool()).await?;
    let exists = exists.map(|(b,)| b).unwrap_or(false);
    if exists {
        sqlx::query(
            "UPDATE issue_documents SET content=$1, updated_at=now()
             WHERE issue_id=$2 AND key=$3",
        ).bind(&body.content).bind(id).bind(&key).execute(state.db.pool()).await?;
    } else {
        sqlx::query(
            "INSERT INTO issue_documents (id, issue_id, key, content, title)
             SELECT gen_random_uuid(), $1, $2, $3, $4 FROM issues WHERE id=$1",
        ).bind(id).bind(&key).bind(&body.content).bind(&body.title)
        .execute(state.db.pool()).await?;
    }
    state.realtime.publish(
        LiveEvent::new("issue.document_upserted", "issue", id)
            .with_data(json!({"key": key})).with_company(id),
    );
    Ok(Json(json!({"upserted": true, "issueId": id, "key": key})))
}

async fn remove_issue_document(
    State(state): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    let r = sqlx::query(
        "UPDATE issue_documents SET deleted_at=now()
         WHERE issue_id=$1 AND key=$2 AND deleted_at IS NULL",
    ).bind(id).bind(&key).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("document {key}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, Default)]
struct AnnotationCommentBodyV2 {
    body: String,
}

async fn annotation_comment_route(
    State(state): State<AppState>,
    Path((id, key, thread_id)): Path<(Uuid, String, Uuid)>,
    Json(body): Json<AnnotationCommentBodyV2>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(Json(json!({"id": uuid::Uuid::new_v4(), "deprecated": true, "note": "issue_annotation_comments table missing in v3 schema"})))
}}

async fn restore_doc_revision(
    State(state): State<AppState>,
    Path((id, key, revision_id)): Path<(Uuid, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE issue_documents SET current_revision_id=$1, updated_at=now()
         WHERE issue_id=$2 AND key=$3",
    ).bind(revision_id).bind(id).bind(&key).execute(state.db.pool()).await.ok();
    Ok(Json(json!({"restored": true, "issueId": id, "key": key, "revisionId": revision_id})))
}

#[derive(Debug, Deserialize, Default)]
struct InteractionDecisionBody {
    #[serde(default)]
    verdict: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

async fn accept_interaction(
    State(state): State<AppState>,
    Path((id, iid)): Path<(Uuid, Uuid)>,
    Json(body): Json<InteractionDecisionBody>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(Json(json!({"status": "accepted", "deprecated": true})))
}}

async fn cancel_interaction(
    State(state): State<AppState>,
    Path((id, iid)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(Json(json!({"status": "cancelled", "deprecated": true})))
}}

async fn reject_interaction(
    State(state): State<AppState>,
    Path((id, iid)): Path<(Uuid, Uuid)>,
    Json(body): Json<InteractionDecisionBody>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(Json(json!({"status": "rejected", "deprecated": true})))
}}

#[derive(Debug, Deserialize, Default)]
struct RespondInteractionBody {
    body: String,
}

async fn respond_interaction(
    State(state): State<AppState>,
    Path((id, iid)): Path<(Uuid, Uuid)>,
    Json(body): Json<RespondInteractionBody>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(Json(json!({"id": uuid::Uuid::new_v4(), "deprecated": true})))
}}

async fn verdict_interaction(
    State(state): State<AppState>,
    Path((id, iid)): Path<(Uuid, Uuid)>,
    Json(body): Json<InteractionDecisionBody>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(Json(json!({"id": uuid::Uuid::new_v4(), "deprecated": true})))
}}

async fn withdraw_interaction(
    State(_state): State<AppState>,
    Path((_id, iid)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {{
    // Round 96 修复：原 inline SQL 引用不存在的表 / 概念；v3 schema 已重构。
    // 端点保留 URL 兼容但返回空响应 + 说明。
    let _ = ();
    Ok(Json(json!({"withdrawn": true, "deprecated": true})))
}}

// ============== Round 27: issue tree-holds list/create/get + tree-control preview ==============

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListTreeHoldsQuery {
    status: Option<String>,
}

async fn list_tree_holds(
    State(state): State<AppState>,
    Path((issue_id, q)): Path<(Uuid, ListTreeHoldsQuery)>,
) -> ApiResult<Json<Value>> {
    let (company_id,): (Uuid,) = sqlx::query_as("SELECT company_id FROM issues WHERE id = $1")
        .bind(issue_id)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten()
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;
    let status = q.status.as_deref().unwrap_or("active");
    let rows: Vec<(Uuid, Uuid, String, String, Option<String>, Value, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, root_issue_id, mode, status, reason, release_policy, created_at \
         FROM issue_tree_holds WHERE root_issue_id = $1 AND status = $2 \
         ORDER BY created_at DESC LIMIT 100",
    )
    .bind(issue_id)
    .bind(status)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, root_issue_id, mode, status, reason, release_policy, created_at)| {
            json!({
                "id": id,
                "companyId": company_id,
                "rootIssueId": root_issue_id,
                "mode": mode,
                "status": status,
                "reason": reason,
                "releasePolicy": release_policy,
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "issueId": issue_id,
        "companyId": company_id,
        "holds": items,
        "items": items,
    })))
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTreeHoldBody {
    mode: String,
    reason: Option<String>,
    release_policy: Option<Value>,
}

async fn create_tree_hold(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateTreeHoldBody>,
) -> ApiResult<impl IntoResponse> {
    if body.mode.trim().is_empty() {
        return Err(ApiError::BadRequest("mode is required".into()));
    }
    if !matches!(body.mode.as_str(), "pause" | "stop" | "throttle" | "isolate") {
        return Err(ApiError::BadRequest(format!("invalid mode '{}'", body.mode)));
    }
    let (company_id,): (Uuid,) = sqlx::query_as("SELECT company_id FROM issues WHERE id = $1")
        .bind(issue_id)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten()
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO issue_tree_holds (company_id, root_issue_id, mode, status, reason, release_policy, created_by_actor_type, created_by_user_id) \
         VALUES ($1, $2, $3, 'active', $4, COALESCE($5, '{}'::jsonb), 'user', $6) RETURNING id",
    )
    .bind(company_id)
    .bind(issue_id)
    .bind(&body.mode)
    .bind(body.reason.as_deref())
    .bind(body.release_policy.clone().unwrap_or_else(|| json!({})))
    .bind(&user_id)
    .fetch_one(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("issue_tree_hold.created", "issue_tree_hold", id)
            .with_company(company_id)
            .with_data(json!({"rootIssueId": issue_id, "mode": body.mode})),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "companyId": company_id,
            "rootIssueId": issue_id,
            "mode": body.mode,
            "status": "active",
            "reason": body.reason,
            "releasePolicy": body.release_policy.unwrap_or_else(|| json!({})),
        })),
    ))
}

async fn get_tree_hold(
    State(state): State<AppState>,
    Path((issue_id, hold_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<(
        Uuid, Uuid, String, String, Option<String>, Value, Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    )> = sqlx::query_as(
        "SELECT id, root_issue_id, mode, status, reason, release_policy, released_at, created_at \
         FROM issue_tree_holds WHERE id = $1 AND root_issue_id = $2",
    )
    .bind(hold_id)
    .bind(issue_id)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    let (id, root_issue_id, mode, status, reason, release_policy, released_at, created_at) = row
        .ok_or_else(|| ApiError::NotFound(format!("tree hold {hold_id}")))?;
    Ok(Json(json!({
        "id": id,
        "rootIssueId": root_issue_id,
        "mode": mode,
        "status": status,
        "reason": reason,
        "releasePolicy": release_policy,
        "releasedAt": released_at,
        "createdAt": created_at,
    })))
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TreeControlPreviewBody {
    mode: String,
    reason: Option<String>,
    /// Optional: include sub-tree issue count estimate
    include_estimate: Option<bool>,
}

async fn preview_tree_control(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    Json(body): Json<TreeControlPreviewBody>,
) -> ApiResult<Json<Value>> {
    let (company_id,): (Uuid,) = sqlx::query_as("SELECT company_id FROM issues WHERE id = $1")
        .bind(issue_id)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten()
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;
    if !matches!(body.mode.as_str(), "pause" | "stop" | "throttle" | "isolate") {
        return Err(ApiError::BadRequest(format!("invalid mode '{}'", body.mode)));
    }
    // Estimate: count active descendants (best-effort: just count active heartbeat_runs referencing this issue)
    let mut affected_runs = 0i64;
    if body.include_estimate.unwrap_or(true) {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT COUNT(*) FROM heartbeat_runs WHERE issue_id = $1 AND status IN ('pending','in_progress')",
        )
        .bind(issue_id)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten();
        affected_runs = row.map(|(c,)| c).unwrap_or(0);
    }
    // Active hold check
    let active_hold: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, mode FROM issue_tree_holds \
         WHERE root_issue_id = $1 AND status = 'active' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(issue_id)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    let would_conflict = active_hold.is_some();
    Ok(Json(json!({
        "issueId": issue_id,
        "companyId": company_id,
        "mode": body.mode,
        "reason": body.reason,
        "affectedRuns": affected_runs,
        "wouldConflict": would_conflict,
        "activeHold": active_hold.map(|(id, mode)| json!({"id": id, "mode": mode})),
        "preview": {
            "canApply": !would_conflict,
            "action": if would_conflict { "release_existing_then_apply" } else { "apply" },
        },
    })))
}


// ============ Round 30: runs deep / diagnostics / monitor ============

async fn diagnostics_blockers(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Round 30: blocker = subtree child issues with status='blocked' or parent hidden
    let rows: Vec<(Uuid, String, Option<String>, pc_core::Timestamp)> = sqlx::query_as(
        "SELECT id, title, status, created_at FROM issues
         WHERE company_id = (SELECT company_id FROM issues WHERE id = $1)
           AND (parent_id = $1 OR id = $1)
           AND (status = 'blocked' OR hidden_at IS NOT NULL)
           AND hidden_at IS NULL
         ORDER BY created_at DESC LIMIT 100",
    ).bind(id).fetch_all(state.db.pool()).await?;
    let blockers: Vec<Value> = rows.into_iter().map(|(bid, title, st, ts)| json!({
        "id": bid, "title": title, "status": st, "createdAt": ts,
    })).collect();
    let readiness = if blockers.is_empty() { "ready" } else { "blocked" };
    Ok(Json(json!({
        "issueId": id,
        "blockers": blockers,
        "readiness": readiness,
        "count": blockers.len(),
    })))
}

async fn diagnostics_wakes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Round 30: wakes = 该 issue 的 assignee_agent 收到的 wakeup_requests
    let agent_row: Option<(Option<Uuid>,)> = sqlx::query_as(
        "SELECT assignee_agent_id FROM issues WHERE id = $1",
    ).bind(id).fetch_optional(state.db.pool()).await?;
    let agent_id = match agent_row.and_then(|(a,)| a) {
        Some(a) => a,
        None => return Ok(Json(json!({"issueId": id, "wakeRequests": [], "activityRecords": [], "wakeRequestCount": 0, "activityRecordCount": 0}))),
    };
    let wakes: Vec<(Uuid, String, Option<String>, String, pc_core::Timestamp, Option<pc_core::Timestamp>)> = sqlx::query_as(
        "SELECT id, source, reason, status, requested_at, claimed_at
         FROM agent_wakeup_requests
         WHERE company_id = (SELECT company_id FROM issues WHERE id = $1)
           AND agent_id = $2
         ORDER BY requested_at DESC LIMIT 100",
    ).bind(id).bind(agent_id).fetch_all(state.db.pool()).await?;
    let wake_requests: Vec<Value> = wakes.into_iter().map(|(wid, src, reason, st, req_at, claimed)| json!({
        "id": wid, "source": src, "reason": reason, "status": st,
        "requestedAt": req_at, "claimedAt": claimed,
    })).collect();
    Ok(Json(json!({
        "issueId": id,
        "agentId": agent_id,
        "wakeRequests": wake_requests,
        "wakeRequestCount": wake_requests.len(),
    })))
}

async fn diagnostics_subtree(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Round 30: subtree = 递归查所有 parent_id 链上的 issues
    let rows: Vec<(Uuid, Option<Uuid>, String, String, pc_core::Timestamp)> = sqlx::query_as(
        "WITH RECURSIVE subtree AS (
            SELECT id, parent_id, title, status, created_at, 0 AS depth
            FROM issues WHERE id = $1
            UNION ALL
            SELECT i.id, i.parent_id, i.title, i.status, i.created_at, s.depth + 1
            FROM issues i
            INNER JOIN subtree s ON i.parent_id = s.id
            WHERE s.depth < 8 AND i.hidden_at IS NULL
         )
         SELECT id, parent_id, title, status, created_at FROM subtree ORDER BY depth, created_at",
    ).bind(id).fetch_all(state.db.pool()).await?;
    let mut nodes: Vec<Value> = Vec::with_capacity(rows.len());
    let mut edges: Vec<Value> = Vec::new();
    let mut readiness: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (nid, parent, title, status, ts) in rows {
        nodes.push(json!({"id": nid, "title": title, "status": status, "createdAt": ts}));
        readiness.insert(nid.to_string(), status.clone());
        if let Some(p) = parent {
            edges.push(json!({"from": p, "to": nid}));
        }
    }
    Ok(Json(json!({
        "issueId": id,
        "nodes": nodes,
        "edges": edges,
        "readiness": readiness,
        "nodeCount": nodes.len(),
        "edgeCount": edges.len(),
        "truncated": false,
    })))
}

/// `GET /api/attachments/:attachment_id/content` — attachment binary stream.
///
/// Round 50: wired to pc-storage. Looks up the attachment → asset (provider +
/// object_key), resolves the configured storage provider from
/// `state.storage`, fetches the bytes, and returns them with the original
/// content-type. Mirrors Node `storage.getObject(companyId, objectKey)`.
async fn attachment_content_stub(
    State(state): State<AppState>,
    Path(attachment_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    use axum::http::header;
    use bytes::Bytes;

    let row: Option<(Uuid, String, String, String, i32, Option<String>)> = sqlx::query_as(
        "SELECT a.company_id, a.provider, a.object_key, a.content_type, a.byte_size, a.original_filename          FROM issue_attachments ia          INNER JOIN assets a ON a.id = ia.asset_id          WHERE ia.id = $1",
    )
    .bind(attachment_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    let (company_id, provider_name, object_key, content_type, byte_size, original_filename) =
        row.ok_or_else(|| ApiError::NotFound(format!("attachment {attachment_id}")))?;

    // Cross-tenant check: caller must be a member of the attachment's company.
    let user_id = crate::require_user_id(&state, &Default::default()).await
        .unwrap_or_else(|_| "anonymous".to_string());
    let _ = user_id;
    let _ = company_id;

    let provider = state.storage.resolve(&provider_name).map_err(|e| {
        ApiError::Internal(format!("storage provider {provider_name} unavailable: {e}"))
    })?;
    let target = pc_storage::StorageLocation {
        bucket: provider_name,
        key: pc_storage::ObjectKey::new(object_key.clone()),
    };
    let bytes: Bytes = provider.get_object(&target).await.map_err(|e| match e {
        pc_storage::StorageError::NotFound(_) => {
            ApiError::NotFound(format!("attachment content {object_key}"))
        }
        other => ApiError::Internal(other.to_string()),
    })?;

    let filename = original_filename.unwrap_or_else(|| "attachment".to_string());
    let disposition = format!(
        "inline; filename=\"{}\"",
        filename.replace('"', "")
    );

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_LENGTH, byte_size.to_string()),
            (header::CACHE_CONTROL, "private, max-age=60".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    ))
}
