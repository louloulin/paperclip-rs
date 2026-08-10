//! `/api/approvals*` 路由：CRUD + 决策。

use axum::Extension as AxumExtension;
#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use pc_auth::AuthContext;
use pc_authz::{enforce_permission, PermissionKey};
use pc_telemetry::global;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx;
use std::collections::BTreeMap;
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::approval::ApprovalRepo;
use pc_repos::issue_approvals::IssueApprovalRepo;

use pc_approvals::{
    ApprovalService, DbHireAgentOps, HireAgentApprovalHook, HireMode, NoopApprovalHook,
};

use std::sync::Arc;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/approvals", get(list).post(create))
        .route("/api/approvals/:id", get(get_one).delete(remove))
        .route("/api/approvals/:id/decide", post(decide))
        // ── Round 22: approval issues, comments, approve/reject/resubmit ──
        .route("/api/approvals/:id/issues", get(list_approval_issues))
        .route("/api/approvals/:id/approve", post(approve_approval))
        .route("/api/approvals/:id/reject", post(reject_approval))
        .route("/api/approvals/:id/resubmit", post(resubmit_approval))
        // ── Round 195: request revision ──
        .route(
            "/api/approvals/:id/request-revision",
            post(request_approval_revision),
        )
        .route(
            "/api/approvals/:id/comments",
            get(list_approval_comments).post(add_approval_comment),
        )
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ListQuery {
    #[serde(default)]
    company_id: Option<Uuid>,
}

async fn list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = match q.company_id {
        Some(cid) => {
            ApprovalRepo::new(&state.db)
                .list_by_company_simple(cid)
                .await?
        }
        None => ApprovalRepo::new(&state.db).list_all(200).await?,
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = ApprovalRepo::new(&state.db)
        .get_id(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("approval {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    company_id: Uuid,
    approval_type: String,
    #[serde(default)]
    payload: serde_json::Value,
}

async fn create(
    State(state): State<AppState>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    // pc-authz：创建 approval 需要 UsersInvite 权限（Operator 角色及以上）
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        body.company_id,
        PermissionKey::UsersInvite,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    if body.approval_type.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "approval_type must not be empty".into(),
        ));
    }
    let payload = if body.payload.is_null() {
        serde_json::json!({})
    } else {
        body.payload
    };
    let row = ApprovalRepo::new(&state.db)
        .create_three_args(body.company_id, &body.approval_type, payload)
        .await?;
    state.realtime.publish(
        LiveEvent::new("approval.created", "approval", row.id).with_company(row.company_id),
    );
    global::track(
        "approval.created",
        BTreeMap::from([
            (
                "company_id".into(),
                serde_json::json!(row.company_id.to_string()),
            ),
            ("approval_id".into(), serde_json::json!(row.id.to_string())),
            ("approval_type".into(), serde_json::json!(row.approval_type)),
        ]),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "company_id": row.company_id, "approval_type": row.approval_type, "status": row.status
        })),
    ))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DecideBody {
    status: String,
    #[serde(default)]
    note: Option<String>,
    decided_by: String,
}

async fn decide(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<DecideBody>,
) -> ApiResult<Json<Value>> {
    if !["approved", "rejected", "cancelled"].contains(&body.status.as_str()) {
        return Err(ApiError::BadRequest(
            "status must be approved|rejected|cancelled".into(),
        ));
    }
    // 先查 company_id（service 需要）
    let company_id = load_approval_company(&state.db, id).await?;
    let svc = build_hire_approval_service(&state.db);
    let row = match body.status.as_str() {
        "approved" => svc
            .approve(company_id, id, &body.decided_by, body.note.as_deref())
            .await
            .map_err(|e| map_approval_service_error(e, id))?,
        "rejected" => svc
            .reject(company_id, id, &body.decided_by, body.note.as_deref())
            .await
            .map_err(|e| map_approval_service_error(e, id))?,
        "cancelled" => svc
            .cancel(company_id, id, &body.decided_by, body.note.as_deref())
            .await
            .map_err(|e| map_approval_service_error(e, id))?
            .ok_or_else(|| ApiError::NotFound(format!("approval {id}")))?,
        _ => unreachable!(),
    };
    state.realtime.publish(
        LiveEvent::new(format!("approval.{}", body.status), "approval", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = ApprovalRepo::new(&state.db).delete_one(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("approval {id}")))
    }
}

// ============== Round 22: approval issues / approve/reject/resubmit / comments ==============

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResubmitApprovalBody {
    note: Option<String>,
    payload: Option<Value>,
}

async fn list_approval_issues(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Issues linked to this approval via issue_approvals table
    let rows = IssueApprovalRepo::new(&state.db)
        .list_issues_for_approval_raw(approval_id)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(issue_id, company_id, linked_by_user_id, created_at)| {
            json!({
                "issueId": issue_id,
                "companyId": company_id,
                "linkedByUserId": linked_by_user_id,
                "linkedAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "approvalId": approval_id,
        "issues": items,
        "items": items,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApproveRejectBody {
    note: Option<String>,
    decided_by: Option<String>,
}

async fn approve_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<ApproveRejectBody>,
) -> ApiResult<Json<Value>> {
    let company_id = load_approval_company(&state.db, approval_id).await?;
    // pc-authz：批准 approval 需要 UsersInvite 权限（Operator 角色及以上）。
    if let Err(err) =
        enforce_permission(&state.db, &actor, company_id, PermissionKey::UsersInvite).await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    let svc = build_hire_approval_service(&state.db);
    let decided_by = body.decided_by.as_deref().unwrap_or("user");
    let row = svc
        .approve(company_id, approval_id, decided_by, body.note.as_deref())
        .await
        .map_err(|e| map_approval_service_error(e, approval_id))?;
    state.realtime.publish(
        LiveEvent::new("approval.approved", "approval", row.id).with_company(row.company_id),
    );
    global::track(
        "approval.approved",
        BTreeMap::from([
            (
                "company_id".into(),
                serde_json::json!(row.company_id.to_string()),
            ),
            ("decision".into(), serde_json::json!("approved")),
        ]),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn reject_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<ApproveRejectBody>,
) -> ApiResult<Json<Value>> {
    let company_id = load_approval_company(&state.db, approval_id).await?;
    if let Err(err) =
        enforce_permission(&state.db, &actor, company_id, PermissionKey::UsersInvite).await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    let svc = build_hire_approval_service(&state.db);
    let decided_by = body.decided_by.as_deref().unwrap_or("user");
    let row = svc
        .reject(company_id, approval_id, decided_by, body.note.as_deref())
        .await
        .map_err(|e| map_approval_service_error(e, approval_id))?;
    state.realtime.publish(
        LiveEvent::new("approval.rejected", "approval", row.id).with_company(row.company_id),
    );
    global::track(
        "approval.rejected",
        BTreeMap::from([
            (
                "company_id".into(),
                serde_json::json!(row.company_id.to_string()),
            ),
            ("approval_id".into(), serde_json::json!(row.id.to_string())),
        ]),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn resubmit_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<ResubmitApprovalBody>,
) -> ApiResult<Json<Value>> {
    // pc-authz：resubmit 需要 UsersInvite 权限
    let preview: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM approvals WHERE id = $1")
        .bind(approval_id)
        .fetch_optional(state.db.pool())
        .await?;
    let preview_company_id = preview
        .ok_or_else(|| ApiError::NotFound(format!("approval {approval_id}")))?
        .0;
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        preview_company_id,
        PermissionKey::UsersInvite,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    // Set status back to 'pending' and update payload if provided
    ApprovalRepo::new(&state.db)
        .resubmit(approval_id, body.payload.as_ref(), body.note.as_deref())
        .await?;
    let (id, company_id) = ApprovalRepo::new(&state.db)
        .get_id_company(approval_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("approval {approval_id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("approval.resubmitted", "approval", id).with_company(company_id));
    global::track(
        "approval.resubmitted",
        BTreeMap::from([(
            "company_id".into(),
            serde_json::json!(company_id.to_string()),
        )]),
    );
    Ok(Json(json!({
        "id": id,
        "companyId": company_id,
        "status": "pending",
        "resubmitted": true,
    })))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RequestRevisionBody {
    #[serde(default)]
    decision_note: Option<String>,
    #[serde(default)]
    decided_by: Option<String>,
}

async fn request_approval_revision(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<RequestRevisionBody>,
) -> ApiResult<Json<Value>> {
    let company_id = load_approval_company(&state.db, approval_id).await?;
    if let Err(err) =
        enforce_permission(&state.db, &actor, company_id, PermissionKey::JoinsApprove).await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    let decided_by = body.decided_by.as_deref().unwrap_or("board");
    // request_revision 内部走 service：检查状态机 + 触发 hooks（无 hire_agent hook 影响 revision_requested）
    let svc = build_hire_approval_service(&state.db);
    let row = svc
        .request_revision(
            company_id,
            approval_id,
            decided_by,
            body.decision_note.as_deref(),
        )
        .await
        .map_err(|e| map_approval_service_error(e, approval_id))?;
    state.realtime.publish(
        LiveEvent::new("approval.revision_requested", "approval", row.id)
            .with_company(row.company_id),
    );
    global::track(
        "approval.revision_requested",
        BTreeMap::from([
            (
                "company_id".into(),
                serde_json::json!(row.company_id.to_string()),
            ),
            ("approval_id".into(), serde_json::json!(row.id.to_string())),
        ]),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn list_approval_comments(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let svc = build_hire_approval_service(&state.db);
    let rows = svc
        .list_comments(approval_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|c| {
            json!({
                "id": c.id,
                "companyId": c.company_id,
                "authorAgentId": c.author_agent_id,
                "authorUserId": c.author_user_id,
                "body": c.body,
                "createdAt": c.created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "approvalId": approval_id,
        "comments": items,
        "items": items,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddApprovalCommentBody {
    body: String,
    author_user_id: Option<String>,
    author_agent_id: Option<Uuid>,
}

async fn add_approval_comment(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<AddApprovalCommentBody>,
) -> ApiResult<impl IntoResponse> {
    if body.body.trim().is_empty() {
        return Err(ApiError::BadRequest("body is required".into()));
    }
    let company_id = load_approval_company(&state.db, approval_id).await?;
    // pc-authz：comment 需要公司成员资格（任何 active member 都能 comment）
    if !actor.actor.has_company_access(company_id) {
        return Err(ApiError::Forbidden(
            "actor lacks access to this company".into(),
        ));
    }
    // 通过 service 走 add_comment：自动隔离业务逻辑
    let svc = build_hire_approval_service(&state.db);
    let c = pc_repos::approval::NewApprovalComment {
        approval_id,
        company_id,
        author_user_id: body.author_user_id.clone(),
        author_agent_id: body.author_agent_id,
        body: body.body.clone(),
    };
    let comment = svc
        .add_comment(&c)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let id = comment.id;
    state.realtime.publish(
        LiveEvent::new("approval.comment_added", "approval_comment", id)
            .with_company(company_id)
            .with_data(json!({"approvalId": approval_id})),
    );
    global::track(
        "approval.comment_added",
        BTreeMap::from([
            (
                "company_id".into(),
                serde_json::json!(company_id.to_string()),
            ),
            (
                "approval_id".into(),
                serde_json::json!(approval_id.to_string()),
            ),
            ("comment_id".into(), serde_json::json!(id.to_string())),
        ]),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "companyId": company_id,
            "approvalId": approval_id,
            "body": body.body,
            "authorUserId": body.author_user_id,
            "authorAgentId": body.author_agent_id,
            "createdAt": comment.created_at,
        })),
    ))
}
// ============== R584: ApprovalService + HireAgentApprovalHook 辅助函数 ==============

/// 从 DB 查 approval 的 company_id（用于 authz 检查）。
async fn load_approval_company(db: &pc_db::Db, approval_id: Uuid) -> Result<Uuid, ApiError> {
    let preview_company: Option<(Uuid,)> =
        sqlx::query_as("SELECT company_id FROM approvals WHERE id = $1")
            .bind(approval_id)
            .fetch_optional(db.pool())
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    preview_company
        .ok_or_else(|| ApiError::NotFound(format!("approval {approval_id}")))
        .map(|(c,)| c)
}

/// 构造"hire agent 决策走真实 DB"路径的 `ApprovalService`：
/// - 注册 `HireAgentApprovalHook<DbHireAgentOps>`：approve → 激活/创建 agent + budget policy；reject → terminate
/// - 注册 `NoopApprovalHook` 占位（未来可加通知、audit log 等副作用）
fn build_hire_approval_service(db: &pc_db::Db) -> ApprovalService<'_> {
    let ops = Arc::new(DbHireAgentOps::new(db.clone()));
    let hire_hook: Arc<dyn pc_approvals::ApprovalHook> = Arc::new(HireAgentApprovalHook::new(ops));
    let noop: Arc<dyn pc_approvals::ApprovalHook> = Arc::new(NoopApprovalHook);
    ApprovalService::with_hooks(db, vec![hire_hook, noop])
}

/// `ApprovalServiceError` → HTTP `ApiError` 映射。
fn map_approval_service_error(
    e: pc_approvals::ApprovalServiceError,
    approval_id: Uuid,
) -> ApiError {
    use pc_approvals::ApprovalServiceError;
    match e {
        ApprovalServiceError::NotFound(_) => ApiError::NotFound(format!("approval {approval_id}")),
        ApprovalServiceError::InvalidTransition(s) => {
            ApiError::BadRequest(format!("invalid transition: {s}"))
        }
        ApprovalServiceError::Repo(e) => ApiError::Internal(e.to_string()),
        ApprovalServiceError::Hook(s) => ApiError::Internal(format!("hook error: {s}")),
    }
}
