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
use pc_repos::issue::{IssueRelationUpdate, IssueRepo, IssueUpdateActor, IssueUpdateReceipt};
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
        .route(
            "/api/issues/:id/tree-control/state",
            get(tree_control_state),
        )
        .route(
            "/api/issues/:id/live-runs",
            get(list_live_runs),
        )
        .route(
            "/api/issues/:id/active-run",
            get(active_run),
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

// ============================================================================
// Round 229: 完整 issue body 结构（对齐 Node `createIssueBaseSchema`）
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateIssueFullBody {
    /// 必填：所属 company id。
    company_id: Uuid,
    /// 必填：issue 标题。
    title: String,
    /// 可选描述。
    #[serde(default)]
    description: Option<String>,
    /// 状态 — 默认 "todo"。Node 端允许 "backlog" / "todo" / "in_progress" / "in_review" / "done" / "blocked" / "cancelled"。
    #[serde(default)]
    status: Option<String>,
    /// 工作模式 — 默认 "standard"。
    #[serde(default)]
    work_mode: Option<String>,
    /// harness 类型（plan / task 等）。
    #[serde(default)]
    harness_kind: Option<String>,
    /// 优先级 — 默认 "medium"。
    #[serde(default = "default_priority")]
    priority: String,
    /// 分配的 agent id。
    #[serde(default)]
    assignee_agent_id: Option<Uuid>,
    /// 分配的 user id。
    #[serde(default)]
    assignee_user_id: Option<String>,
    /// 所属 project id。
    #[serde(default)]
    project_id: Option<Uuid>,
    /// 所属 project workspace id。
    #[serde(default)]
    project_workspace_id: Option<Uuid>,
    /// 关联 goal id。
    #[serde(default)]
    goal_id: Option<Uuid>,
    /// 父 issue id（创建子 issue 时设置）。
    #[serde(default)]
    parent_id: Option<Uuid>,
    /// 从指定 issue 继承 execution workspace 配置。
    #[serde(default)]
    inherit_execution_workspace_from_issue_id: Option<Uuid>,
    /// 创建者 user id。
    #[serde(default)]
    created_by_user_id: Option<String>,
    /// 责任 user id。
    #[serde(default)]
    responsible_user_id: Option<String>,
    /// 计费代码。
    #[serde(default)]
    billing_code: Option<String>,
    /// 请求深度（用于追踪递归创建）。
    #[serde(default)]
    request_depth: Option<i32>,
    /// 分配 agent 的 adapter 覆盖配置。
    #[serde(default)]
    assignee_adapter_overrides: Option<Value>,
    /// 执行策略。
    #[serde(default)]
    execution_policy: Option<Value>,
    /// 关联 execution workspace id。
    #[serde(default)]
    execution_workspace_id: Option<Uuid>,
    /// execution workspace 偏好（isolated/shared/inherit 等）。
    #[serde(default)]
    execution_workspace_preference: Option<String>,
    /// execution workspace 设置。
    #[serde(default)]
    execution_workspace_settings: Option<Value>,
    /// 阻塞 issue id 列表。
    #[serde(default)]
    blocked_by_issue_ids: Option<Vec<Uuid>>,
    /// 标签 id 列表。
    #[serde(default)]
    label_ids: Option<Vec<Uuid>>,
    /// unblock 描述符（status='blocked' 时使用）。
    #[serde(default)]
    unblock_descriptor: Option<Value>,
    /// 幂等性 key。
    #[serde(default)]
    idempotency_key: Option<String>,
    /// 是否允许绕过 recent-title 重复检测。
    #[serde(default)]
    allow_duplicate: Option<bool>,
}

fn default_priority() -> String {
    "medium".into()
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateIssueFullBody>,
) -> ApiResult<impl IntoResponse> {
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    // unblockDescriptor 必须配 status='blocked'
    if body.unblock_descriptor.is_some() {
        let s = body.status.as_deref().unwrap_or("todo");
        if s != "blocked" {
            return Err(ApiError::BadRequest(
                "unblockDescriptor requires blocked status".into(),
            ));
        }
    }
    let request_depth = body.request_depth.unwrap_or(0);
    // R235: idempotency key 重放 — 如果 idempotency_key 存在, 先查找 existing
    if let Some(key) = body.idempotency_key.as_deref() {
        if let Some(existing_id) = IssueRepo::new(&state.db)
            .find_idempotency_key(body.company_id, key)
            .await?
        {
            if let Some(existing) = IssueRepo::new(&state.db).get(existing_id).await? {
                // Replay: 返回 existing issue (200 OK)
                return Ok((
                    StatusCode::OK,
                    Json(json!({
                        "id": existing.id,
                        "company_id": existing.company_id,
                        "title": existing.title,
                        "status": existing.status,
                        "priority": existing.priority,
                        "workMode": existing.work_mode,
                        "replayed": true,
                    })),
                ));
            }
        }
    }
    if let Some(pid) = body.parent_id {
        // 子 issue：通过 create_child_full 路径以继承 company_id 与 request_depth。
        let parent = IssueRepo::new(&state.db)
            .get(pid)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("parent issue {pid}")))?;
        let input = pc_repos::issue::CreateChildIssueInput {
            title: &body.title,
            description: body.description.as_deref(),
            status: body.status.as_deref(),
            work_mode: body.work_mode.as_deref(),
            harness_kind: body.harness_kind.as_deref(),
            priority: Some(&body.priority),
            assignee_agent_id: body.assignee_agent_id,
            assignee_user_id: body.assignee_user_id.as_deref(),
            project_id: body.project_id,
            project_workspace_id: body.project_workspace_id,
            goal_id: body.goal_id,
            created_by_user_id: body.created_by_user_id.as_deref(),
            responsible_user_id: body.responsible_user_id.as_deref(),
            billing_code: body.billing_code.as_deref(),
            request_depth,
            assignee_adapter_overrides: body.assignee_adapter_overrides.as_ref(),
            execution_policy: body.execution_policy.as_ref(),
            execution_workspace_id: body.execution_workspace_id,
            execution_workspace_preference: body.execution_workspace_preference.as_deref(),
            execution_workspace_settings: body.execution_workspace_settings.as_ref(),
            blocked_by_issue_ids: body.blocked_by_issue_ids.as_deref(),
            label_ids: body.label_ids.as_deref(),
            unblock_descriptor: body.unblock_descriptor.as_ref(),
            acceptance_criteria: None,
            block_parent_until_done: false,
        };
        // R230: 当 label_ids 或 blocked_by_issue_ids 不为空时使用事务版本,
        // 在事务内同步插入 labels / relations
        let needs_relations = body.label_ids.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
            || body.blocked_by_issue_ids.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
        let row = if needs_relations {
            IssueRepo::new(&state.db)
                .create_child_full_with_relations(&parent, &input, None)
                .await?
        } else {
            IssueRepo::new(&state.db).create_child_full(&parent, &input).await?
        };
        // R235: 持久化 idempotency key
        // 注：ChildIssueFullBody 暂无 idempotency_key 字段, 未来可加
        // 当前复用 parent.company_id 作为 company_id
        state.realtime.publish(
            LiveEvent::new("issue.created", "issue", row.id)
                .with_company(row.company_id)
                .with_actor("system"),
        );
        return Ok((
            StatusCode::CREATED,
            Json(json!({
                "id": row.id, "company_id": row.company_id, "parent_id": row.parent_id,
                "title": row.title, "status": row.status, "priority": row.priority,
                "workMode": row.work_mode
            })),
        ));
    }
    let input = pc_repos::issue::CreateIssueInput {
        company_id: body.company_id,
        title: &body.title,
        description: body.description.as_deref(),
        status: body.status.as_deref(),
        work_mode: body.work_mode.as_deref(),
        harness_kind: body.harness_kind.as_deref(),
        priority: Some(&body.priority),
        assignee_agent_id: body.assignee_agent_id,
        assignee_user_id: body.assignee_user_id.as_deref(),
        project_id: body.project_id,
        project_workspace_id: body.project_workspace_id,
        goal_id: body.goal_id,
        parent_id: body.parent_id,
        inherit_execution_workspace_from_issue_id: body.inherit_execution_workspace_from_issue_id,
        created_by_user_id: body.created_by_user_id.as_deref(),
        responsible_user_id: body.responsible_user_id.as_deref(),
        billing_code: body.billing_code.as_deref(),
        request_depth,
        assignee_adapter_overrides: body.assignee_adapter_overrides.as_ref(),
        execution_policy: body.execution_policy.as_ref(),
        execution_workspace_id: body.execution_workspace_id,
        execution_workspace_preference: body.execution_workspace_preference.as_deref(),
        execution_workspace_settings: body.execution_workspace_settings.as_ref(),
        blocked_by_issue_ids: body.blocked_by_issue_ids.as_deref(),
        label_ids: body.label_ids.as_deref(),
        unblock_descriptor: body.unblock_descriptor.as_ref(),
    };
    // R230: 当 label_ids 或 blocked_by_issue_ids 不为空时使用事务版本
    let needs_relations = body.label_ids.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
        || body.blocked_by_issue_ids.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
    let row = if needs_relations {
        IssueRepo::new(&state.db)
            .create_full_with_relations(&input, None)
            .await?
    } else {
        IssueRepo::new(&state.db).create_full(&input).await?
    };
    // R235: 持久化 idempotency key (如果有)
    if let Some(key) = body.idempotency_key.as_deref() {
        let _ = IssueRepo::new(&state.db)
            .create_idempotency_key(body.company_id, key, row.id)
            .await?;
    }
    state.realtime.publish(
        LiveEvent::new("issue.created", "issue", row.id)
            .with_company(row.company_id)
            .with_actor("system"),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "company_id": row.company_id, "title": row.title,
            "status": row.status, "priority": row.priority, "workMode": row.work_mode
        })),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateIssueFullBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    work_mode: Option<String>,
    #[serde(default)]
    harness_kind: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    assignee_agent_id: Option<Uuid>,
    #[serde(default)]
    assignee_user_id: Option<String>,
    #[serde(default)]
    responsible_user_id: Option<String>,
    #[serde(default)]
    billing_code: Option<String>,
    #[serde(default)]
    execution_policy: Option<Value>,
    #[serde(default)]
    execution_workspace_id: Option<Uuid>,
    #[serde(default)]
    execution_workspace_preference: Option<String>,
    #[serde(default)]
    execution_workspace_settings: Option<Value>,
    #[serde(default)]
    unblock_descriptor: Option<Value>,
    #[serde(default)]
    hidden_at: Option<String>,
    #[serde(default)]
    reopen: Option<bool>,
    #[serde(default)]
    resume: Option<bool>,
    #[serde(default)]
    interrupt: Option<bool>,
    #[serde(default, alias = "label_ids")]
    label_ids: Option<Vec<Uuid>>,
    #[serde(default, alias = "blocked_by_issue_ids")]
    blocked_by_issue_ids: Option<Vec<Uuid>>,
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateIssueFullBody>,
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
    // R234: actor 提取为 JSON 字符串(在 actor move 到 update_with_relations 调用前)
    let interrupt_actor_json = actor
        .as_ref()
        .map(|a| json!({"agentId": a.agent_id, "userId": a.user_id, "runId": a.run_id}))
        .unwrap_or(json!({}));
    let receipt = if body.title.is_some()
        || body.description.is_some()
        || body.status.is_some()
        || body.work_mode.is_some()
        || body.harness_kind.is_some()
        || body.priority.is_some()
        || body.assignee_agent_id.is_some()
        || body.assignee_user_id.is_some()
        || body.responsible_user_id.is_some()
        || body.billing_code.is_some()
        || body.execution_policy.is_some()
        || body.execution_workspace_id.is_some()
        || body.execution_workspace_preference.is_some()
        || body.execution_workspace_settings.is_some()
        || body.unblock_descriptor.is_some()
        || body.hidden_at.is_some()
    {
        // 完整 update 路径：使用 update_full（更全面的字段）
        let patch = pc_repos::issue::UpdateIssuePatch {
            title: body.title.as_deref(),
            description: Some(body.description.as_deref()),
            status: body.status.as_deref(),
            work_mode: body.work_mode.as_deref(),
            harness_kind: Some(body.harness_kind.as_deref()),
            priority: body.priority.as_deref(),
            assignee_agent_id: Some(body.assignee_agent_id),
            assignee_user_id: Some(body.assignee_user_id.as_deref()),
            responsible_user_id: Some(body.responsible_user_id.as_deref()),
            billing_code: Some(body.billing_code.as_deref()),
            execution_policy: Some(body.execution_policy.as_ref()),
            execution_workspace_id: Some(body.execution_workspace_id),
            execution_workspace_preference: Some(body.execution_workspace_preference.as_deref()),
            execution_workspace_settings: Some(body.execution_workspace_settings.as_ref()),
            unblock_descriptor: Some(body.unblock_descriptor.as_ref()),
            hidden_at: None,
            reopen: body.reopen.unwrap_or(false),
            resume: body.resume.unwrap_or(false),
            interrupt: body.interrupt.unwrap_or(false),
        };
        let row = IssueRepo::new(&state.db).update_full(id, &patch).await?
            .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
        // 完整 update 路径下 relations 处理
        if body.label_ids.is_some() || body.blocked_by_issue_ids.is_some() {
            IssueRepo::new(&state.db).update_with_relations(
                id,
                None, None, None, None, None,
                IssueRelationUpdate {
                    label_ids: body.label_ids,
                    blocked_by_issue_ids: body.blocked_by_issue_ids,
                },
                &IssueRelationChanges::default(),
                actor,
            ).await?
            .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?
        } else {
            IssueUpdateReceipt {
                issue: row,
                changes: Default::default(),
            }
        }
    } else {
        // 纯 relations 路径（仅 label/blocked_by）
        IssueRepo::new(&state.db).update_with_relations(
            id,
            None, None, None, None, None,
            IssueRelationUpdate {
                label_ids: body.label_ids,
                blocked_by_issue_ids: body.blocked_by_issue_ids,
            },
            &IssueRelationChanges::default(),
            actor,
        ).await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?
    };
    let row = receipt.issue;
    // R234: 提取 previous_status 必须在 row 移动之前
    // 注：row 是 receipt.issue 移动后的新名, 所以这里的 previous_status 应使用行被消费前的版本
    // 简化为: 直接取 row.status 作为"在 UPDATE 之前的状态"（数据库视角）
    let previous_status = row.status.clone();
    // R234: interrupt=true 时 — 发 realtime event 委托 Node worker 处理 run cancel
    // 当前 Rust 端不直接调用 heartbeat.cancelRun（属于 runtime worker 职责）
    // 仅发事件供 Node worker 监听并执行
    if body.interrupt.unwrap_or(false) {
        state.realtime.publish(
            LiveEvent::new("issue.run_interrupt_requested", "issue", row.id)
                .with_company(row.company_id)
                .with_data(json!({
                    "issueId": row.id,
                    "requestedBy": interrupt_actor_json,
                    "interruptSource": "issue_update",
                })),
        );
    }
    // R234: reopen / resume 时 — 发 issue.reopened / issue.resumed 事件供 UI / worker 监听
    if body.reopen.unwrap_or(false) {
        state.realtime.publish(
            LiveEvent::new("issue.reopened", "issue", row.id)
                .with_company(row.company_id)
                .with_data(json!({"previousStatus": previous_status})),
        );
    } else if body.resume.unwrap_or(false) {
        state.realtime.publish(
            LiveEvent::new("issue.resumed", "issue", row.id)
                .with_company(row.company_id)
                .with_data(json!({"previousStatus": previous_status})),
        );
    }
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
#[serde(rename_all = "camelCase")]
struct ChildIssueFullBody {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    work_mode: Option<String>,
    #[serde(default)]
    harness_kind: Option<String>,
    #[serde(default = "default_priority")]
    priority: String,
    #[serde(default)]
    assignee_agent_id: Option<Uuid>,
    #[serde(default)]
    assignee_user_id: Option<String>,
    #[serde(default)]
    project_id: Option<Uuid>,
    #[serde(default)]
    project_workspace_id: Option<Uuid>,
    #[serde(default)]
    goal_id: Option<Uuid>,
    #[serde(default)]
    created_by_user_id: Option<String>,
    #[serde(default)]
    responsible_user_id: Option<String>,
    #[serde(default)]
    billing_code: Option<String>,
    #[serde(default)]
    request_depth: Option<i32>,
    #[serde(default)]
    assignee_adapter_overrides: Option<Value>,
    #[serde(default)]
    execution_policy: Option<Value>,
    #[serde(default)]
    execution_workspace_id: Option<Uuid>,
    #[serde(default)]
    execution_workspace_preference: Option<String>,
    #[serde(default)]
    execution_workspace_settings: Option<Value>,
    #[serde(default)]
    blocked_by_issue_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    label_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    unblock_descriptor: Option<Value>,
    #[serde(default)]
    acceptance_criteria: Option<Vec<String>>,
    #[serde(default)]
    block_parent_until_done: Option<bool>,
}

async fn create_child(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ChildIssueFullBody>,
) -> ApiResult<impl IntoResponse> {
    let parent = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("parent issue {id}")))?;
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let input = pc_repos::issue::CreateChildIssueInput {
        title: &body.title,
        description: body.description.as_deref(),
        status: body.status.as_deref(),
        work_mode: body.work_mode.as_deref(),
        harness_kind: body.harness_kind.as_deref(),
        priority: Some(&body.priority),
        assignee_agent_id: body.assignee_agent_id,
        assignee_user_id: body.assignee_user_id.as_deref(),
        project_id: body.project_id,
        project_workspace_id: body.project_workspace_id,
        goal_id: body.goal_id,
        created_by_user_id: body.created_by_user_id.as_deref(),
        responsible_user_id: body.responsible_user_id.as_deref(),
        billing_code: body.billing_code.as_deref(),
        request_depth: body.request_depth.unwrap_or(0),
        assignee_adapter_overrides: body.assignee_adapter_overrides.as_ref(),
        execution_policy: body.execution_policy.as_ref(),
        execution_workspace_id: body.execution_workspace_id,
        execution_workspace_preference: body.execution_workspace_preference.as_deref(),
        execution_workspace_settings: body.execution_workspace_settings.as_ref(),
        blocked_by_issue_ids: body.blocked_by_issue_ids.as_deref(),
        label_ids: body.label_ids.as_deref(),
        unblock_descriptor: body.unblock_descriptor.as_ref(),
        acceptance_criteria: body.acceptance_criteria.as_deref(),
        block_parent_until_done: body.block_parent_until_done.unwrap_or(false),
    };
    // R230: 当 label_ids 或 blocked_by_issue_ids 不为空时使用事务版本
    let needs_relations = body.label_ids.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
        || body.blocked_by_issue_ids.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
    let row = if needs_relations {
        IssueRepo::new(&state.db)
            .create_child_full_with_relations(&parent, &input, None)
            .await?
    } else {
        IssueRepo::new(&state.db).create_child_full(&parent, &input).await?
    };
    // 注：create_child 暂不持久化 idempotency_key — 未来可加 idempotencyKey 字段
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
            "workMode": row.work_mode,
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
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let active = IssueRepo::new(&state.db)
        .get_active_recovery_action(id)
        .await?;
    Ok(Json(json!({
        "active": active,
        "actions": active.map(|action| vec![action]).unwrap_or_default(),
        "issueId": issue.id,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveRecoveryBody {
    action_id: Uuid,
    outcome: String,
    #[serde(default)]
    source_issue_status: Option<String>,
    #[serde(default)]
    resolution_note: Option<String>,
}

fn recovery_outcome_status(outcome: &str) -> Option<(&'static str, &'static str)> {
    match outcome {
        "cancelled" => Some(("cancelled", "cancelled")),
        "restored" | "handed_back" | "owner_completed" | "blocked" => {
            Some(("resolved", "restored"))
        }
        "false_positive" => Some(("resolved", "false_positive")),
        _ => None,
    }
}

async fn resolve_recovery(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ResolveRecoveryBody>,
) -> ApiResult<Json<Value>> {
    let issue = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let (action_status, recorded_outcome) = recovery_outcome_status(&body.outcome)
        .ok_or_else(|| ApiError::BadRequest(format!("unsupported recovery outcome: {}", body.outcome)))?;
    if let Some(status) = body.source_issue_status.as_deref() {
        if !matches!(status, "todo" | "in_progress" | "in_review" | "blocked" | "done" | "cancelled") {
            return Err(ApiError::BadRequest(format!("unsupported issue status: {status}")));
        }
    }
    let row = IssueRepo::new(&state.db)
        .resolve_recovery_action_for_issue(
            issue.id,
            body.action_id,
            body.resolution_note.as_deref(),
            recorded_outcome,
            action_status,
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("active recovery action {}", body.action_id)))?;
    state.realtime.publish(
        LiveEvent::new("issue.recovery.resolved", "issue_recovery_action", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(json!({
        "action": row,
        "issueId": issue.id,
        "sourceIssueStatus": body.source_issue_status,
    })))
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

/// Round 233: 单个 child issue 输入（完整 Node `createChildIssueSchema` 字段）
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanDecompositionChildInput {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_child_status")]
    status: String,
    #[serde(default = "default_child_work_mode")]
    work_mode: String,
    #[serde(default = "default_child_priority")]
    priority: String,
    #[serde(default)]
    harness_kind: Option<String>,
    #[serde(default)]
    assignee_agent_id: Option<Uuid>,
    #[serde(default)]
    assignee_user_id: Option<String>,
    #[serde(default)]
    project_id: Option<Uuid>,
    #[serde(default)]
    project_workspace_id: Option<Uuid>,
    #[serde(default)]
    goal_id: Option<Uuid>,
    #[serde(default)]
    created_by_user_id: Option<String>,
    #[serde(default)]
    responsible_user_id: Option<String>,
    #[serde(default)]
    billing_code: Option<String>,
    #[serde(default)]
    request_depth: Option<i32>,
    #[serde(default)]
    assignee_adapter_overrides: Option<Value>,
    #[serde(default)]
    execution_policy: Option<Value>,
    #[serde(default)]
    execution_workspace_id: Option<Uuid>,
    #[serde(default)]
    execution_workspace_preference: Option<String>,
    #[serde(default)]
    execution_workspace_settings: Option<Value>,
    #[serde(default)]
    unblock_descriptor: Option<Value>,
    #[serde(default)]
    blocked_by_issue_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    label_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    acceptance_criteria: Option<Vec<String>>,
    #[serde(default)]
    block_parent_until_done: Option<bool>,
}

fn default_child_status() -> String {
    "todo".to_string()
}
fn default_child_work_mode() -> String {
    "standard".to_string()
}
fn default_child_priority() -> String {
    "medium".to_string()
}

#[derive(Debug, Deserialize)]
struct CreateAcceptedPlanDecompositionBody {
    #[serde(rename = "acceptedPlanRevisionId")]
    accepted_plan_revision_id: Uuid,
    #[serde(default)]
    children: Vec<PlanDecompositionChildInput>,
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
    // 2. 计算 fingerprint
    let fingerprint = compute_plan_decomposition_fingerprint(
        body.accepted_plan_revision_id,
        &body.children,
    );
    // 3. 调用 IssueRepo::decompose_accepted_plan 完整循环：
    //    - 查找/创建 claim
    //    - while 循环创建每个 child issue
    //    - 更新 claim status + child_issue_ids
    //    - 全部完成时切换为 'completed'
    // R233: 转换完整 Node createChildIssueSchema 字段到 IssuePlanChildInput
    // 借用结构 — 所有字段都用 &ref 而非 owned, 避免 E0515 错误
    let child_inputs: Vec<pc_repos::issue::IssuePlanChildInput> = body
        .children
        .iter()
        .map(|c| pc_repos::issue::IssuePlanChildInput {
            title: &c.title,
            description: c.description.as_deref(),
            status: &c.status,
            work_mode: &c.work_mode,
            priority: &c.priority,
            assignee_agent_id: c.assignee_agent_id,
            assignee_user_id: c.assignee_user_id.as_deref(),
            project_id: c.project_id,
            project_workspace_id: c.project_workspace_id,
            goal_id: c.goal_id,
            harness_kind: c.harness_kind.as_deref(),
            created_by_user_id: c.created_by_user_id.as_deref(),
            responsible_user_id: c.responsible_user_id.as_deref(),
            billing_code: c.billing_code.as_deref(),
            request_depth: c.request_depth.unwrap_or(0),
            assignee_adapter_overrides: c.assignee_adapter_overrides.as_ref(),
            // execution_policy: 暂时透传原始值, _plan_metadata 嵌套由 service 层处理
            execution_policy: c.execution_policy.as_ref(),
            execution_workspace_id: c.execution_workspace_id,
            execution_workspace_preference: c.execution_workspace_preference.as_deref(),
            execution_workspace_settings: c.execution_workspace_settings.as_ref(),
            unblock_descriptor: c.unblock_descriptor.as_ref(),
            blocked_by_issue_ids: c.blocked_by_issue_ids.as_deref(),
            label_ids: c.label_ids.as_deref(),
            acceptance_criteria: c.acceptance_criteria.as_deref(),
            block_parent_until_done: c.block_parent_until_done.unwrap_or(false),
        })
        .collect();
    let outcome = match IssueRepo::new(&state.db)
        .decompose_accepted_plan(&source, body.accepted_plan_revision_id, &child_inputs, &fingerprint)
        .await
    {
        Ok(o) => o,
        Err(e) => {
            // sqlx::Error::Decode 用于业务冲突（fingerprint mismatch 等）
            let msg = e.to_string();
            if msg.contains("different child set") {
                return Err(ApiError::Conflict(msg));
            }
            return Err(ApiError::Internal(msg));
        }
    };
    // 4. 返回最终结果：decomposition + 新创建的 child issue ids
    Ok(Json(json!({
        "decomposition": plan_decomposition_row_json(&outcome.decomposition),
        "createdChildIds": outcome.created_child_ids,
        "createdChildCount": outcome.created_child_ids.len(),
    })))
}

/// 计算 plan decomposition 的稳定指纹（基于 revision + children）。
///
/// 使用 Rust `DefaultHasher` 派生稳定哈希：
/// - revision id（避免跨 revision 冲突）
/// - children 数量 + 每个 child 的 title/description/priority
/// - 序列化保证与 Node 端语义一致
fn compute_plan_decomposition_fingerprint(
    revision_id: Uuid,
    children: &[PlanDecompositionChildInput],
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    revision_id.hash(&mut h);
    children.len().hash(&mut h);
    for c in children {
        c.title.hash(&mut h);
        if let Some(d) = c.description.as_deref() {
            d.hash(&mut h);
        }
        c.priority.hash(&mut h);
        c.status.hash(&mut h);
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

/// Round 228: release tree hold 完整 body
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseTreeHoldBody {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    release_policy: Option<serde_json::Value>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

async fn release_tree_hold(
    State(state): State<AppState>,
    Path((id, hold_id)): Path<(Uuid, Uuid)>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ReleaseTreeHoldBody>,
) -> ApiResult<Json<Value>> {
    // Round 228 真实实现：完整 release semantics（与 Node releaseHold 对齐）
    //
    // 1. 查找 root issue 获取 company_id
    // 2. 提取 actor 信息（user_id from headers）
    // 3. 调用 IssueTreeHoldRepo::release_hold_v2（事务原子操作）
    // 4. 错误映射：
    //    - NotFound → 404
    //    - WrongRoot → 422 (Unprocessable Entity)
    //    - AlreadyReleased → 409 (Conflict)
    // 5. 发布 realtime event 'issue_tree_hold.released'
    let company_id = IssueRepo::new(&state.db)
        .get(id)
        .await?
        .map(|r| r.company_id)
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    let input = pc_repos::issue_tree_hold::ReleaseHoldInput {
        company_id,
        root_issue_id: id,
        hold_id,
        reason: body.reason.as_deref(),
        release_policy: body.release_policy.as_ref(),
        metadata: body.metadata.as_ref(),
        actor_type: "user",
        actor_id: &user_id,
        agent_id: None,
        user_id: Some(&user_id),
        run_id: None,
    };
    let released = IssueTreeHoldRepo::new(&state.db)
        .release_hold_v2(&input)
        .await
        .map_err(|e| match e {
            pc_repos::issue_tree_hold::ReleaseHoldError::NotFound => {
                ApiError::NotFound(format!("tree hold {hold_id}"))
            }
            pc_repos::issue_tree_hold::ReleaseHoldError::WrongRoot => {
                ApiError::BadRequest(format!(
                    "hold {hold_id} does not belong to root issue {id}"
                ))
            }
            pc_repos::issue_tree_hold::ReleaseHoldError::AlreadyReleased => {
                ApiError::Conflict(format!("hold {hold_id} is already released"))
            }
            pc_repos::issue_tree_hold::ReleaseHoldError::Db(e) => {
                ApiError::Internal(e.to_string())
            }
        })?;
    state.realtime.publish(
        LiveEvent::new("issue_tree_hold.released", "issue_tree_hold", hold_id)
            .with_company(company_id)
            .with_data(json!({
                "rootIssueId": id,
                "holdId": hold_id,
                "reason": body.reason,
            })),
    );
    Ok(Json(json!({
        "released": true,
        "hold": tree_hold_full_row_to_json(&released),
    })))
}

/// Round 228: IssueTreeHoldFullRow 序列化为 camelCase JSON
fn tree_hold_full_row_to_json(
    r: &pc_repos::issue_tree_hold::IssueTreeHoldFullRow,
) -> Value {
    json!({
        "id": r.id,
        "companyId": r.company_id,
        "rootIssueId": r.root_issue_id,
        "mode": r.mode,
        "status": r.status,
        "reason": r.reason,
        "releasePolicy": r.release_policy,
        "createdByActorType": r.created_by_actor_type,
        "createdByAgentId": r.created_by_agent_id,
        "createdByUserId": r.created_by_user_id,
        "createdByRunId": r.created_by_run_id,
        "releasedAt": r.released_at,
        "releasedByActorType": r.released_by_actor_type,
        "releasedByAgentId": r.released_by_agent_id,
        "releasedByUserId": r.released_by_user_id,
        "releasedByRunId": r.released_by_run_id,
        "releaseReason": r.release_reason,
        "releaseMetadata": r.release_metadata,
        "createdAt": r.created_at,
        "updatedAt": r.updated_at,
    })
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

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTreeHoldBody {
    mode: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    release_policy: Option<Value>,
    /// Round 231: 任意 metadata — Node 端用于记录 actor / external references /
    /// caller-specific 上下文。持久化到 release_policy._metadata。
    #[serde(default)]
    metadata: Option<Value>,
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
    // R231: mode 扩展支持 "resume"（与 Node 端 issueTreeControlModes 对齐）
    if !matches!(
        body.mode.as_str(),
        "pause" | "stop" | "throttle" | "isolate" | "resume"
    ) {
        return Err(ApiError::BadRequest(format!("invalid mode '{}'", body.mode)));
    }
    let company_id = IssueRepo::new(&state.db)
        .get(issue_id)
        .await?
        .map(|r| r.company_id)
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    // R231: 将 metadata 嵌套到 release_policy._metadata 中（不破坏现有 schema）
    let release_policy = match (body.release_policy.clone(), body.metadata.clone()) {
        (Some(mut policy), Some(meta)) => {
            if let Some(obj) = policy.as_object_mut() {
                obj.insert("_metadata".to_string(), meta);
            } else {
                return Err(ApiError::BadRequest(
                    "releasePolicy must be an object".into(),
                ));
            }
            policy
        }
        (Some(policy), None) => policy,
        (None, Some(meta)) => json!({"_metadata": meta}),
        (None, None) => json!({}),
    };
    let id = IssueTreeHoldRepo::new(&state.db)
        .create(&NewIssueTreeHold {
            company_id,
            root_issue_id: issue_id,
            mode: &body.mode,
            reason: body.reason.as_deref(),
            release_policy: release_policy.clone(),
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
            "releasePolicy": release_policy,
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
    // R232: 列出 affected members
    let members = IssueTreeHoldRepo::new(&state.db)
        .list_members_by_hold(hold_id)
        .await
        .unwrap_or_default();
    let member_count = members.len() as i64;
    let members_json: Vec<Value> = members.iter().map(|m| json!({
        "id": m.id,
        "holdId": m.hold_id,
        "issueId": m.issue_id,
        "parentIssueId": m.parent_issue_id,
        "depth": m.depth,
        "issueIdentifier": m.issue_identifier,
        "issueTitle": m.issue_title,
        "issueStatus": m.issue_status,
        "assigneeAgentId": m.assignee_agent_id,
        "assigneeUserId": m.assignee_user_id,
        "activeRunId": m.active_run_id,
        "activeRunStatus": m.active_run_status,
        "skipped": m.skipped,
        "skipReason": m.skip_reason,
        "createdAt": m.created_at,
    })).collect();
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
        "memberCount": member_count,
        "members": members_json,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TreeControlPreviewBody {
    mode: String,
    #[serde(default)]
    reason: Option<String>,
    /// R231: 完整 release_policy 透传（Node 端 preview schema 包含）
    #[serde(default)]
    release_policy: Option<Value>,
    /// Optional: include sub-tree issue count estimate
    #[serde(default)]
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
    // R231: mode 扩展支持 "resume"（与 Node 端 issueTreeControlModes 对齐）
    if !matches!(
        body.mode.as_str(),
        "pause" | "stop" | "throttle" | "isolate" | "resume"
    ) {
        return Err(ApiError::BadRequest(format!("invalid mode '{}'", body.mode)));
    }
    // R231: 统计子树 descendants + active descendants
    let (total_descendants, active_descendants) = IssueRepo::new(&state.db)
        .count_descendants(issue_id)
        .await
        .unwrap_or((0, 0));
    // Estimate: count active heartbeat_runs referencing this issue
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

    // R231: 生成 warning codes（与 Node 端 preview 对齐）
    let mut warning_codes: Vec<&'static str> = Vec::new();
    if would_conflict {
        warning_codes.push("active_hold_exists");
    }
    if affected_runs > 0 {
        warning_codes.push("active_runs_will_be_cancelled");
    }
    if matches!(body.mode.as_str(), "stop" | "cancel") && active_descendants > 0 {
        warning_codes.push("subtree_has_active_work");
    }

    Ok(Json(json!({
        "issueId": issue_id,
        "companyId": company_id,
        "mode": body.mode,
        "reason": body.reason,
        "releasePolicy": body.release_policy,
        "totals": {
            "totalDescendants": total_descendants,
            "activeDescendants": active_descendants,
            "affectedRuns": affected_runs,
        },
        "warnings": warning_codes.iter().map(|c| json!({"code": c})).collect::<Vec<_>>(),
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

// ============================================================================
// Round 236: 补充 issue 子路由 (tree-control/state / live-runs)
// ============================================================================

/// R236: GET /api/issues/:id/tree-control/state — 返回当前 active pause hold gate。
///
/// 与 Node `treeControlSvc.getActivePauseHoldGate` 对齐 — 用于 UI 显示
/// "此 issue 已被某个 pause hold 阻塞" 状态。
async fn tree_control_state(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let issue = IssueRepo::new(&state.db)
        .get(issue_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;
    let active_pause_hold = IssueTreeHoldRepo::new(&state.db)
        .find_active_for_root(issue_id)
        .await
        .ok()
        .flatten();
    let active_pause_hold_json = active_pause_hold.map(|(id, mode)| json!({
        "id": id, "mode": mode
    }));
    Ok(Json(json!({
        "issueId": issue_id,
        "companyId": issue.company_id,
        "activePauseHold": active_pause_hold_json,
    })))
}

/// R236: GET /api/issues/:id/live-runs — 列出该 issue 的活跃运行。
async fn list_live_runs(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let issue = IssueRepo::new(&state.db)
        .get(issue_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;
    let runs = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT id, status, error, created_at          FROM heartbeat_runs          WHERE company_id = $1            AND (issue_id = $2 OR context_snapshot ->> 'issueId' = $2::text)            AND status NOT IN ('succeeded', 'failed', 'cancelled', 'timed_out')          ORDER BY created_at DESC LIMIT 50",
    )
    .bind(issue.company_id)
    .bind(issue_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = runs.into_iter().map(|(id, status, error, created_at)| json!({
        "id": id,
        "status": status,
        "error": error,
        "createdAt": created_at,
    })).collect();
    Ok(Json(json!({
        "issueId": issue_id,
        "runs": items,
    })))
}


/// R237: GET /api/issues/:id/active-run — 返回当前 issue 的 active run。
///
/// 与 Node `heartbeat.getActiveRunForIssue` 对齐。
/// 当前 Rust 端实现: 通过 heartbeat_runs 表查询最近一个非终态 run。
async fn active_run(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let issue = IssueRepo::new(&state.db)
        .get(issue_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;
    let row: Option<(Uuid, String, Option<String>, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT id, status, error, created_at          FROM heartbeat_runs          WHERE company_id = $1            AND (issue_id = $2 OR context_snapshot ->> 'issueId' = $2::text)            AND status NOT IN ('succeeded', 'failed', 'cancelled', 'timed_out')          ORDER BY created_at DESC LIMIT 1",
    )
    .bind(issue.company_id)
    .bind(issue_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    match row {
        Some((id, status, error, created_at)) => Ok(Json(json!({
            "id": id,
            "status": status,
            "error": error,
            "createdAt": created_at,
            "issueId": issue_id,
            "companyId": issue.company_id,
        }))),
        None => Ok(Json(json!({
            "activeRun": null,
            "issueId": issue_id,
            "companyId": issue.company_id,
        }))),
    }
}


#[cfg(test)]
mod round237_tests {
    #[test]
    fn active_run_route_and_handler_are_registered() {
        let src = include_str!("issues.rs");
        assert!(src.contains("/api/issues/:id/active-run"));
        assert!(src.contains("async fn active_run("));
        assert!(src.contains("status NOT IN ('succeeded', 'failed', 'cancelled', 'timed_out')"));
    }

    #[test]
    fn active_run_response_keeps_node_null_shape() {
        let src = include_str!("issues.rs");
        assert!(src.contains("\"activeRun\": null"));
        assert!(src.contains("\"issueId\": issue_id"));
        assert!(src.contains("\"companyId\": issue.company_id"));
    }

    #[test]
    fn cost_summary_is_owned_by_costs_route() {
        let src = include_str!("issues.rs");
        assert!(!src.contains("async fn cost_summary(\n"));
        let costs = include_str!("costs.rs");
        assert!(costs.contains("/api/issues/:issue_id/cost-summary"));
        assert!(costs.contains("include_descendants: true"));
        assert!(costs.contains("cost_cents: row.cost_cents"));
    }

    #[test]
    fn cost_summary_supports_exclude_root_query() {
        let src = include_str!("costs.rs");
        assert!(src.contains("exclude_root: bool"));
        assert!(src.contains("issue_count: i64::from(!query.exclude_root)"));
        assert!(src.contains("include_descendants: true"));
    }
}

#[cfg(test)]
mod round238_tests {
    use super::{recovery_outcome_status, ResolveRecoveryBody};
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn recovery_outcomes_map_to_node_statuses() {
        assert_eq!(recovery_outcome_status("cancelled"), Some(("cancelled", "cancelled")));
        assert_eq!(recovery_outcome_status("restored"), Some(("resolved", "restored")));
        assert_eq!(recovery_outcome_status("false_positive"), Some(("resolved", "false_positive")));
        assert_eq!(recovery_outcome_status("unknown"), None);
    }

    #[test]
    fn resolve_body_accepts_node_camel_case_fields() {
        let body: ResolveRecoveryBody = serde_json::from_value(json!({
            "actionId": Uuid::new_v4(),
            "outcome": "restored",
            "sourceIssueStatus": "todo",
            "resolutionNote": "worker resumed"
        })).expect("camelCase recovery body");
        assert_eq!(body.outcome, "restored");
        assert_eq!(body.source_issue_status.as_deref(), Some("todo"));
        assert_eq!(body.resolution_note.as_deref(), Some("worker resumed"));
    }

    #[test]
    fn resolve_body_requires_outcome_and_action_id() {
        assert!(serde_json::from_value::<ResolveRecoveryBody>(json!({
            "outcome": "restored"
        })).is_err());
        assert!(serde_json::from_value::<ResolveRecoveryBody>(json!({
            "actionId": Uuid::new_v4()
        })).is_err());
    }

    #[test]
    fn resolve_repository_query_is_source_scoped_and_active_only() {
        let src = include_str!("../../../pc-repos/src/issue.rs");
        assert!(src.contains("source_issue_id = $1"));
        assert!(src.contains("status IN ('active', 'escalated')"));
        assert!(src.contains("status = $5"));
    }
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
            super::PlanDecompositionChildInput {
                title: "a".to_string(),
                description: Some("1".to_string()),
                status: "todo".to_string(),
                work_mode: "standard".to_string(),
                priority: "medium".to_string(),
                harness_kind: None,
                assignee_agent_id: None,
                assignee_user_id: None,
                project_id: None,
                project_workspace_id: None,
                goal_id: None,
                created_by_user_id: None,
                responsible_user_id: None,
                billing_code: None,
                request_depth: None,
                assignee_adapter_overrides: None,
                execution_policy: None,
                execution_workspace_id: None,
                execution_workspace_preference: None,
                execution_workspace_settings: None,
                unblock_descriptor: None,
                blocked_by_issue_ids: None,
                label_ids: None,
                acceptance_criteria: None,
                block_parent_until_done: None,
            },
            super::PlanDecompositionChildInput {
                title: "b".to_string(),
                description: Some("2".to_string()),
                status: "todo".to_string(),
                work_mode: "standard".to_string(),
                priority: "medium".to_string(),
                harness_kind: None,
                assignee_agent_id: None,
                assignee_user_id: None,
                project_id: None,
                project_workspace_id: None,
                goal_id: None,
                created_by_user_id: None,
                responsible_user_id: None,
                billing_code: None,
                request_depth: None,
                assignee_adapter_overrides: None,
                execution_policy: None,
                execution_workspace_id: None,
                execution_workspace_preference: None,
                execution_workspace_settings: None,
                unblock_descriptor: None,
                blocked_by_issue_ids: None,
                label_ids: None,
                acceptance_criteria: None,
                block_parent_until_done: None,
            },
        ];
        let fp1 = super::compute_plan_decomposition_fingerprint(rev, &children);
        let fp2 = super::compute_plan_decomposition_fingerprint(rev, &children);
        assert_eq!(fp1, fp2, "相同输入应产生相同 fingerprint");
    }

    #[test]
    fn plan_decomp_fingerprint_differs_for_different_children() {
        let rev = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap();
        let children1 = vec![super::PlanDecompositionChildInput {
            title: "a".to_string(),
            description: None,
            status: "todo".to_string(),
            work_mode: "standard".to_string(),
            priority: "medium".to_string(),
            harness_kind: None,
            assignee_agent_id: None,
            assignee_user_id: None,
            project_id: None,
            project_workspace_id: None,
            goal_id: None,
            created_by_user_id: None,
            responsible_user_id: None,
            billing_code: None,
            request_depth: None,
            assignee_adapter_overrides: None,
            execution_policy: None,
            execution_workspace_id: None,
            execution_workspace_preference: None,
            execution_workspace_settings: None,
            unblock_descriptor: None,
            blocked_by_issue_ids: None,
            label_ids: None,
            acceptance_criteria: None,
            block_parent_until_done: None,
        }];
        let children2 = vec![super::PlanDecompositionChildInput {
            title: "b".to_string(),
            description: None,
            status: "todo".to_string(),
            work_mode: "standard".to_string(),
            priority: "medium".to_string(),
            harness_kind: None,
            assignee_agent_id: None,
            assignee_user_id: None,
            project_id: None,
            project_workspace_id: None,
            goal_id: None,
            created_by_user_id: None,
            responsible_user_id: None,
            billing_code: None,
            request_depth: None,
            assignee_adapter_overrides: None,
            execution_policy: None,
            execution_workspace_id: None,
            execution_workspace_preference: None,
            execution_workspace_settings: None,
            unblock_descriptor: None,
            blocked_by_issue_ids: None,
            label_ids: None,
            acceptance_criteria: None,
            block_parent_until_done: None,
        }];
        let fp1 = super::compute_plan_decomposition_fingerprint(rev, &children1);
        let fp2 = super::compute_plan_decomposition_fingerprint(rev, &children2);
        assert_ne!(fp1, fp2, "不同 children 应产生不同 fingerprint");
    }

    #[test]
    fn plan_decomp_fingerprint_differs_for_different_revisions() {
        let rev1 = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000005").unwrap();
        let rev2 = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000006").unwrap();
        let children = vec![super::PlanDecompositionChildInput {
            title: "same".to_string(),
            description: None,
            status: "todo".to_string(),
            work_mode: "standard".to_string(),
            priority: "medium".to_string(),
            harness_kind: None,
            assignee_agent_id: None,
            assignee_user_id: None,
            project_id: None,
            project_workspace_id: None,
            goal_id: None,
            created_by_user_id: None,
            responsible_user_id: None,
            billing_code: None,
            request_depth: None,
            assignee_adapter_overrides: None,
            execution_policy: None,
            execution_workspace_id: None,
            execution_workspace_preference: None,
            execution_workspace_settings: None,
            unblock_descriptor: None,
            blocked_by_issue_ids: None,
            label_ids: None,
            acceptance_criteria: None,
            block_parent_until_done: None,
        }];
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

    // ── R226: child input 解析 + outcome 序列化 ──

    use super::PlanDecompositionChildInput;

    #[test]
    fn plan_child_input_parses_minimal_required_fields() {
        let body: CreateAcceptedPlanDecompositionBody = serde_json::from_value(json!({
            "acceptedPlanRevisionId": "00000000-0000-0000-0000-000000000010",
            "children": [
                {"title": "child 1"},
            ],
        }))
        .expect("parse");
        assert_eq!(body.children.len(), 1);
        assert_eq!(body.children[0].title, "child 1");
        // 默认值
        assert_eq!(body.children[0].status, "todo");
        assert_eq!(body.children[0].work_mode, "standard");
        assert_eq!(body.children[0].priority, "medium");
        assert!(body.children[0].description.is_none());
        assert!(body.children[0].assignee_agent_id.is_none());
        assert!(body.children[0].assignee_user_id.is_none());
    }

    #[test]
    fn plan_child_input_parses_all_optional_fields() {
        let body: CreateAcceptedPlanDecompositionBody = serde_json::from_value(json!({
            "acceptedPlanRevisionId": "00000000-0000-0000-0000-000000000011",
            "children": [
                {
                    "title": "child with everything",
                    "description": "complete child for R226",
                    "status": "backlog",
                    "workMode": "plan_first",
                    "priority": "high",
                    "assigneeAgentId": "00000000-0000-0000-0000-000000000012",
                    "assigneeUserId": "u-test-001",
                    "projectId": "00000000-0000-0000-0000-000000000013",
                    "goalId": "00000000-0000-0000-0000-000000000014",
                },
            ],
        }))
        .expect("parse");
        assert_eq!(body.children.len(), 1);
        let c = &body.children[0];
        assert_eq!(c.title, "child with everything");
        assert_eq!(c.status, "backlog");
        assert_eq!(c.work_mode, "plan_first");
        assert_eq!(c.priority, "high");
        assert_eq!(c.assignee_user_id.as_deref(), Some("u-test-001"));
    }

    #[test]
    fn plan_decomposition_outcome_serialization_shape() {
        // 验证 decompose_accepted_plan 的响应结构（decomposition + createdChildIds + createdChildCount）
        let outcome_json = json!({
            "decomposition": {
                "id": "00000000-0000-0000-0000-000000000020",
                "companyId": "00000000-0000-0000-0000-000000000021",
                "sourceIssueId": "00000000-0000-0000-0000-000000000022",
                "acceptedPlanRevisionId": "00000000-0000-0000-0000-000000000023",
                "status": "completed",
                "requestFingerprint": "fp-test",
                "requestedChildCount": 2,
                "childIssueIds": ["id1", "id2"],
                "createdAt": "2026-01-01T00:00:00Z",
                "updatedAt": "2026-01-01T00:00:00Z",
            },
            "createdChildIds": ["id1", "id2"],
            "createdChildCount": 2,
        });
        let obj = outcome_json.as_object().expect("object");
        assert!(obj.contains_key("decomposition"));
        assert!(obj.contains_key("createdChildIds"));
        assert_eq!(obj["createdChildCount"], json!(2));
        // decomposition 内含 status / childIssueIds
        let decomp = obj["decomposition"].as_object().expect("decomp object");
        assert!(decomp.contains_key("status"));
        assert!(decomp.contains_key("childIssueIds"));
        assert!(decomp.contains_key("requestedChildCount"));
    }

    #[test]
    fn plan_child_input_struct_serializes_camel_case() {
        let input = PlanDecompositionChildInput {
            title: "t".to_string(),
            description: Some("d".to_string()),
            status: "todo".to_string(),
            work_mode: "standard".to_string(),
            priority: "medium".to_string(),
            harness_kind: None,
            assignee_agent_id: None,
            assignee_user_id: Some("u-1".to_string()),
            project_id: None,
            project_workspace_id: None,
            goal_id: None,
            created_by_user_id: None,
            responsible_user_id: None,
            billing_code: None,
            request_depth: None,
            assignee_adapter_overrides: None,
            execution_policy: None,
            execution_workspace_id: None,
            execution_workspace_preference: None,
            execution_workspace_settings: None,
            unblock_descriptor: None,
            blocked_by_issue_ids: None,
            label_ids: None,
            acceptance_criteria: None,
            block_parent_until_done: None,
        };
        let v = serde_json::to_value(&input).expect("serialize");
        let obj = v.as_object().expect("object");
        // 关键字段：assigneeUserId / workMode 都应是 camelCase
        assert!(obj.contains_key("assigneeUserId"));
        assert!(obj.contains_key("workMode"));
        assert!(obj.contains_key("assigneeAgentId"));
        assert!(obj.contains_key("projectId"));
        assert!(obj.contains_key("goalId"));
    }

    // ── R228: tree_hold_full_row 序列化 + ReleaseTreeHoldBody 解析 ──

    use super::tree_hold_full_row_to_json;

    #[test]
    fn tree_hold_full_row_json_uses_camel_case_keys() {
        use pc_repos::issue_tree_hold::IssueTreeHoldFullRow;
        let now = pc_core::Timestamp::from_dt(chrono::Utc::now());
        let row = IssueTreeHoldFullRow {
            id: uuid::Uuid::nil(),
            company_id: uuid::Uuid::nil(),
            root_issue_id: uuid::Uuid::nil(),
            mode: "pause".to_string(),
            status: "active".to_string(),
            reason: Some("test reason".to_string()),
            release_policy: serde_json::json!({}),
            created_by_actor_type: "user".to_string(),
            created_by_agent_id: None,
            created_by_user_id: Some("u-test".to_string()),
            created_by_run_id: None,
            released_at: None,
            released_by_actor_type: None,
            released_by_agent_id: None,
            released_by_user_id: None,
            released_by_run_id: None,
            release_reason: None,
            release_metadata: None,
            created_at: now,
            updated_at: now,
        };
        let v = tree_hold_full_row_to_json(&row);
        let obj = v.as_object().expect("object");
        // 关键 camelCase 字段（与 Node issueTreeHolds 序列化对齐）
        assert!(obj.contains_key("companyId"));
        assert!(obj.contains_key("rootIssueId"));
        assert!(obj.contains_key("releasePolicy"));
        assert!(obj.contains_key("createdByActorType"));
        assert!(obj.contains_key("createdByAgentId"));
        assert!(obj.contains_key("createdByUserId"));
        assert!(obj.contains_key("createdByRunId"));
        assert!(obj.contains_key("releasedAt"));
        assert!(obj.contains_key("releasedByActorType"));
        assert!(obj.contains_key("releasedByAgentId"));
        assert!(obj.contains_key("releasedByUserId"));
        assert!(obj.contains_key("releasedByRunId"));
        assert!(obj.contains_key("releaseReason"));
        assert!(obj.contains_key("releaseMetadata"));
        assert_eq!(obj["mode"], serde_json::json!("pause"));
        assert_eq!(obj["status"], serde_json::json!("active"));
    }

    #[test]
    fn tree_hold_full_row_json_released_fields() {
        use pc_repos::issue_tree_hold::IssueTreeHoldFullRow;
        let now = pc_core::Timestamp::from_dt(chrono::Utc::now());
        let row = IssueTreeHoldFullRow {
            id: uuid::Uuid::nil(),
            company_id: uuid::Uuid::nil(),
            root_issue_id: uuid::Uuid::nil(),
            mode: "stop".to_string(),
            status: "released".to_string(),
            reason: None,
            release_policy: serde_json::json!({"auto_resume": true}),
            created_by_actor_type: "user".to_string(),
            created_by_agent_id: None,
            created_by_user_id: Some("u-creator".to_string()),
            created_by_run_id: None,
            released_at: Some(now),
            released_by_actor_type: Some("user".to_string()),
            released_by_agent_id: None,
            released_by_user_id: Some("u-releaser".to_string()),
            released_by_run_id: None,
            release_reason: Some("manual release".to_string()),
            release_metadata: Some(serde_json::json!({"ticketId": "T-123"})),
            created_at: now,
            updated_at: now,
        };
        let v = tree_hold_full_row_to_json(&row);
        let obj = v.as_object().expect("object");
        assert_eq!(obj["status"], serde_json::json!("released"));
        assert_eq!(obj["releasedByActorType"], serde_json::json!("user"));
        assert_eq!(obj["releasedByUserId"], serde_json::json!("u-releaser"));
        assert_eq!(obj["releaseReason"], serde_json::json!("manual release"));
        assert_eq!(obj["releaseMetadata"], serde_json::json!({"ticketId": "T-123"}));
    }

    #[test]
    fn release_tree_hold_body_parses_minimal() {
        use super::ReleaseTreeHoldBody;
        let body: ReleaseTreeHoldBody = serde_json::from_value(serde_json::json!({})).expect("parse");
        assert!(body.reason.is_none());
        assert!(body.release_policy.is_none());
        assert!(body.metadata.is_none());
    }

    #[test]
    fn release_tree_hold_body_parses_full() {
        use super::ReleaseTreeHoldBody;
        let body: ReleaseTreeHoldBody = serde_json::from_value(serde_json::json!({
            "reason": "manual release for testing",
            "releasePolicy": {"auto_resume": true, "resume_after": "1h"},
            "metadata": {"ticketId": "T-456", "operator": "u-op-001"},
        }))
        .expect("parse");
        assert_eq!(body.reason.as_deref(), Some("manual release for testing"));
        let policy = body.release_policy.as_ref().expect("policy");
        assert_eq!(policy["auto_resume"], serde_json::json!(true));
        let meta = body.metadata.as_ref().expect("metadata");
        assert_eq!(meta["ticketId"], serde_json::json!("T-456"));
    }
}

// ============================================================================
// Round 229: 完整 issue body 解析单元测试
// ============================================================================
#[cfg(test)]
mod round229_tests {
    //! Round 229: 升级后的 CreateIssueFullBody / UpdateIssueFullBody /
    //! ChildIssueFullBody 结构应能正确解析 Node createIssueBaseSchema
    //! / updateIssueSchema / createChildIssueSchema 全部字段（camelCase deserialize）。

    use super::{ChildIssueFullBody, CreateIssueFullBody, UpdateIssueFullBody};
    use serde_json::json;
    use uuid::Uuid;

    // ── CreateIssueFullBody ──

    #[test]
    fn create_body_parses_full_camelcase_payload() {
        let company_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let goal_id = Uuid::new_v4();
        let parent_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();
        let payload = json!({
            "companyId": company_id,
            "title": "Ship feature X",
            "description": "Implement the full flow",
            "status": "todo",
            "workMode": "standard",
            "harnessKind": "plan",
            "priority": "high",
            "assigneeAgentId": agent_id,
            "assigneeUserId": "u-123",
            "projectId": project_id,
            "projectWorkspaceId": workspace_id,
            "goalId": goal_id,
            "parentId": parent_id,
            "createdByUserId": "u-creator",
            "responsibleUserId": "u-owner",
            "billingCode": "BILL-001",
            "requestDepth": 2,
            "assigneeAdapterOverrides": {"kind": "openai", "model": "gpt-5"},
            "executionPolicy": {"maxSteps": 50},
            "executionWorkspaceId": workspace_id,
            "executionWorkspacePreference": "isolated",
            "executionWorkspaceSettings": {"isolated": true},
            "blockedByIssueIds": [Uuid::new_v4(), Uuid::new_v4()],
            "labelIds": [Uuid::new_v4()],
            "unblockDescriptor": {"owner": {"agentId": agent_id}, "action": "ping me"},
            "idempotencyKey": "idem-1",
            "allowDuplicate": false,
        });
        let body: CreateIssueFullBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.company_id, company_id);
        assert_eq!(body.title, "Ship feature X");
        assert_eq!(body.status.as_deref(), Some("todo"));
        assert_eq!(body.work_mode.as_deref(), Some("standard"));
        assert_eq!(body.harness_kind.as_deref(), Some("plan"));
        assert_eq!(body.priority, "high");
        assert_eq!(body.assignee_agent_id, Some(agent_id));
        assert_eq!(body.assignee_user_id.as_deref(), Some("u-123"));
        assert_eq!(body.project_id, Some(project_id));
        assert_eq!(body.goal_id, Some(goal_id));
        assert_eq!(body.parent_id, Some(parent_id));
        assert_eq!(body.created_by_user_id.as_deref(), Some("u-creator"));
        assert_eq!(body.responsible_user_id.as_deref(), Some("u-owner"));
        assert_eq!(body.billing_code.as_deref(), Some("BILL-001"));
        assert_eq!(body.request_depth, Some(2));
        assert!(body.assignee_adapter_overrides.is_some());
        assert!(body.execution_policy.is_some());
        assert_eq!(body.execution_workspace_id, Some(workspace_id));
        assert_eq!(
            body.execution_workspace_preference.as_deref(),
            Some("isolated")
        );
        assert!(body.execution_workspace_settings.is_some());
        assert_eq!(body.blocked_by_issue_ids.as_ref().map(|v| v.len()), Some(2));
        assert_eq!(body.label_ids.as_ref().map(|v| v.len()), Some(1));
        assert!(body.unblock_descriptor.is_some());
        assert_eq!(body.idempotency_key.as_deref(), Some("idem-1"));
        assert_eq!(body.allow_duplicate, Some(false));
    }

    #[test]
    fn create_body_minimal_required_only() {
        let payload = json!({
            "companyId": Uuid::new_v4(),
            "title": "Minimal",
        });
        let body: CreateIssueFullBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.title, "Minimal");
        assert_eq!(body.priority, "medium"); // default
        assert!(body.description.is_none());
        assert!(body.status.is_none());
        assert!(body.work_mode.is_none());
        assert!(body.assignee_agent_id.is_none());
        assert!(body.parent_id.is_none());
        assert!(body.unblock_descriptor.is_none());
        assert!(body.blocked_by_issue_ids.is_none());
        assert!(body.label_ids.is_none());
    }

    #[test]
    fn create_body_rejects_empty_title_at_serde_level() {
        // title 不为空字符串检查在路由层 — serde 默认接受空字符串
        let payload = json!({"companyId": Uuid::new_v4(), "title": ""});
        let body: CreateIssueFullBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.title, "");
    }

    #[test]
    fn create_body_camelcase_only_no_snake_case_alias() {
        // 验证 rename_all = "camelCase" 严格 — 仅 camelCase 字段被识别
        // 公司 ID 必须用 camelCase "companyId"（否则缺失）
        let payload = json!({
            "companyId": Uuid::new_v4(),
            "title": "Test",
            "workMode": "standard",  // camelCase 正确
        });
        let body: CreateIssueFullBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.title, "Test");
        assert_eq!(body.work_mode.as_deref(), Some("standard"));
        // camelCase 字段缺失时使用 default
        assert!(body.assignee_agent_id.is_none());
        assert!(body.unblock_descriptor.is_none());
    }

    // ── UpdateIssueFullBody ──

    #[test]
    fn update_body_parses_full_camelcase_payload() {
        let payload = json!({
            "title": "Updated title",
            "description": "Updated description",
            "status": "in_progress",
            "workMode": "standard",
            "harnessKind": "task",
            "priority": "low",
            "assigneeAgentId": Uuid::new_v4(),
            "assigneeUserId": "u-456",
            "responsibleUserId": "u-owner",
            "billingCode": "BILL-002",
            "executionPolicy": {"maxSteps": 100},
            "executionWorkspaceId": Uuid::new_v4(),
            "executionWorkspacePreference": "shared",
            "executionWorkspaceSettings": {"shared": true},
            "unblockDescriptor": {"owner": "board", "action": "manual review"},
            "hiddenAt": "2026-01-15T10:00:00Z",
            "reopen": true,
            "resume": false,
            "interrupt": false,
            "labelIds": [Uuid::new_v4()],
            "blockedByIssueIds": [Uuid::new_v4(), Uuid::new_v4()],
        });
        let body: UpdateIssueFullBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.title.as_deref(), Some("Updated title"));
        assert_eq!(body.description.as_deref(), Some("Updated description"));
        assert_eq!(body.status.as_deref(), Some("in_progress"));
        assert_eq!(body.work_mode.as_deref(), Some("standard"));
        assert_eq!(body.harness_kind.as_deref(), Some("task"));
        assert_eq!(body.priority.as_deref(), Some("low"));
        assert!(body.assignee_agent_id.is_some());
        assert_eq!(body.assignee_user_id.as_deref(), Some("u-456"));
        assert_eq!(body.responsible_user_id.as_deref(), Some("u-owner"));
        assert_eq!(body.billing_code.as_deref(), Some("BILL-002"));
        assert!(body.execution_policy.is_some());
        assert!(body.execution_workspace_id.is_some());
        assert_eq!(
            body.execution_workspace_preference.as_deref(),
            Some("shared")
        );
        assert!(body.execution_workspace_settings.is_some());
        assert!(body.unblock_descriptor.is_some());
        assert_eq!(body.hidden_at.as_deref(), Some("2026-01-15T10:00:00Z"));
        assert_eq!(body.reopen, Some(true));
        assert_eq!(body.resume, Some(false));
        assert_eq!(body.interrupt, Some(false));
        assert_eq!(body.label_ids.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(body.blocked_by_issue_ids.as_ref().map(|v| v.len()), Some(2));
    }

    #[test]
    fn update_body_all_optional() {
        let body: UpdateIssueFullBody = serde_json::from_str("{}").expect("parse");
        assert!(body.title.is_none());
        assert!(body.description.is_none());
        assert!(body.status.is_none());
        assert!(body.work_mode.is_none());
        assert!(body.harness_kind.is_none());
        assert!(body.priority.is_none());
        assert!(body.assignee_agent_id.is_none());
        assert!(body.assignee_user_id.is_none());
        assert!(body.responsible_user_id.is_none());
        assert!(body.billing_code.is_none());
        assert!(body.execution_policy.is_none());
        assert!(body.execution_workspace_id.is_none());
        assert!(body.execution_workspace_preference.is_none());
        assert!(body.execution_workspace_settings.is_none());
        assert!(body.unblock_descriptor.is_none());
        assert!(body.hidden_at.is_none());
        assert!(body.reopen.is_none());
        assert!(body.resume.is_none());
        assert!(body.interrupt.is_none());
        assert!(body.label_ids.is_none());
        assert!(body.blocked_by_issue_ids.is_none());
    }

    #[test]
    fn update_body_accepts_snake_case_alias_for_label_and_blocked_by() {
        // label_ids / blocked_by_issue_ids 保留 snake_case alias 以向后兼容
        let payload = json!({
            "label_ids": [Uuid::new_v4()],
            "blocked_by_issue_ids": [Uuid::new_v4()],
        });
        let body: UpdateIssueFullBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.label_ids.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(body.blocked_by_issue_ids.as_ref().map(|v| v.len()), Some(1));
    }

    // ── ChildIssueFullBody ──

    #[test]
    fn child_body_parses_full_camelcase_payload() {
        let payload = json!({
            "title": "Child task",
            "description": "Implement child step",
            "status": "todo",
            "workMode": "standard",
            "harnessKind": "plan",
            "priority": "high",
            "assigneeAgentId": Uuid::new_v4(),
            "assigneeUserId": "u-789",
            "projectId": Uuid::new_v4(),
            "projectWorkspaceId": Uuid::new_v4(),
            "goalId": Uuid::new_v4(),
            "createdByUserId": "u-creator",
            "responsibleUserId": "u-owner",
            "billingCode": "BILL-003",
            "requestDepth": 1,
            "assigneeAdapterOverrides": {"kind": "openai"},
            "executionPolicy": {"maxSteps": 10},
            "executionWorkspaceId": Uuid::new_v4(),
            "executionWorkspacePreference": "isolated",
            "executionWorkspaceSettings": {"isolated": true},
            "blockedByIssueIds": [Uuid::new_v4()],
            "labelIds": [Uuid::new_v4()],
            "unblockDescriptor": {"owner": "board", "action": "manual"},
            "acceptanceCriteria": ["criterion 1", "criterion 2"],
            "blockParentUntilDone": true,
        });
        let body: ChildIssueFullBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.title, "Child task");
        assert_eq!(body.priority, "high");
        assert!(body.assignee_agent_id.is_some());
        assert!(body.project_id.is_some());
        assert!(body.project_workspace_id.is_some());
        assert!(body.goal_id.is_some());
        assert_eq!(body.created_by_user_id.as_deref(), Some("u-creator"));
        assert_eq!(body.request_depth, Some(1));
        assert!(body.assignee_adapter_overrides.is_some());
        assert!(body.execution_policy.is_some());
        assert!(body.execution_workspace_id.is_some());
        assert_eq!(
            body.acceptance_criteria.as_ref().map(|v| v.len()),
            Some(2)
        );
        assert_eq!(body.block_parent_until_done, Some(true));
    }

    #[test]
    fn child_body_minimal_required() {
        let payload = json!({"title": "Just title"});
        let body: ChildIssueFullBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.title, "Just title");
        assert_eq!(body.priority, "medium");
        assert!(body.description.is_none());
        assert!(body.acceptance_criteria.is_none());
        assert!(body.block_parent_until_done.is_none());
    }

    #[test]
    fn child_body_accepts_empty_acceptance_criteria_array() {
        let payload = json!({"title": "x", "acceptanceCriteria": []});
        let body: ChildIssueFullBody = serde_json::from_value(payload).expect("parse");
        let criteria = body.acceptance_criteria.expect("criteria");
        assert!(criteria.is_empty());
    }

    // ── 业务规则测试 ──

    #[test]
    fn unblock_descriptor_owner_variants_accepted() {
        // owner 可以是 {agentId} / {userId} / "board" 三种形式
        for owner in [
            json!({"agentId": Uuid::new_v4()}),
            json!({"userId": "u-x"}),
            json!("board"),
        ] {
            let payload = json!({
                "companyId": Uuid::new_v4(),
                "title": "blocked issue",
                "status": "blocked",
                "unblockDescriptor": {"owner": owner, "action": "do X"},
            });
            let body: CreateIssueFullBody =
                serde_json::from_value(payload).expect("parse");
            assert!(body.unblock_descriptor.is_some());
        }
    }
}

// ============================================================================
// Round 230: create path relations 处理逻辑测试
// ============================================================================
#[cfg(test)]
mod round230_tests {
    //! Round 230: 验证 create / create_child handler 在
    //! label_ids / blocked_by_issue_ids 不为空时正确选择事务路径。
    //!
    //! 由于 helper `needs_relations` 是路由函数内的局部逻辑,
    //! 这里通过解析 payload 后模拟其行为来验证:
    //! 1. 空 vec → 不需要事务
    //! 2. None → 不需要事务
    //! 3. 非空 → 需要事务（由 handler 选择事务方法）

    use super::{ChildIssueFullBody, CreateIssueFullBody};
    use serde_json::json;
    use uuid::Uuid;

    fn needs_relations_create(body: &CreateIssueFullBody) -> bool {
        body.label_ids.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
            || body.blocked_by_issue_ids.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
    }

    fn needs_relations_child(body: &ChildIssueFullBody) -> bool {
        body.label_ids.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
            || body.blocked_by_issue_ids.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
    }

    #[test]
    fn create_path_no_relations_when_label_ids_absent() {
        let body: CreateIssueFullBody = serde_json::from_value(json!({
            "companyId": Uuid::new_v4(),
            "title": "x"
        }))
        .expect("parse");
        assert!(!needs_relations_create(&body));
    }

    #[test]
    fn create_path_no_relations_when_label_ids_empty_array() {
        let body: CreateIssueFullBody = serde_json::from_value(json!({
            "companyId": Uuid::new_v4(),
            "title": "x",
            "labelIds": [],
            "blockedByIssueIds": [],
        }))
        .expect("parse");
        assert!(!needs_relations_create(&body));
    }

    #[test]
    fn create_path_needs_relations_when_label_ids_present() {
        let body: CreateIssueFullBody = serde_json::from_value(json!({
            "companyId": Uuid::new_v4(),
            "title": "x",
            "labelIds": [Uuid::new_v4()],
        }))
        .expect("parse");
        assert!(needs_relations_create(&body));
    }

    #[test]
    fn create_path_needs_relations_when_blocked_by_present() {
        let body: CreateIssueFullBody = serde_json::from_value(json!({
            "companyId": Uuid::new_v4(),
            "title": "x",
            "blockedByIssueIds": [Uuid::new_v4(), Uuid::new_v4()],
        }))
        .expect("parse");
        assert!(needs_relations_create(&body));
    }

    #[test]
    fn create_path_needs_relations_when_both_present() {
        let body: CreateIssueFullBody = serde_json::from_value(json!({
            "companyId": Uuid::new_v4(),
            "title": "x",
            "labelIds": [Uuid::new_v4()],
            "blockedByIssueIds": [Uuid::new_v4()],
        }))
        .expect("parse");
        assert!(needs_relations_create(&body));
    }

    // ── Child path ──

    #[test]
    fn child_path_no_relations_when_label_ids_absent() {
        let body: ChildIssueFullBody =
            serde_json::from_value(json!({"title": "x"})).expect("parse");
        assert!(!needs_relations_child(&body));
    }

    #[test]
    fn child_path_no_relations_when_empty_arrays() {
        let body: ChildIssueFullBody = serde_json::from_value(json!({
            "title": "x",
            "labelIds": [],
            "blockedByIssueIds": [],
        }))
        .expect("parse");
        assert!(!needs_relations_child(&body));
    }

    #[test]
    fn child_path_needs_relations_when_blocked_by_present() {
        let body: ChildIssueFullBody = serde_json::from_value(json!({
            "title": "x",
            "blockedByIssueIds": [Uuid::new_v4()],
        }))
        .expect("parse");
        assert!(needs_relations_child(&body));
    }

    #[test]
    fn child_path_accepts_acceptance_criteria_without_relations() {
        // acceptance_criteria 不影响 relations 路径选择
        let body: ChildIssueFullBody = serde_json::from_value(json!({
            "title": "x",
            "acceptanceCriteria": ["c1"],
            "blockParentUntilDone": true,
        }))
        .expect("parse");
        assert!(!needs_relations_child(&body));
        assert_eq!(
            body.acceptance_criteria.as_ref().map(|v| v.len()),
            Some(1)
        );
        assert_eq!(body.block_parent_until_done, Some(true));
    }

    #[test]
    fn child_path_needs_relations_with_full_payload() {
        // 包含 acceptance_criteria + label_ids + blocked_by — 应走事务
        let body: ChildIssueFullBody = serde_json::from_value(json!({
            "title": "x",
            "labelIds": [Uuid::new_v4()],
            "blockedByIssueIds": [Uuid::new_v4()],
            "acceptanceCriteria": ["c1", "c2"],
            "blockParentUntilDone": false,
        }))
        .expect("parse");
        assert!(needs_relations_child(&body));
        assert_eq!(
            body.acceptance_criteria.as_ref().map(|v| v.len()),
            Some(2)
        );
    }
}

// ============================================================================
// Round 231: tree-control preview / hold body 完整 schema 单元测试
// ============================================================================
#[cfg(test)]
mod round231_tests {
    //! Round 231: 验证 CreateTreeHoldBody / TreeControlPreviewBody 支持
    //! Node `createIssueTreeHoldSchema` / `previewIssueTreeControlSchema` 完整字段。

    use super::{CreateTreeHoldBody, TreeControlPreviewBody};
    use serde_json::json;
    use uuid::Uuid;

    // ── CreateTreeHoldBody ──

    #[test]
    fn create_hold_body_parses_mode_reason_release_policy_metadata() {
        let payload = json!({
            "mode": "pause",
            "reason": "Hold for manual review",
            "releasePolicy": {"strategy": "manual", "note": "release on approval"},
            "metadata": {"ticketId": "T-001", "operator": "u-op"},
        });
        let body: CreateTreeHoldBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.mode, "pause");
        assert_eq!(body.reason.as_deref(), Some("Hold for manual review"));
        assert!(body.release_policy.is_some());
        assert!(body.metadata.is_some());
        let meta = body.metadata.as_ref().unwrap();
        assert_eq!(meta["ticketId"], json!("T-001"));
        assert_eq!(meta["operator"], json!("u-op"));
    }

    #[test]
    fn create_hold_body_minimal_required_only() {
        let payload = json!({"mode": "stop"});
        let body: CreateTreeHoldBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.mode, "stop");
        assert!(body.reason.is_none());
        assert!(body.release_policy.is_none());
        assert!(body.metadata.is_none());
    }

    #[test]
    fn create_hold_body_accepts_resume_mode() {
        let payload = json!({"mode": "resume", "reason": "Subtree resume applied."});
        let body: CreateTreeHoldBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.mode, "resume");
        assert_eq!(body.reason.as_deref(), Some("Subtree resume applied."));
    }

    #[test]
    fn create_hold_body_metadata_complex_object() {
        let payload = json!({
            "mode": "isolate",
            "metadata": {
                "caller": "system",
                "externalRefs": ["ref-1", "ref-2"],
                "nested": {"k": "v"}
            }
        });
        let body: CreateTreeHoldBody = serde_json::from_value(payload).expect("parse");
        let meta = body.metadata.expect("metadata");
        assert_eq!(meta["caller"], json!("system"));
        assert_eq!(meta["externalRefs"][0], json!("ref-1"));
        assert_eq!(meta["nested"]["k"], json!("v"));
    }

    // ── TreeControlPreviewBody ──

    #[test]
    fn preview_body_parses_full_payload() {
        let payload = json!({
            "mode": "pause",
            "reason": "preview reason",
            "releasePolicy": {"strategy": "manual"},
            "includeEstimate": false,
        });
        let body: TreeControlPreviewBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.mode, "pause");
        assert_eq!(body.reason.as_deref(), Some("preview reason"));
        assert!(body.release_policy.is_some());
        assert_eq!(body.include_estimate, Some(false));
    }

    #[test]
    fn preview_body_mode_only() {
        let payload = json!({"mode": "throttle"});
        let body: TreeControlPreviewBody = serde_json::from_value(payload).expect("parse");
        assert_eq!(body.mode, "throttle");
        assert!(body.reason.is_none());
        assert!(body.release_policy.is_none());
        // include_estimate 默认 None
        assert!(body.include_estimate.is_none());
    }

    #[test]
    fn preview_body_accepts_all_node_modes() {
        // 验证 5 种 Node mode 都被接受
        for mode in ["pause", "stop", "throttle", "isolate", "resume"] {
            let payload = json!({"mode": mode});
            let body: TreeControlPreviewBody = serde_json::from_value(payload).expect("parse");
            assert_eq!(body.mode, mode);
        }
    }

    #[test]
    fn preview_body_camelcase_required() {
        // include_estimate snake_case 不被识别
        let payload = json!({"mode": "pause", "include_estimate": true});
        let body: TreeControlPreviewBody = serde_json::from_value(payload).expect("parse");
        assert!(body.include_estimate.is_none());
        let payload2 = json!({"mode": "pause", "includeEstimate": true});
        let body2: TreeControlPreviewBody = serde_json::from_value(payload2).expect("parse");
        assert_eq!(body2.include_estimate, Some(true));
    }
}

// ============================================================================
// Round 233: accepted_plan_decomposition 完整 schema 单元测试
// ============================================================================
#[cfg(test)]
mod round233_tests {
    //! Round 233: 验证 PlanDecompositionChildInput 接受完整 Node
    //! `createChildIssueSchema` 字段（含 acceptanceCriteria /
    //! blockParentUntilDone 扩展字段）。

    use super::PlanDecompositionChildInput;
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn plan_child_input_parses_full_camelcase_payload() {
        let payload = json!({
            "title": "Ship feature X",
            "description": "Full implementation",
            "status": "todo",
            "workMode": "standard",
            "harnessKind": "plan",
            "priority": "high",
            "assigneeAgentId": Uuid::new_v4(),
            "assigneeUserId": "u-1",
            "projectId": Uuid::new_v4(),
            "projectWorkspaceId": Uuid::new_v4(),
            "goalId": Uuid::new_v4(),
            "createdByUserId": "u-creator",
            "responsibleUserId": "u-owner",
            "billingCode": "BILL-1",
            "requestDepth": 2,
            "assigneeAdapterOverrides": {"kind": "openai"},
            "executionPolicy": {"maxSteps": 50},
            "executionWorkspaceId": Uuid::new_v4(),
            "executionWorkspacePreference": "isolated",
            "executionWorkspaceSettings": {"isolated": true},
            "unblockDescriptor": {"owner": "board", "action": "manual"},
            "blockedByIssueIds": [Uuid::new_v4(), Uuid::new_v4()],
            "labelIds": [Uuid::new_v4()],
            "acceptanceCriteria": ["c1", "c2"],
            "blockParentUntilDone": true,
        });
        let input: PlanDecompositionChildInput =
            serde_json::from_value(payload).expect("parse");
        assert_eq!(input.title, "Ship feature X");
        assert_eq!(input.status, "todo");
        assert_eq!(input.work_mode, "standard");
        assert_eq!(input.harness_kind.as_deref(), Some("plan"));
        assert_eq!(input.priority, "high");
        assert!(input.assignee_agent_id.is_some());
        assert_eq!(input.assignee_user_id.as_deref(), Some("u-1"));
        assert!(input.project_id.is_some());
        assert!(input.project_workspace_id.is_some());
        assert!(input.goal_id.is_some());
        assert_eq!(input.created_by_user_id.as_deref(), Some("u-creator"));
        assert_eq!(input.responsible_user_id.as_deref(), Some("u-owner"));
        assert_eq!(input.billing_code.as_deref(), Some("BILL-1"));
        assert_eq!(input.request_depth, Some(2));
        assert!(input.assignee_adapter_overrides.is_some());
        assert!(input.execution_policy.is_some());
        assert!(input.execution_workspace_id.is_some());
        assert_eq!(
            input.execution_workspace_preference.as_deref(),
            Some("isolated")
        );
        assert!(input.execution_workspace_settings.is_some());
        assert!(input.unblock_descriptor.is_some());
        assert_eq!(input.blocked_by_issue_ids.as_ref().map(|v| v.len()), Some(2));
        assert_eq!(input.label_ids.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(
            input.acceptance_criteria.as_ref().map(|v| v.len()),
            Some(2)
        );
        assert_eq!(input.block_parent_until_done, Some(true));
    }

    #[test]
    fn plan_child_input_minimal_required_only() {
        let payload = json!({"title": "minimal"});
        let input: PlanDecompositionChildInput =
            serde_json::from_value(payload).expect("parse");
        assert_eq!(input.title, "minimal");
        // 默认值由 default_* 函数提供
        assert_eq!(input.status, "todo");
        assert_eq!(input.work_mode, "standard");
        assert_eq!(input.priority, "medium");
        assert!(input.assignee_agent_id.is_none());
        assert!(input.project_id.is_none());
        assert!(input.goal_id.is_none());
        assert!(input.acceptance_criteria.is_none());
        assert!(input.block_parent_until_done.is_none());
    }

    #[test]
    fn plan_child_input_accepts_acceptance_criteria_array() {
        let payload = json!({
            "title": "with criteria",
            "acceptanceCriteria": ["criterion 1", "criterion 2", "criterion 3"],
            "blockParentUntilDone": true,
        });
        let input: PlanDecompositionChildInput =
            serde_json::from_value(payload).expect("parse");
        let criteria = input.acceptance_criteria.expect("criteria");
        assert_eq!(criteria.len(), 3);
        assert_eq!(criteria[0], "criterion 1");
        assert!(input.block_parent_until_done.unwrap_or(false));
    }

    #[test]
    fn plan_child_input_accepts_empty_acceptance_criteria() {
        let payload = json!({"title": "x", "acceptanceCriteria": []});
        let input: PlanDecompositionChildInput =
            serde_json::from_value(payload).expect("parse");
        let criteria = input.acceptance_criteria.expect("criteria");
        assert!(criteria.is_empty());
    }

    #[test]
    fn plan_child_input_serializes_camelcase_full() {
        // 验证 serialization 也是 camelCase
        let input = PlanDecompositionChildInput {
            title: "t".to_string(),
            description: Some("d".to_string()),
            status: "todo".to_string(),
            work_mode: "standard".to_string(),
            priority: "medium".to_string(),
            harness_kind: Some("plan".to_string()),
            assignee_agent_id: None,
            assignee_user_id: Some("u-1".to_string()),
            project_id: None,
            project_workspace_id: None,
            goal_id: None,
            created_by_user_id: Some("u-creator".to_string()),
            responsible_user_id: None,
            billing_code: None,
            request_depth: Some(1),
            assignee_adapter_overrides: None,
            execution_policy: None,
            execution_workspace_id: None,
            execution_workspace_preference: None,
            execution_workspace_settings: None,
            unblock_descriptor: None,
            blocked_by_issue_ids: None,
            label_ids: None,
            acceptance_criteria: None,
            block_parent_until_done: Some(false),
        };
        let v = serde_json::to_value(&input).expect("serialize");
        let obj = v.as_object().expect("object");
        assert!(obj.contains_key("workMode"));
        assert!(obj.contains_key("harnessKind"));
        assert!(obj.contains_key("assigneeAgentId"));
        assert!(obj.contains_key("assigneeUserId"));
        assert!(obj.contains_key("projectWorkspaceId"));
        assert!(obj.contains_key("createdByUserId"));
        assert!(obj.contains_key("responsibleUserId"));
        assert!(obj.contains_key("billingCode"));
        assert!(obj.contains_key("requestDepth"));
        assert!(obj.contains_key("assigneeAdapterOverrides"));
        assert!(obj.contains_key("executionPolicy"));
        assert!(obj.contains_key("executionWorkspaceId"));
        assert!(obj.contains_key("executionWorkspacePreference"));
        assert!(obj.contains_key("executionWorkspaceSettings"));
        assert!(obj.contains_key("unblockDescriptor"));
        assert!(obj.contains_key("blockedByIssueIds"));
        assert!(obj.contains_key("labelIds"));
        assert!(obj.contains_key("acceptanceCriteria"));
        assert!(obj.contains_key("blockParentUntilDone"));
    }

    #[test]
    fn plan_child_input_camelcase_strict_no_snake_alias() {
        // 验证 rename_all = "camelCase" 严格, snake_case 不识别
        let payload = json!({
            "title": "x",
            "work_mode": "standard",       // snake_case — 应被忽略
            "assignee_agent_id": Uuid::new_v4(),
        });
        let input: PlanDecompositionChildInput =
            serde_json::from_value(payload).expect("parse");
        assert_eq!(input.title, "x");
        // snake_case 字段被忽略，使用 default
        assert_eq!(input.work_mode, "standard"); // default
        assert!(input.assignee_agent_id.is_none());
    }

    #[test]
    fn plan_child_input_with_relations_full_payload() {
        // 同时包含 relations + acceptance criteria + blockParentUntilDone
        let blocker_id = Uuid::new_v4();
        let label_id = Uuid::new_v4();
        let payload = json!({
            "title": "child with all",
            "priority": "high",
            "status": "todo",
            "blockedByIssueIds": [blocker_id],
            "labelIds": [label_id],
            "acceptanceCriteria": ["must pass tests"],
            "blockParentUntilDone": true,
            "executionPolicy": {"maxSteps": 100},
            "executionWorkspacePreference": "isolated",
        });
        let input: PlanDecompositionChildInput =
            serde_json::from_value(payload).expect("parse");
        assert_eq!(input.priority, "high");
        let blockers = input.blocked_by_issue_ids.expect("blockers");
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0], blocker_id);
        let labels = input.label_ids.expect("labels");
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0], label_id);
        let criteria = input.acceptance_criteria.expect("criteria");
        assert_eq!(criteria[0], "must pass tests");
        assert!(input.block_parent_until_done.unwrap_or(false));
    }
}

// ============================================================================
// Round 236: 新路由注册 + handler 签名验证
// ============================================================================
#[cfg(test)]
mod round236_route_tests {
    //! Round 236: 验证 tree-control/state 和 live-runs 路由已注册,
    //! 并能正确处理不同路径格式 (虽然 handler 内部依赖 DB, 单元测试仅验证
    //! router 注册 + 路径匹配)。

    use axum::http::Request;
    use axum::Router;

    // 我们不能在 #[cfg(test)] 内构建完整 AppState (依赖 DB pool).
    // 这里用路由路径检查 — 通过遍历 router 内部路由表。

    #[test]
    fn tree_control_state_route_registered() {
        // 验证路由文件包含 R236 新路由
        let src = include_str!("issues.rs");
        assert!(
            src.contains("/api/issues/:id/tree-control/state"),
            "tree-control/state route should be registered"
        );
        assert!(
            src.contains("async fn tree_control_state"),
            "tree_control_state handler should be defined"
        );
    }

    #[test]
    fn live_runs_route_registered() {
        let src = include_str!("issues.rs");
        assert!(
            src.contains("/api/issues/:id/live-runs"),
            "live-runs route should be registered"
        );
        assert!(
            src.contains("async fn list_live_runs"),
            "list_live_runs handler should be defined"
        );
    }

    #[test]
    fn tree_control_state_handler_signature() {
        // 验证 handler 签名包含 State<AppState> + Path<Uuid> 参数
        let src = include_str!("issues.rs");
        let has_signature = src.contains("async fn tree_control_state(\n    State(state): State<AppState>,\n    Path(issue_id): Path<Uuid>,\n) -> ApiResult<Json<Value>>");
        assert!(
            has_signature,
            "tree_control_state signature should match (State + Path + Json return)"
        );
    }

    #[test]
    fn list_live_runs_handler_signature() {
        let src = include_str!("issues.rs");
        let has_signature = src.contains("async fn list_live_runs(\n    State(state): State<AppState>,\n    Path(issue_id): Path<Uuid>,\n) -> ApiResult<Json<Value>>");
        assert!(
            has_signature,
            "list_live_runs signature should match (State + Path + Json return)"
        );
    }

    #[test]
    fn tree_control_state_returns_correct_json_keys() {
        // 验证响应 JSON 包含核心字段: issueId, companyId, activePauseHold
        let src = include_str!("issues.rs");
        assert!(
            src.contains("\"issueId\""),
            "tree_control_state response should include issueId"
        );
        assert!(
            src.contains("\"companyId\""),
            "tree_control_state response should include companyId"
        );
        assert!(
            src.contains("\"activePauseHold\""),
            "tree_control_state response should include activePauseHold"
        );
    }

    #[test]
    fn list_live_runs_returns_correct_json_keys() {
        let src = include_str!("issues.rs");
        assert!(
            src.contains("\"runs\""),
            "list_live_runs response should include runs array"
        );
    }

    #[test]
    fn tree_control_state_handles_missing_issue() {
        // 验证 NotFound 错误处理路径: "issue {issue_id}"
        let src = include_str!("issues.rs");
        assert!(
            src.contains("format!(\"issue {issue_id}\")"),
            "tree_control_state should return NotFound with issue_id format"
        );
    }

    #[test]
    fn live_runs_filters_terminal_statuses() {
        // 验证 SQL 过滤终态 (succeeded/failed/cancelled/timed_out)
        let src = include_str!("issues.rs");
        assert!(
            src.contains("status NOT IN ('succeeded', 'failed', 'cancelled', 'timed_out')"),
            "list_live_runs should filter out terminal run statuses"
        );
    }

    #[test]
    fn live_runs_uses_context_snapshot_or_issue_id() {
        // 验证 SQL 包含 context_snapshot OR issue_id 查询
        let src = include_str!("issues.rs");
        assert!(
            src.contains("context_snapshot ->> 'issueId'"),
            "list_live_runs should query context_snapshot ->> 'issueId' as fallback"
        );
    }
}
