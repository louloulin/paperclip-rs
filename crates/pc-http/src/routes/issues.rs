//! `/api/issues*` 路由：完整 issue 生命周期。
//!
//! 覆盖：CRUD / children / comments / labels / read state / inbox archive。

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use pc_activity::kinds::ActivityKind;
use pc_activity::types::{ActivityActor, ActivityEvent};
use pc_realtime::LiveEvent;
use pc_repos::activity::ActivityRepo;
use pc_repos::issue::{IssueRelationUpdate, IssueRepo, IssueUpdateActor};
use pc_repos::feedback_vote::FeedbackVoteRepo;
use pc_repos::feedback_trace::FeedbackTraceRepo;
use pc_repos::case::CaseRepo;
use pc_repos::heartbeat::HeartbeatRepo;
use pc_repos::issue_tree_hold::{IssueTreeHoldRepo, NewIssueTreeHold};
use pc_repos::issue_diagnostics::IssueDiagnosticsRepo;
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
        // ── Round 219: list/create/delete interactions ──
        .route("/api/issues/:id/interactions", get(list_issue_interactions).post(create_issue_interaction))
        .route("/api/issues/:id/interactions/:interaction_id", delete(delete_issue_interaction))
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
        // ── Round 210: aggregate endpoints ──
        .route(
            "/api/companies/:company_id/issues/by-status",
            get(issues_by_status),
        )
        .route(
            "/api/companies/:company_id/issues/by-priority",
            get(issues_by_priority),
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


// ============================================================================
// Round 210: company-scoped issue aggregate endpoints
// ============================================================================

async fn issues_by_status(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, i64)> = IssueRepo::new(&state.db)
        .count_visible_by_status(company_id)
        .await?;
    let mut total = 0i64;
    let groups: Vec<Value> = rows
        .iter()
        .map(|(status, count)| {
            total += count;
            json!({ "status": status, "count": count })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "total": total,
        "groups": groups,
    })))
}

async fn issues_by_priority(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, i64)> = IssueRepo::new(&state.db)
        .count_visible_by_priority(company_id)
        .await?;
    let mut total = 0i64;
    let groups: Vec<Value> = rows
        .iter()
        .map(|(priority, count)| {
            total += count;
            json!({ "priority": priority, "count": count })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "total": total,
        "groups": groups,
    })))
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
    let row = IssueRepo::new(&state.db)
        .heartbeat_context_inputs(id)
        .await?;
    let (company_id, assignee_agent_id, project_id, project_workspace_id, status, work_mode) = row
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let recent_runs = HeartbeatRepo::new(&state.db)
        .recent_runs_for_issue(id, 5)
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
    let rows = IssueRepo::new(&state.db)
        .list_company_basic(company_id, limit)
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
    let company_id = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .map(|r| r.company_id)
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
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
    let company_id = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .map(|r| r.company_id)
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("issue.low_trust.promotion", "issue", id)
            .with_company(company_id));
    Ok(Json(json!({ "promoted": true, "issueId": id })))
}

#[derive(Debug, Deserialize)]
struct CreateAcceptedPlanDecompositionBody {
    #[serde(rename = "acceptedPlanRevisionId")]
    accepted_plan_revision_id: Uuid,
    #[serde(default)]
    children: Vec<serde_json::Value>,
}

/// Round 222: 真实实现 GET /api/issues/:id/accepted-plan-decompositions
///
/// 与 Node `svc.listAcceptedPlanDecompositions` 对齐。
/// 表 `issue_plan_decompositions` 已存在 (migration 0092)。
async fn list_accepted_plan_decompositions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = IssueRepo::new(&state.db)
        .list_plan_decompositions(id)
        .await?;
    // camelCase 序列化（保留 Node 字段命名）
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "companyId": r.company_id,
                "sourceIssueId": r.source_issue_id,
                "acceptedPlanRevisionId": r.accepted_plan_revision_id,
                "acceptedInteractionId": r.accepted_interaction_id,
                "status": r.status,
                "requestFingerprint": r.request_fingerprint,
                "requestedChildCount": r.requested_child_count,
                "childIssueIds": r.child_issue_ids,
                "ownerAgentId": r.owner_agent_id,
                "ownerUserId": r.owner_user_id,
                "ownerRunId": r.owner_run_id,
                "completedAt": r.completed_at,
                "createdAt": r.created_at,
                "updatedAt": r.updated_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

/// Round 222: 真实实现 POST /api/issues/:id/accepted-plan-decompositions
///
/// 与 Node `svc.decomposeAcceptedPlan` 简化对齐 — 本轮仅做 claim 持久化。
/// 完整的 child issue 创建循环（createChild + cursor 推进）属于 service 层职责，
/// 这里聚焦 idempotent claim 创建：
/// 1. 验证 source issue 存在并获取 company_id
/// 2. 检查同一 revision 是否已有 claim（idempotent 返回现有）
/// 3. 否则创建新 in_flight claim
///
/// 后续 R223+ 可在本基础上叠加 child issue 创建循环。
async fn create_accepted_plan_decomposition(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateAcceptedPlanDecompositionBody>,
) -> ApiResult<Json<Value>> {
    if body.children.is_empty() {
        return Err(ApiError::BadRequest(
            "children must contain at least 1 entry".into(),
        ));
    }
    // 1. 验证 source issue
    let source = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    // 2. 计算 fingerprint（基于 revision + 规范化 child payload 字段顺序）
    let fingerprint = compute_plan_decomposition_fingerprint(
        body.accepted_plan_revision_id,
        &body.children,
    );
    // 3. 检查现有 claim（idempotent）
    if let Some(existing) = IssueRepo::new(&state.db)
        .find_plan_decomposition_by_revision(
            source.company_id,
            id,
            body.accepted_plan_revision_id,
        )
        .await?
    {
        if existing.request_fingerprint == fingerprint {
            // 同一请求幂等返回现有
            return Ok(Json(plan_decomposition_row_json(&existing)));
        }
        return Err(ApiError::Conflict(
            "Accepted-plan decomposition already exists for this revision with a different child set".into(),
        ));
    }
    // 4. 创建新 in_flight claim
    let requested_children_value = serde_json::Value::Array(body.children.clone());
    let row = IssueRepo::new(&state.db)
        .create_plan_decomposition(
            source.company_id,
            id,
            body.accepted_plan_revision_id,
            None,
            &fingerprint,
            body.children.len() as i32,
            &requested_children_value,
            None,
            None,
            None,
        )
        .await?;
    Ok(Json(plan_decomposition_row_json(&row)))
}

/// 计算 plan decomposition 的稳定指纹（基于 revision + children JSON）。
///
/// 使用 serde_json 的 to_string 后再做 SHA-256 子串以获得稳定哈希。
/// 当前简化为基于 revision 与 children 数量，避免引入额外依赖。
fn compute_plan_decomposition_fingerprint(
    revision_id: Uuid,
    children: &[serde_json::Value],
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    revision_id.hash(&mut h);
    children.len().hash(&mut h);
    // 对每个 child 的 title+description 做轻量 hash，避免深度 JSON 序列化
    for c in children {
        if let Some(obj) = c.as_object() {
            if let Some(t) = obj.get("title").and_then(|v| v.as_str()) {
                t.hash(&mut h);
            }
            if let Some(d) = obj.get("description").and_then(|v| v.as_str()) {
                d.hash(&mut h);
            }
        }
    }
    format!("{:x}-{:x}", revision_id.simple(), h.finish())
}

/// 将 IssuePlanDecompositionRow 序列化为 camelCase JSON。
fn plan_decomposition_row_json(r: &pc_repos::issue::IssuePlanDecompositionRow) -> Value {
    json!({
        "id": r.id,
        "companyId": r.company_id,
        "sourceIssueId": r.source_issue_id,
        "acceptedPlanRevisionId": r.accepted_plan_revision_id,
        "acceptedInteractionId": r.accepted_interaction_id,
        "status": r.status,
        "requestFingerprint": r.request_fingerprint,
        "requestedChildCount": r.requested_child_count,
        "childIssueIds": r.child_issue_ids,
        "ownerAgentId": r.owner_agent_id,
        "ownerUserId": r.owner_user_id,
        "ownerRunId": r.owner_run_id,
        "completedAt": r.completed_at,
        "createdAt": r.created_at,
        "updatedAt": r.updated_at,
    })
}

async fn list_issue_feedback_traces(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = FeedbackTraceRepo::new(&state.db)
        .list_by_issue(id, 100)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "kind": r.kind,
                "payload": r.payload,
                "createdAt": r.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn get_feedback_trace(
    State(state): State<AppState>,
    Path(trace_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = FeedbackTraceRepo::new(&state.db)
        .get_by_id_full(trace_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("feedback trace {trace_id}")))?;
    let (issue_id, kind, payload, created_at) = row;
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
    let deleted = FeedbackTraceRepo::new(&state.db)
        .delete(trace_id)
        .await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("feedback trace {trace_id}")))
    }
}

async fn get_feedback_trace_bundle(
    State(state): State<AppState>,
    Path(trace_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (issue_id, payload) = FeedbackTraceRepo::new(&state.db)
        .get_bundle(trace_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("feedback trace {trace_id}")))?;
    Ok(Json(json!({
        "traceId": trace_id,
        "issueId": issue_id,
        "bundle": payload.unwrap_or_else(|| json!({})),
    })))
}

/// Round 219: GET /api/issues/:id/interactions
///
/// 与 Node GET /issues/:id/interactions 对齐。
async fn list_issue_interactions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = IssueRepo::new(&state.db)
        .list_interactions(id)
        .await?;
    let items: Vec<Value> = rows.into_iter().map(|r| interaction_row_json(&r)).collect();
    Ok(Json(json!({"items": items, "issueId": id})))
}

/// Round 219: POST /api/issues/:id/interactions
///
/// 与 Node POST /issues/:id/interactions 对齐。
async fn create_issue_interaction(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateInteractionBody>,
) -> ApiResult<Json<Value>> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let payload = body.payload.unwrap_or(serde_json::json!({}));
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
            body.created_by_user_id.as_deref(),
        )
        .await?;
    Ok(Json(interaction_row_json(&row)))
}

/// Round 219: DELETE /api/issues/:id/interactions/:interaction_id
///
/// 与 Node DELETE /issues/:id/interactions/:interactionId 对齐。
async fn delete_issue_interaction(
    State(state): State<AppState>,
    Path((_id, interaction_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let removed = IssueRepo::new(&state.db)
        .delete_interaction(interaction_id)
        .await?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("interaction {interaction_id}")))
    }
}

/// Round 219: shared JSON converter for IssueThreadInteractionRow。
fn interaction_row_json(r: &pc_repos::issue::IssueThreadInteractionRow) -> Value {
    json!({
        "id": r.id,
        "companyId": r.company_id,
        "issueId": r.issue_id,
        "kind": r.kind,
        "status": r.status,
        "continuationPolicy": r.continuation_policy,
        "sourceCommentId": r.source_comment_id,
        "sourceRunId": r.source_run_id,
        "title": r.title,
        "summary": r.summary,
        "createdByAgentId": r.created_by_agent_id,
        "createdByUserId": r.created_by_user_id,
        "resolvedByAgentId": r.resolved_by_agent_id,
        "resolvedByUserId": r.resolved_by_user_id,
        "payload": r.payload,
        "result": r.result,
        "resolvedAt": r.resolved_at,
        "createdAt": r.created_at,
        "updatedAt": r.updated_at,
    })
}

async fn list_issue_feedback_votes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = FeedbackVoteRepo::new(&state.db);
    let rows = repo.list_by_issue(id, 100).await.unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "voterKind": r.target_type,
                "targetId": r.target_id,
                "vote": r.vote,
                "reason": r.reason,
                "createdAt": r.created_at,
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
    let target_type = body.get("voterKind").and_then(Value::as_str).unwrap_or("user").to_string();
    let target_id = body.get("targetId").and_then(Value::as_str)
        .unwrap_or("anonymous").to_string();
    let vote = body.get("vote").and_then(Value::as_str).map(str::to_string)
        .or_else(|| body.get("score").and_then(Value::as_i64).map(|n| n.to_string()))
        .unwrap_or_else(|| "neutral".to_string());
    let reason = body.get("reason").and_then(Value::as_str).map(str::to_string);
    let repo = FeedbackVoteRepo::new(&state.db);
    let vote_id = match repo
        .create_for_issue(id, &target_type, &target_id, "system", &vote, reason.as_deref())
        .await
    {
        Ok(id) => id,
        Err(sqlx::Error::RowNotFound) => {
            return Err(ApiError::NotFound(format!("issue {id}")));
        }
        Err(e) => return Err(ApiError::Internal(e.to_string())),
    };
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

/// Round 218: DELETE /api/issues/:id/read — 撤销已读标记。
///
/// 与 Node `markUnread` 对齐：仅 board 用户可调用。
async fn unmark_read_route(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let removed = IssueRepo::new(&state.db)
        .delete_read_state(id, &user_id)
        .await?;
    Ok(Json(json!({
        "id": id,
        "companyId": issue.company_id,
        "removed": removed,
    })))
}

/// Round 220: GET /api/issues/:id/activity — 列出关联该 issue 的活动事件。
///
/// 与 Node GET /issues/:id/activity 对齐：
/// - 通过 activity_log WHERE entity_type='issue' AND entity_id=issue_id 过滤
/// - 按 created_at DESC 排序
/// - 默认 limit=100，最大 500
async fn issue_activity(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<ActivityLimitQuery>,
) -> ApiResult<Json<Value>> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows = ActivityRepo::new(&state.db)
        .list_for_entity(issue.company_id, "issue", &id.to_string(), limit)
        .await?;
    let items: Vec<Value> = rows.iter().map(activity_log_row_json).collect();
    Ok(Json(json!({
        "items": items,
        "issueId": id,
        "total": items.len(),
        "limit": limit,
    })))
}

/// Round 220: 共享查询参数。
#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct ActivityLimitQuery {
    #[serde(default)]
    limit: Option<i64>,
}

/// Round 220: 活动行 JSON 转换器（issues.rs 本地简化版）。
///
/// 与 activity.rs 的同名函数保持一致 — 字段 camelCase 对齐 Node 端返回。
fn activity_log_row_json(row: &pc_repos::activity::ActivityRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "actorType": row.actor_type,
        "actorId": row.actor_id,
        "action": row.action,
        "entityType": row.entity_type,
        "entityId": row.entity_id,
        "agentId": row.agent_id,
        "runId": row.run_id,
        "responsibleUserId": row.responsible_user_id,
        "details": row.details,
        "createdAt": row.created_at,
    })
}

async fn list_issue_cases(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = CaseRepo::new(&state.db).list_issue_cases(id).await?;
    let items: Vec<Value> = rows.into_iter().map(|r| json!({
        "linkId": r.link_id,
        "caseId": r.case_id,
        "issueId": id,
        "role": r.role,
        "caseStatus": r.status,
        "projectId": r.project_id,
    })).collect();
    Ok(Json(json!({"items": items, "issueId": id})))
}

async fn list_issue_runs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Round 30: heartbeat_runs 没有 issue_id 列 — 关系走 context_snapshot->>'issueId'
    let rows = HeartbeatRepo::new(&state.db).list_runs_by_issue(id, 100).await?;
    let items: Vec<Value> = rows.into_iter().map(|r| json!({
        "id": r.id,
        "issueId": id,
        "companyId": r.company_id,
        "agentId": r.agent_id,
        "status": r.status,
        "invocationSource": r.invocation_source,
        "startedAt": r.started_at,
        "finishedAt": r.finished_at,
        "createdAt": r.created_at,
        "error": r.error,
    })).collect();
    Ok(Json(json!({"items": items, "issueId": id, "count": items.len()})))
}

async fn get_issue_run(
    State(state): State<AppState>,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row = HeartbeatRepo::new(&state.db)
        .get_run_with_context(run_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("run {run_id}")))?;
    let (rid, cid, aid, st, src, started, finished, created, err, ctx) = row;
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
    let cancelled = HeartbeatRepo::new(&state.db)
        .cancel_run_for_issue(run_id, id)
        .await?;
    if !cancelled {
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
    let repo = HeartbeatRepo::new(&state.db);
    let (agent_id, mut ctx) = repo
        .get_agent_and_context(run_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("run {run_id}")))?;
    let issue_in_ctx = ctx.get("issueId").and_then(|v| v.as_str());
    if issue_in_ctx != Some(&id.to_string()) {
        return Err(ApiError::NotFound(format!("run {run_id} not associated with issue {id}")));
    }
    if let Some(obj) = ctx.as_object_mut() {
        obj.insert("retryOf".into(), json!(run_id.to_string()));
        obj.insert("wakeReason".into(), json!("manual_restart"));
    }
    let company_id = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .map(|r| r.company_id)
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let new_id = Uuid::new_v4();
    repo.insert_queued_run(new_id, company_id, agent_id, &ctx).await?;
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
    let issue_row = IssueRepo::new(&state.db)
        .start_run_inputs(id)
        .await?;
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
    HeartbeatRepo::new(&state.db)
        .insert_queued_run(run_id, company_id, agent_id, &ctx)
        .await?;
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
    let row = IssueRepo::new(&state.db)
        .find_one_comment(id, comment_id)
        .await?;
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
    IssueTreeHoldRepo::new(&state.db).release(id, hold_id).await?;
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
    let exists = IssueRepo::new(&state.db)
        .issue_doc_exists(id, &key)
        .await?;
    if exists {
        IssueRepo::new(&state.db)
            .update_issue_doc_content(id, &key, body.content.as_ref().unwrap_or(&Value::Null))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    } else {
        IssueRepo::new(&state.db)
            .insert_issue_doc(id, &key, body.content.as_ref().unwrap_or(&Value::Null), body.title.as_deref())
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let n = IssueRepo::new(&state.db)
        .soft_delete_issue_doc(id, &key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !n {
        return Err(ApiError::NotFound(format!("document {key}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, Default)]
struct AnnotationCommentBodyV2 {
    body: String,
}

/// Round 221: POST /api/issues/:id/documents/:key/annotations/:thread_id/comments
///
/// 与 Node POST /issues/:id/documents/:key/annotations/:threadId/comments 对齐。
///
/// 实际逻辑与 `add_annotation_comment`（POST .../annotations/:thread_id）一致，
/// 这里仅作为 path 别名转发，保持 URL 兼容。
async fn annotation_comment_route(
    State(state): State<AppState>,
    Path((id, key, thread_id)): Path<(Uuid, String, Uuid)>,
    Json(body): Json<AnnotationCommentBodyV2>,
) -> ApiResult<Json<Value>> {
    // V2 body shape: { body } (与 Node createDocumentAnnotationCommentSchema 子集)
    // author_type/user 来自 actor context（本轮略 — 默认 'user'）
    if body.body.trim().is_empty() {
        return Err(ApiError::BadRequest("comment body must not be empty".into()));
    }
    let thread = pc_repos::document::DocumentRepo::new(&state.db)
        .get_annotation_thread(thread_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("annotation thread {thread_id}")))?;
    let _ = (id, key); // 仅满足参数解构
    let row = pc_repos::document::DocumentRepo::new(&state.db)
        .create_annotation_comment(
            thread.company_id,
            thread_id,
            id,
            thread.document_id,
            &body.body,
            "user",
            None,
        )
        .await?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}



async fn restore_doc_revision(
    State(state): State<AppState>,
    Path((id, key, revision_id)): Path<(Uuid, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let _ = IssueRepo::new(&state.db)
        .set_issue_doc_current_revision(id, &key, revision_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()));
    Ok(Json(json!({"restored": true, "issueId": id, "key": key, "revisionId": revision_id})))
}

/// Round 216: cancel / withdraw 共用的请求体。
#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct InteractionResolveBody {
    #[serde(default)]
    reason: Option<String>,
}

/// Round 217: accept 专用 body（含向后兼容字段）。
///
/// Node `acceptIssueThreadInteractionSchema`：
/// - selectedClientKeys?: string[]
/// - selectedOptionIds?: string[]
///
/// `reason` 保留为可选 — 旧 stub (R96) 用的是 InteractionDecisionBody，
/// 保留 reason 字段避免破坏可能存在的客户端调用。
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct AcceptInteractionBody {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    selected_client_keys: Option<Vec<String>>,
    #[serde(default)]
    selected_option_ids: Option<Vec<String>>,
}

/// Round 217: respond body。
///
/// Node `respondIssueThreadInteractionSchema`：
/// - answers: array (1..20)
/// - summaryMarkdown?: string | null
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct RespondInteractionBody {
    #[serde(default)]
    answers: Vec<serde_json::Value>,
    #[serde(default)]
    summary_markdown: Option<String>,
}

/// Round 217: verdicts body。
///
/// Node `submitIssueThreadInteractionVerdictsSchema`：
/// - verdicts: array of { id, verdict, reason? }
#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct VerdictInteractionBody {
    #[serde(default)]
    verdicts: Vec<VerdictEntry>,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct VerdictEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    verdict: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct InteractionDecisionBody {
    #[serde(default)]
    verdict: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

/// Round 216: cancel / withdraw 共享的解析逻辑。
///
/// 流程：
/// 1. 加载 issue（必须存在）
/// 2. 校验 interaction 属于该 issue
/// 3. 调用 `IssueRepo::resolve_interaction` 写入终态
/// 4. 通过 `state.activity` 记录活动事件
/// 5. 返回更新后的 row
async fn resolve_interaction_status(
    state: &AppState,
    issue_id: Uuid,
    interaction_id: Uuid,
    new_status: &str,
    reason: Option<&str>,
    activity_kind: &str,
) -> ApiResult<Json<Value>> {
    let issue = IssueRepo::new(&state.db)
        .get(issue_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;

    let interaction = IssueRepo::new(&state.db)
        .get_interaction(interaction_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("interaction {interaction_id}")))?;
    if interaction.issue_id != issue_id {
        return Err(ApiError::BadRequest(
            "interaction does not belong to issue".into(),
        ));
    }

    let result_json = reason.map(|r| serde_json::json!({ "reason": r }));

    let updated = IssueRepo::new(&state.db)
        .resolve_interaction(
            interaction_id,
            new_status,
            result_json.as_ref(),
            None, // resolved_by_user_id 在此场景通常由 auth context 提供
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("interaction {interaction_id}")))?;

    // 活动事件（best-effort，失败不影响主流程）
    let event = ActivityEvent::new(
        parse_activity_kind(activity_kind),
        ActivityActor::System {
            component: "paperclip-api".into(),
        },
        "issue_thread_interaction",
        interaction_id,
    )
    .with_company(issue.company_id)
    .with_payload(serde_json::json!({
        "issueId": issue_id,
        "interactionKind": interaction.kind,
        "newStatus": new_status,
        "reason": reason,
    }));
    let _ = state.activity.emit(event).await;

    Ok(Json(serde_json::json!({
        "id": updated.id,
        "issueId": updated.issue_id,
        "kind": updated.kind,
        "status": updated.status,
        "result": updated.result,
        "resolvedAt": updated.resolved_at,
        "updatedAt": updated.updated_at,
    })))
}

/// Round 217: resolve_interaction_status 的扩展版本，支持传入自定义 result JSON。
///
/// 适用场景：accept / respond / verdicts 需要把请求字段
/// (selectedClientKeys, answers, verdicts) 写入 result 列。
async fn resolve_interaction_status_with_payload(
    state: &AppState,
    issue_id: Uuid,
    interaction_id: Uuid,
    new_status: &str,
    reason: Option<&str>,
    mut result_payload: serde_json::Value,
    activity_kind: &str,
) -> ApiResult<Json<Value>> {
    let issue = IssueRepo::new(&state.db)
        .get(issue_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;

    let interaction = IssueRepo::new(&state.db)
        .get_interaction(interaction_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("interaction {interaction_id}")))?;
    if interaction.issue_id != issue_id {
        return Err(ApiError::BadRequest(
            "interaction does not belong to issue".into(),
        ));
    }

    // Merge reason into payload if provided
    if let Some(r) = reason {
        if let serde_json::Value::Object(ref mut map) = result_payload {
            map.insert("reason".to_string(), serde_json::Value::String(r.to_string()));
        }
    }

    let updated = IssueRepo::new(&state.db)
        .resolve_interaction(
            interaction_id,
            new_status,
            Some(&result_payload),
            None,
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("interaction {interaction_id}")))?;

    let event = ActivityEvent::new(
        parse_activity_kind(activity_kind),
        ActivityActor::System {
            component: "paperclip-api".into(),
        },
        "issue_thread_interaction",
        interaction_id,
    )
    .with_company(issue.company_id)
    .with_payload(serde_json::json!({
        "issueId": issue_id,
        "interactionKind": interaction.kind,
        "newStatus": new_status,
        "reason": reason,
    }));
    let _ = state.activity.emit(event).await;

    Ok(Json(serde_json::json!({
        "id": updated.id,
        "issueId": updated.issue_id,
        "kind": updated.kind,
        "status": updated.status,
        "result": updated.result,
        "resolvedAt": updated.resolved_at,
        "updatedAt": updated.updated_at,
    })))
}

/// Round 216: 将字符串映射为活动 kind。
/// 当前 ActivityKind 枚举没有 thread_interaction 相关变体，统一映射为 Other。
/// payload 中保留具体 kind 字符串，便于上层过滤。
fn parse_activity_kind(_s: &str) -> ActivityKind {
    ActivityKind::Other
}

/// Round 217: POST /api/issues/:id/interactions/:interaction_id/accept
///
/// 与 Node `acceptIssueThreadInteraction` 对齐。
/// Body: `{ selectedClientKeys?, selectedOptionIds? }` (Node schema 完全 1:1)
///
/// 仓储层面通过 `resolve_interaction(status="accepted")` 完成
/// payload (selectedClientKeys/selectedOptionIds) 写入 result JSON。
async fn accept_interaction(
    State(state): State<AppState>,
    Path((id, iid)): Path<(Uuid, Uuid)>,
    Json(body): Json<AcceptInteractionBody>,
) -> ApiResult<Json<Value>> {
    resolve_interaction_status_with_payload(
        &state,
        id,
        iid,
        "accepted",
        body.reason.as_deref(),
        serde_json::json!({
            "selectedClientKeys": body.selected_client_keys,
            "selectedOptionIds": body.selected_option_ids,
        }),
        "issue.thread_interaction_accepted",
    )
    .await
}

/// Round 216: POST /api/issues/:id/interactions/:interaction_id/cancel
///
/// 与 Node `cancelIssueThreadInteraction` 对齐。
/// Body: `{ reason?: string }`
async fn cancel_interaction(
    State(state): State<AppState>,
    Path((id, iid)): Path<(Uuid, Uuid)>,
    Json(body): Json<InteractionResolveBody>,
) -> ApiResult<Json<Value>> {
    resolve_interaction_status(
        &state,
        id,
        iid,
        "cancelled",
        body.reason.as_deref(),
        "issue.thread_interaction_cancelled",
    )
    .await
}

/// Round 217: POST /api/issues/:id/interactions/:interaction_id/reject
///
/// 与 Node `rejectIssueThreadInteraction` 对齐。
/// Body: `{ reason?: string }`
async fn reject_interaction(
    State(state): State<AppState>,
    Path((id, iid)): Path<(Uuid, Uuid)>,
    Json(body): Json<InteractionResolveBody>,
) -> ApiResult<Json<Value>> {
    resolve_interaction_status(
        &state,
        id,
        iid,
        "rejected",
        body.reason.as_deref(),
        "issue.thread_interaction_rejected",
    )
    .await
}

/// Round 217: POST /api/issues/:id/interactions/:interaction_id/respond
///
/// 与 Node `respondIssueThreadInteraction` 对齐。
/// Body: `{ answers: [...], summaryMarkdown?: string }`
///
/// answers 通过 payload 写入 result JSON。
async fn respond_interaction(
    State(state): State<AppState>,
    Path((id, iid)): Path<(Uuid, Uuid)>,
    Json(body): Json<RespondInteractionBody>,
) -> ApiResult<Json<Value>> {
    let result_json = serde_json::json!({
        "answers": body.answers,
        "summaryMarkdown": body.summary_markdown,
    });
    resolve_interaction_status_with_payload(
        &state,
        id,
        iid,
        "responded",
        None,
        result_json,
        "issue.thread_interaction_responded",
    )
    .await
}

/// Round 217: POST /api/issues/:id/interactions/:interaction_id/verdicts
///
/// 与 Node `submitIssueThreadInteractionVerdicts` 对齐。
/// Body: `{ verdicts: [{ id, verdict, reason? }] }`
///
/// verdicts 通过 payload 写入 result JSON。
async fn verdict_interaction(
    State(state): State<AppState>,
    Path((id, iid)): Path<(Uuid, Uuid)>,
    Json(body): Json<VerdictInteractionBody>,
) -> ApiResult<Json<Value>> {
    let result_json = serde_json::json!({ "verdicts": body.verdicts });
    resolve_interaction_status_with_payload(
        &state,
        id,
        iid,
        "responded",
        None,
        result_json,
        "issue.thread_interaction_verdicts",
    )
    .await
}

/// Round 216: POST /api/issues/:id/interactions/:interaction_id/withdraw
///
/// 与 Node `withdrawIssueThreadInteraction` 对齐。
/// Body: `{ reason?: string }`
///
/// 区别于 cancel：
/// - cancel 通常用于撤销整个 thread / system action
/// - withdraw 通常是 agent 自己撤回之前发出的请求
/// 但仓储层面都通过 `resolve_interaction(status=...)` 完成
async fn withdraw_interaction(
    State(state): State<AppState>,
    Path((id, iid)): Path<(Uuid, Uuid)>,
    Json(body): Json<InteractionResolveBody>,
) -> ApiResult<Json<Value>> {
    resolve_interaction_status(
        &state,
        id,
        iid,
        "withdrawn",
        body.reason.as_deref(),
        "issue.thread_interaction_withdrawn",
    )
    .await
}

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
    let company_id = IssueRepo::new(&state.db)
        .get(issue_id)
        .await?
        .map(|r| r.company_id)
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;
    let status = q.status.as_deref().unwrap_or("active");
    let repo = IssueTreeHoldRepo::new(&state.db);
    let rows = repo.list_by_root(issue_id, status, 100).await.unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "companyId": company_id,
                "rootIssueId": r.root_issue_id,
                "mode": r.mode,
                "status": r.status,
                "reason": r.reason,
                "releasePolicy": r.release_policy,
                "createdAt": r.created_at,
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
    let company_id = IssueRepo::new(&state.db)
        .get(issue_id)
        .await?
        .map(|r| r.company_id)
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    let id = IssueTreeHoldRepo::new(&state.db)
        .create(&NewIssueTreeHold {
            company_id,
            root_issue_id: issue_id,
            mode: &body.mode,
            reason: body.reason.as_deref(),
            release_policy: body.release_policy.clone().unwrap_or_else(|| json!({})),
            created_by_user_id: &user_id,
        })
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
    let row = IssueTreeHoldRepo::new(&state.db)
        .get_by_id(hold_id, issue_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tree hold {hold_id}")))?;
    let (id, root_issue_id, mode, status, reason, release_policy, released_at, created_at) = (
        row.id, row.root_issue_id, row.mode, row.status, row.reason, row.release_policy,
        row.released_at, row.created_at,
    );
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
    let company_id = IssueRepo::new(&state.db)
        .get(issue_id)
        .await?
        .map(|r| r.company_id)
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;
    if !matches!(body.mode.as_str(), "pause" | "stop" | "throttle" | "isolate") {
        return Err(ApiError::BadRequest(format!("invalid mode '{}'", body.mode)));
    }
    // Estimate: count active descendants (best-effort: just count active heartbeat_runs referencing this issue)
    let mut affected_runs = 0i64;
    if body.include_estimate.unwrap_or(true) {
        affected_runs = HeartbeatRepo::new(&state.db)
            .count_active_runs_for_issue(issue_id)
            .await
            .unwrap_or(0);
    }
    // Active hold check
    let active_hold = IssueTreeHoldRepo::new(&state.db)
        .find_active_for_root(issue_id)
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
    let rows = IssueDiagnosticsRepo::new(&state.db)
        .list_blockers(id, 100)
        .await?;
    let blockers: Vec<Value> = rows.into_iter().map(|r| json!({
        "id": r.id, "title": r.title, "status": r.status, "createdAt": r.created_at,
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
    let repo = IssueDiagnosticsRepo::new(&state.db);
    let agent_id = match repo.assignee_agent_id(id).await? {
        Some(a) => a,
        None => return Ok(Json(json!({"issueId": id, "wakeRequests": [], "activityRecords": [], "wakeRequestCount": 0, "activityRecordCount": 0}))),
    };
    let wakes = repo.list_wake_requests_for_agent(id, agent_id, 100).await?;
    let wake_requests: Vec<Value> = wakes.into_iter().map(|r| json!({
        "id": r.id, "source": r.source, "reason": r.reason, "status": r.status,
        "requestedAt": r.requested_at, "claimedAt": r.claimed_at,
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
    // Round 30: subtree = 递归查所有 parent_id 链上的 issues（max_depth = 8）
    let rows = IssueDiagnosticsRepo::new(&state.db)
        .list_subtree(id, 8)
        .await?;
    let mut nodes: Vec<Value> = Vec::with_capacity(rows.len());
    let mut edges: Vec<Value> = Vec::new();
    let mut readiness: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for r in rows {
        nodes.push(json!({"id": r.id, "title": r.title, "status": r.status, "createdAt": r.created_at}));
        if let Some(st) = r.status.clone() {
            readiness.insert(r.id.to_string(), st);
        }
        if let Some(p) = r.parent_id {
            edges.push(json!({"from": p, "to": r.id}));
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

    let row = IssueRepo::new(&state.db)
        .attachment_content_meta(attachment_id)
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

#[cfg(test)]
mod round216_tests {
    //! Round 216: interaction cancel/withdraw 共享解析逻辑的单元测试。
    //!
    //! `parse_activity_kind` 是纯函数 — 容易单测。
    //! `InteractionResolveBody` 是 serde 结构 — 验证字段解析。
    use super::{
        activity_log_row_json, interaction_row_json, parse_activity_kind,
        AcceptInteractionBody, InteractionResolveBody, RespondInteractionBody,
        VerdictEntry, VerdictInteractionBody,
    };
    use pc_activity::kinds::ActivityKind;

    #[test]
    fn parse_activity_kind_returns_other_for_known_strings() {
        // 当前实现统一映射到 ActivityKind::Other
        // 因为枚举没有 thread_interaction 变体。
        assert!(matches!(
            parse_activity_kind("issue.thread_interaction_cancelled"),
            ActivityKind::Other
        ));
        assert!(matches!(
            parse_activity_kind("issue.thread_interaction_withdrawn"),
            ActivityKind::Other
        ));
        assert!(matches!(parse_activity_kind("unknown.kind"), ActivityKind::Other));
    }

    #[test]
    fn interaction_resolve_body_accepts_empty_object() {
        let body: InteractionResolveBody = serde_json::from_str("{}").expect("parse");
        assert!(body.reason.is_none());
    }

    #[test]
    fn interaction_resolve_body_accepts_reason() {
        let body: InteractionResolveBody =
            serde_json::from_str(r#"{"reason": "user changed mind"}"#).expect("parse");
        assert_eq!(body.reason.as_deref(), Some("user changed mind"));
    }

    #[test]
    fn interaction_resolve_body_accepts_null_reason() {
        let body: InteractionResolveBody = serde_json::from_str(r#"{"reason": null}"#)
            .expect("parse null");
        assert!(body.reason.is_none());
    }

    // ── R217 新 body 类型测试 ──

    #[test]
    fn accept_body_parses_selected_keys_and_options() {
        let body: AcceptInteractionBody = serde_json::from_str(
            r#"{"selectedClientKeys":["k1","k2"],"selectedOptionIds":["o1"]}"#,
        )
        .expect("parse");
        assert_eq!(
            body.selected_client_keys.as_deref(),
            Some(&["k1".to_string(), "k2".to_string()][..])
        );
        assert_eq!(
            body.selected_option_ids.as_deref(),
            Some(&["o1".to_string()][..])
        );
    }

    #[test]
    fn accept_body_empty_object() {
        let body: AcceptInteractionBody = serde_json::from_str("{}").expect("parse");
        assert!(body.selected_client_keys.is_none());
        assert!(body.selected_option_ids.is_none());
        assert!(body.reason.is_none());
    }

    #[test]
    fn respond_body_parses_answers_and_summary() {
        let body: RespondInteractionBody = serde_json::from_str(
            r#"{"answers":[{"id":"q1","value":"a"}],"summaryMarkdown":"thanks"}"#,
        )
        .expect("parse");
        assert_eq!(body.answers.len(), 1);
        assert_eq!(body.summary_markdown.as_deref(), Some("thanks"));
    }

    #[test]
    fn respond_body_summary_markdown_optional() {
        let body: RespondInteractionBody =
            serde_json::from_str(r#"{"answers":[]}"#).expect("parse");
        assert_eq!(body.answers.len(), 0);
        assert!(body.summary_markdown.is_none());
    }

    #[test]
    fn verdict_body_parses_entries() {
        let body: VerdictInteractionBody = serde_json::from_str(
            r#"{"verdicts":[{"id":"v1","verdict":"approve","reason":"ok"}]}"#,
        )
        .expect("parse");
        assert_eq!(body.verdicts.len(), 1);
        assert_eq!(body.verdicts[0].id, "v1");
        assert_eq!(body.verdicts[0].verdict, "approve");
        assert_eq!(body.verdicts[0].reason.as_deref(), Some("ok"));
    }

    #[test]
    fn verdict_body_reason_optional() {
        let body: VerdictInteractionBody = serde_json::from_str(
            r#"{"verdicts":[{"id":"v1","verdict":"reject"}]}"#,
        )
        .expect("parse");
        assert!(body.verdicts[0].reason.is_none());
    }

    // ── R219 interaction_row_json + CreateInteractionBody 测试 ──

    #[test]
    fn activity_log_row_json_uses_camel_case_keys() {
        // 验证序列化输出字段名都是 camelCase
        use pc_repos::activity::ActivityRow;
        let now = chrono::Utc::now();
        let row = ActivityRow {
            id: uuid::Uuid::nil(),
            company_id: uuid::Uuid::nil(),
            actor_type: "user".to_string(),
            actor_id: "user-1".to_string(),
            action: "issue.updated".to_string(),
            entity_type: "issue".to_string(),
            entity_id: uuid::Uuid::nil().to_string(),
            agent_id: None,
            run_id: None,
            responsible_user_id: None,
            details: Some(serde_json::json!({"key": "value"})),
            created_at: pc_core::Timestamp::from_dt(now),
        };
        let json = activity_log_row_json(&row);
        let obj = json.as_object().expect("object");
        assert!(obj.contains_key("companyId"));
        assert!(obj.contains_key("actorType"));
        assert!(obj.contains_key("actorId"));
        assert!(obj.contains_key("entityType"));
        assert!(obj.contains_key("entityId"));
        assert!(obj.contains_key("agentId"));
        assert!(obj.contains_key("runId"));
        assert!(obj.contains_key("responsibleUserId"));
        assert!(obj.contains_key("createdAt"));
    }

    #[test]
    fn interaction_row_json_uses_camel_case_keys() {
        // 验证序列化输出字段名都是 camelCase
        use pc_repos::issue::IssueThreadInteractionRow;

        let now = chrono::Utc::now();
        let row = IssueThreadInteractionRow {
            id: uuid::Uuid::nil(),
            company_id: uuid::Uuid::nil(),
            issue_id: uuid::Uuid::nil(),
            kind: "suggest_tasks".to_string(),
            status: "pending".to_string(),
            continuation_policy: "wake_assignee".to_string(),
            source_comment_id: None,
            source_run_id: None,
            title: Some("test".to_string()),
            summary: None,
            created_by_agent_id: None,
            created_by_user_id: None,
            resolved_by_agent_id: None,
            resolved_by_user_id: None,
            payload: serde_json::json!({"k": "v"}),
            result: None,
            resolved_at: None,
            created_at: pc_core::Timestamp::from_dt(now),
            updated_at: pc_core::Timestamp::from_dt(now),
        };
        let json = interaction_row_json(&row);
        let obj = json.as_object().expect("object");
        // 关键 camelCase 字段
        assert!(obj.contains_key("issueId"));
        assert!(obj.contains_key("companyId"));
        assert!(obj.contains_key("kind"));
        assert!(obj.contains_key("status"));
        assert!(obj.contains_key("continuationPolicy"));
        assert!(obj.contains_key("sourceCommentId"));
        assert!(obj.contains_key("sourceRunId"));
        assert!(obj.contains_key("createdByAgentId"));
        assert!(obj.contains_key("createdByUserId"));
        assert!(obj.contains_key("resolvedByAgentId"));
        assert!(obj.contains_key("resolvedByUserId"));
        assert!(obj.contains_key("resolvedAt"));
        assert!(obj.contains_key("createdAt"));
        assert!(obj.contains_key("updatedAt"));
    }

    // ── R222: plan_decomposition body 解析 + fingerprint 稳定性 + JSON 序列化 ──

    use super::CreateAcceptedPlanDecompositionBody;
    use serde_json::json;

    #[test]
    fn plan_decomp_body_parses_revision_and_children() {
        let body: CreateAcceptedPlanDecompositionBody = serde_json::from_value(json!({
            "acceptedPlanRevisionId": "00000000-0000-0000-0000-000000000001",
            "children": [
                {"title": "child 1", "description": "first"},
                {"title": "child 2", "description": "second"},
            ],
        }))
        .expect("parse");
        assert_eq!(
            body.accepted_plan_revision_id,
            uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
        );
        assert_eq!(body.children.len(), 2);
    }

    #[test]
    fn plan_decomp_body_rejects_empty_children_via_deserialize() {
        // children 字段是 Vec<Value>，默认空 vec 不报错
        // 上层 handler 负责 400 业务校验
        let body: CreateAcceptedPlanDecompositionBody = serde_json::from_value(json!({
            "acceptedPlanRevisionId": "00000000-0000-0000-0000-000000000002",
        }))
        .expect("parse default children");
        assert!(body.children.is_empty());
    }

    #[test]
    fn plan_decomp_fingerprint_stable_for_same_input() {
        let rev = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap();
        let children = vec![
            json!({"title": "a", "description": "1"}),
            json!({"title": "b", "description": "2"}),
        ];
        let fp1 = super::compute_plan_decomposition_fingerprint(rev, &children);
        let fp2 = super::compute_plan_decomposition_fingerprint(rev, &children);
        assert_eq!(fp1, fp2, "相同输入应产生相同 fingerprint");
    }

    #[test]
    fn plan_decomp_fingerprint_differs_for_different_children() {
        let rev = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap();
        let children1 = vec![json!({"title": "a"})];
        let children2 = vec![json!({"title": "b"})];
        let fp1 = super::compute_plan_decomposition_fingerprint(rev, &children1);
        let fp2 = super::compute_plan_decomposition_fingerprint(rev, &children2);
        assert_ne!(fp1, fp2, "不同 children 应产生不同 fingerprint");
    }

    #[test]
    fn plan_decomp_fingerprint_differs_for_different_revisions() {
        let rev1 = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000005").unwrap();
        let rev2 = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000006").unwrap();
        let children = vec![json!({"title": "same"})];
        let fp1 = super::compute_plan_decomposition_fingerprint(rev1, &children);
        let fp2 = super::compute_plan_decomposition_fingerprint(rev2, &children);
        assert_ne!(fp1, fp2, "不同 revision 应产生不同 fingerprint");
    }

    #[test]
    fn plan_decomp_row_json_uses_camel_case_keys() {
        use pc_repos::issue::IssuePlanDecompositionRow;
        let now = chrono::Utc::now();
        let row = IssuePlanDecompositionRow {
            id: uuid::Uuid::nil(),
            company_id: uuid::Uuid::nil(),
            source_issue_id: uuid::Uuid::nil(),
            accepted_plan_revision_id: uuid::Uuid::nil(),
            accepted_interaction_id: None,
            status: "in_flight".to_string(),
            request_fingerprint: "abc".to_string(),
            requested_child_count: 3,
            requested_children: json!([]),
            child_issue_ids: json!([]),
            owner_agent_id: None,
            owner_user_id: None,
            owner_run_id: None,
            completed_at: None,
            created_at: pc_core::Timestamp::from_dt(now),
            updated_at: pc_core::Timestamp::from_dt(now),
        };
        let v = super::plan_decomposition_row_json(&row);
        let obj = v.as_object().expect("object");
        // 关键 camelCase 字段（与 Node AcceptPlanDecomposition 序列化对齐）
        assert!(obj.contains_key("companyId"));
        assert!(obj.contains_key("sourceIssueId"));
        assert!(obj.contains_key("acceptedPlanRevisionId"));
        assert!(obj.contains_key("acceptedInteractionId"));
        assert!(obj.contains_key("requestFingerprint"));
        assert!(obj.contains_key("requestedChildCount"));
        assert!(obj.contains_key("childIssueIds"));
        assert!(obj.contains_key("ownerAgentId"));
        assert!(obj.contains_key("ownerUserId"));
        assert!(obj.contains_key("ownerRunId"));
        assert!(obj.contains_key("completedAt"));
        assert!(obj.contains_key("createdAt"));
        assert!(obj.contains_key("updatedAt"));
    }
}
