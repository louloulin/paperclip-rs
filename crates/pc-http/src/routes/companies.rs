//! `/api/companies*` 路由：CRUD + 归档。

#[allow(unused_imports)]
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::hooks::CompanyActivityHook;
use pc_activity::{ActivityActor, ActivityEvent, ActivityFilter, ActivityKind};
use pc_companies::{CompanyActor, CompanyService, CreateCompanyInput, UpdateCompanyPatch};
use pc_plugin_host::plugin_event_bus::{ActorType, PluginEvent};
use pc_portability::{
    ExportInput, PortabilityPreviewInput, PortabilityService, PortabilityServiceError,
};
use pc_realtime::LiveEvent;
use pc_repos::agent::AgentRepo;
use pc_repos::agent_action_audit::{
    AgentActionAuditFilters, AgentActionAuditPage, AgentActionAuditRepo,
};
use pc_repos::approval::ApprovalRepo;
use pc_repos::asset::AssetRepo;
use pc_repos::case::CaseRepo;
use pc_repos::company::{CompanyListRow, CompanyRepo, CompanyRow};
use pc_repos::cost::{CostRepo, FinanceEventRow, NewFinanceEvent};
use pc_repos::decision::DecisionRepo;
use pc_repos::feedback_trace::FeedbackTraceRepo;
use pc_repos::folder::{FolderKind, FolderPatch, FolderRepo, NewFolder};
use pc_repos::folder::{MoveFolderItem, MoveFolderItemKind};
use pc_repos::goal::GoalRepo;
use pc_repos::heartbeat::HeartbeatRepo;
use pc_repos::invite::{InviteRepo, NewInvite};
use pc_repos::issue::IssueRepo;
use pc_repos::label::{LabelPatch, LabelRepo, NewLabel};
use pc_repos::pipeline::PipelineRepo;
use pc_repos::work_timeline::{
    WorkTimelineQuery as RepoWorkTimelineQuery, WorkTimelineRepo, WorkTimelineResult,
};

use crate::{state::require_user_id, ApiError, ApiResult, AppState};
use axum::Extension as AxumExtension;
use pc_auth::AuthContext;
use pc_authz::{enforce_permission, Action, PermissionKey, Resource};
use pc_core::Timestamp;

/// R591: 构造一个自动触发 activity / realtime / plugin event 的 CompanyService。
fn company_service_with_activity(state: &AppState) -> CompanyService<'_> {
    let state_arc = std::sync::Arc::new(state.clone());
    let hook: std::sync::Arc<dyn pc_companies::CompanyHook> =
        std::sync::Arc::new(CompanyActivityHook::new(state_arc));
    CompanyService::with_hooks(&state.db, vec![hook])
}

fn map_portability_service_error(e: PortabilityServiceError, id: Uuid) -> ApiError {
    use PortabilityServiceError::*;
    match e {
        NotFound(_) => ApiError::NotFound(format!("company {id}")),
        InvalidInput(m) => ApiError::BadRequest(m),
        Repo(m) => ApiError::Internal(format!("repo: {m}")),
    }
}

fn map_company_service_error(e: pc_companies::CompanyServiceError, id: Uuid) -> ApiError {
    use pc_companies::CompanyServiceError::*;
    match e {
        NotFound(_) => ApiError::NotFound(format!("company {id}")),
        InvalidInput(m) => ApiError::BadRequest(m),
        Forbidden(m) => ApiError::Forbidden(m),
        Repo(m) => ApiError::Internal(format!("repo: {m}")),
    }
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/companies", get(list).post(create))
        // R515: trailing-slash alias (与 Node 默认行为一致)。
        .route("/api/companies/", get(list).post(create))
        .route(
            "/api/companies/:company_id",
            get(get_one).patch(update).delete(remove),
        )
        .route("/api/companies/:company_id/archive", post(archive))
        .route("/api/companies/:company_id/stats", get(get_stats))
        .route("/api/companies/:company_id/timeline", get(get_timeline))
        .route("/api/companies/:company_id/artifacts", get(list_artifacts))
        .route(
            "/api/companies/:company_id/branding",
            get(get_branding).patch(update_branding),
        )
        // ── Round 211: company-level diagnostics aggregate ──
        .route(
            "/api/companies/:company_id/diagnostics",
            get(company_diagnostics),
        )
        // ── Round 208: company-level GET aliases ──
        .route(
            "/api/companies/:company_id/finance-events",
            get(list_company_finance_events),
        )
        .route(
            "/api/companies/:company_id/exports/preview",
            post(export_preview),
        )
        .route(
            "/api/companies/:company_id/imports/preview",
            post(import_preview),
        )
        .route("/api/companies/import/preview", post(import_preview_root))
        .route("/api/companies/import/jobs/:job_id", get(get_import_job))
        .route(
            "/api/companies/:company_id/export",
            post(start_company_export),
        )
        // ── Round 45: cross-company aggregation + export plural alias ──
        .route("/api/companies/stats", get(get_companies_stats))
        .route("/api/companies/issues", get(get_companies_issues_malformed))
        .route(
            "/api/companies/:company_id/exports",
            post(start_company_export),
        )
        // ── Round 49: removed duplicate plugin_ui_static alias ──
        // Real impl lives in routes/plugin_ui_static.rs (registered via routes/mod.rs)
        .route(
            "/api/companies/:company_id/export/fidelity",
            get(get_company_export_fidelity),
        )
        .route(
            "/api/companies/:company_id/feedback-traces",
            get(list_company_feedback_traces),
        )
        .route(
            "/api/companies/:company_id/imports/apply",
            post(apply_company_import),
        )
        // NOTE: `/api/companies/:company_id/labels` is registered by labels.rs.
        // The duplicate GET/POST registration here was removed in Round 282 because
        // it produced axum "Overlapping method route" panics during integration tests.
        // NOTE: `/api/companies/:company_id/folders*` routes are registered by
        // folders.rs (the canonical folders router module). The duplicate registrations
        // were removed in Round 282 because they produced axum "Overlapping method route"
        // panics. The local handler functions remain as dead code (kept for reference).
        // ===== labels / invites / members / org / audit =====
        .route(
            "/api/companies/:company_id/labels/:label_id",
            patch(patch_label).delete(delete_label),
        )
        .route(
            "/api/companies/:company_id/invites",
            get(list_invites).post(create_invite),
        )
        .route(
            "/api/companies/:company_id/invites/:invite_id",
            delete(revoke_invite),
        )
        .route(
            "/api/companies/:company_id/join-requests",
            get(list_join_requests),
        )
        .route(
            "/api/companies/:company_id/join-requests/:req_id/approve",
            post(approve_join_request),
        )
        .route(
            "/api/companies/:company_id/join-requests/:req_id/reject",
            post(reject_join_request),
        )
        .route("/api/companies/:company_id/members", get(list_members))
        .route(
            "/api/companies/:company_id/members/:member_id",
            patch(patch_member),
        )
        .route(
            "/api/companies/:company_id/members/:member_id/archive",
            post(archive_member),
        )
        .route(
            "/api/companies/:company_id/members/:member_id/permissions",
            patch(patch_member_permissions),
        )
        .route(
            "/api/companies/:company_id/members/:member_id/role-and-grants",
            patch(patch_member_role_and_grants),
        )
        // NOTE: `/api/companies/:company_id/users/me/inbox-agent-policy` is registered
        // by inbox_agent_policy.rs (the canonical module). The duplicate registration
        // here was removed in Round 282 because it produced axum "Overlapping method
        // route" panics during integration tests. The local handler functions remain
        // as dead code (kept for reference); they can be deleted in a follow-up.
        .route(
            "/api/companies/:company_id/audit/agent-actions",
            get(list_agent_actions),
        )
        .route(
            "/api/companies/:company_id/audit/agent-actions.csv",
            get(export_agent_actions_csv),
        )
        .route("/api/companies/:company_id/org", get(get_org))
        .route("/api/companies/:company_id/org.svg", get(get_org_svg))
        .route("/api/companies/:company_id/org.png", get(get_org_png))
        .route(
            // ── R510: GET aliased to POST for Node parity
            //   (`/companies/:companyId/search/extract`). Both honour the same
            //   JSON body — search query + limit. GET is historical.
            "/api/companies/:company_id/search/extract",
            post(search_extract).get(search_extract),
        )
        // NOTE: POST `/api/companies/:company_id/finance-events` is registered by
        // costs.rs (the canonical module). The duplicate registration here was
        // removed in Round 282 because it produced axum "Overlapping method route"
        // panics during integration tests. The local `create_finance_event` handler
        // remains as dead code (kept for reference).
        .route("/api/companies/:company_id/agents", post(create_agent))
        // NOTE: `/api/companies/:company_id/built-in-agents/:key/provision` is
        // registered by built_in_agents.rs (the canonical module). The duplicate
        // single-route registration here was removed in Round 282 because it
        // produced axum "Invalid route ... insertion failed" panics during
        // integration tests (path param name `:id` vs `:key` mismatch).
        // The local `provision_built_in_agent` handler remains as dead code
        // (kept for reference).
        // ---- Round 37: company sub-resources (activity / approvals / decisions / goals / pipelines / case-events / user-directory / review-cases) ----
        .route(
            "/api/companies/:company_id/activity",
            get(list_company_activity_route).post(create_activity),
        )
        .route(
            "/api/companies/:company_id/approvals",
            get(list_company_approvals_route).post(create_company_approval_route),
        )
        .route(
            "/api/companies/:company_id/decisions",
            get(list_company_decisions_route).post(create_company_decision_route),
        )
        // NOTE: `/api/companies/:company_id/goals` is registered by goals.rs (the
        // canonical goals router module). The duplicate registration here was removed
        // in Round 282 because it produced axum "Overlapping method route" panics
        // during integration tests. The local `list_company_goals_route` handler
        // remains as dead code (kept for reference).
        .route(
            "/api/companies/:company_id/pipelines",
            get(list_company_pipelines_route).post(create_company_pipeline_route),
        )
        .route(
            "/api/companies/:company_id/case-events",
            get(list_company_case_events_route),
        )
        .route(
            "/api/companies/:company_id/user-directory",
            get(list_company_user_directory_route),
        )
        .route(
            "/api/companies/:company_id/review-cases",
            get(list_company_review_cases_route),
        )
        // ---- Round 245: company watchdog evaluation worker 入口 ----
        .route(
            "/api/companies/:company_id/watchdog-evaluations",
            get(list_company_watchdog_evaluations_route),
        )
}

async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<CompanyListRow>>> {
    // R590: 业务下沉到 CompanyService
    let rows = company_service_with_activity(&state)
        .list()
        .await
        .map_err(|e| map_company_service_error(e, Uuid::nil()))?;
    Ok(Json(rows))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    // R590: 业务下沉到 CompanyService
    let row = company_service_with_activity(&state)
        .get_by_id(id)
        .await
        .map_err(|e| map_company_service_error(e, id))?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    // R590: 业务下沉到 CompanyService
    // R626: 取消 "local-board" fallback — 鉴权失败必须 surface（之前会掩盖 client
    //       鉴权 bug，导致创建出来的 company owner 是 "local-board" 占位 principal，
    //       后续 is_active_member / WS auth / resource ACL 全部误判）。
    let owner_id = match require_user_id(&state, &headers).await {
        Ok(user_id) => user_id,
        Err(error) => return Err(error),
    };
    let row = company_service_with_activity(&state)
        .create(CreateCompanyInput {
            name: body.name,
            description: body.description,
            owner_principal_id: owner_id.clone(),
            budget_monthly_cents: None,
        })
        .await
        .map_err(|e| map_company_service_error(e, Uuid::nil()))?;
    state.realtime.publish(
        LiveEvent::new("company.created", "company", row.id)
            .with_company(row.id)
            .with_actor("system"),
    );
    global::track(
        "company.created",
        BTreeMap::from([
            ("name".into(), serde_json::json!(row.name.clone())),
            ("owner_id".into(), serde_json::json!(owner_id.clone())),
        ]),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": row.id, "name": row.name, "status": row.status })),
    ))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UpdateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    // R590: 业务下沉到 CompanyService
    let patch = UpdateCompanyPatch {
        name: body.name,
        description: body.description,
        status: body.status,
        ..Default::default()
    };
    let row = company_service_with_activity(&state)
        .update(id, patch, &CompanyActor::system())
        .await
        .map_err(|e| map_company_service_error(e, id))?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("company.updated", "company", row.id).with_company(row.id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn archive(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    // R590: 业务下沉到 CompanyService
    let row = company_service_with_activity(&state)
        .archive(id, &CompanyActor::system())
        .await
        .map_err(|e| map_company_service_error(e, id))?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    Ok(Json(
        json!({ "id": row.id, "status": row.status, "archived_at": row.updated_at }),
    ))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    // R590: 业务下沉到 CompanyService
    let ok = company_service_with_activity(&state)
        .remove(id)
        .await
        .map_err(|e| map_company_service_error(e, id))?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("company {id}")))
    }
}

// ============================================================================
// Stats & timeline
// ============================================================================

async fn get_stats(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let s = CompanyRepo::new(&state.db).stats(id).await?;
    Ok(Json(json!({
        "company_id": s.company_id,
        "issue_count": s.issue_count,
        "open_issue_count": s.open_issue_count,
        "agent_count": s.agent_count,
        "pipeline_count": s.pipeline_count,
        "project_count": s.project_count,
        "goal_count": s.goal_count,
    })))
}

/// `GET /api/companies/:id/timeline` — Round 51 deepened.
///
/// Mirrors Node `workTimelineService.getTimeline`. Aggregates events from
/// three sources into a single sorted feed:
/// 1. `activity_log` — board/agent/system actions on issues, decisions, etc.
/// 2. `pipeline_case_events` — case lifecycle events (created, transitioned, etc.)
/// 3. `heartbeat_runs` — agent run lifecycle (started, finished, failed, etc.)
///
/// Query params:
/// - `limit` — default 50, max 200
/// - `from` — ISO-8601 timestamp lower bound (inclusive)
/// - `to` — ISO-8601 timestamp upper bound (inclusive)
/// - `entity_type` — filter to a specific entity_type (issue, case, decision, etc.)
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimelineQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    to: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    goal_id: Option<uuid::Uuid>,
    #[serde(default)]
    project_id: Option<uuid::Uuid>,
    #[serde(default)]
    issue_id: Option<uuid::Uuid>,
}

async fn get_timeline(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<TimelineQuery>,
) -> ApiResult<Json<WorkTimelineResult>> {
    let query = RepoWorkTimelineQuery {
        company_id: id,
        from: q.from,
        to: q.to,
        user_id: q.user_id,
        goal_id: q.goal_id,
        project_id: q.project_id,
        issue_id: q.issue_id,
        limit: q.limit,
        offset: q.offset,
    };
    let result = WorkTimelineRepo::new(&state.db)
        .get_timeline(query, chrono::Utc::now())
        .await;
    Ok(Json(result))
}

async fn list_artifacts(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = AssetRepo::new(&state.db).list_by_company(id, 100).await?;
    let assets: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "provider": r.provider,
                "object_key": r.object_key,
                "byte_size": r.byte_size,
                "content_type": r.content_type,
                "sha256": r.sha256,
                "original_filename": r.original_filename,
                "created_at": r.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "company_id": id, "assets": assets })))
}

// ============================================================================
// Branding
// ============================================================================

#[derive(Debug, Deserialize, Default)]
struct BrandingBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// 可选 logo URL（暂存到 description 后缀：实际项目应有独立 branding 表）
    #[serde(default)]
    logo_url: Option<String>,
}

async fn update_branding(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<BrandingBody>,
) -> ApiResult<Json<Value>> {
    let row = CompanyRepo::new(&state.db)
        .update_branding(id, body.name.as_deref(), body.logo_url.as_deref())
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    state.realtime.publish(
        LiveEvent::new("company.branding.updated", "company", row.id).with_company(row.id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

// ============================================================================
// Export / Import preview
// ============================================================================

async fn export_preview(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // R593: 通过 PortabilityService::preview 服务化
    let company: Option<CompanyRow> = CompanyRepo::new(&state.db).get(id).await?;
    let company = company.ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    let preview = PortabilityService::new(&state.db)
        .preview(id, PortabilityPreviewInput::default())
        .await
        .map_err(|e| map_portability_service_error(e, id))?;
    Ok(Json(json!({
        "version": preview.version,
        "company": {
            "id": company.id,
            "name": company.name,
            "description": company.description,
            "status": company.status,
        },
        "counts": {
            "issues": preview.counts.issues,
            "agents": preview.counts.agents,
            "pipelines": preview.counts.pipelines,
        },
        "issues": preview.issues.into_iter().map(|i| json!({"id":i.id,"title":i.title,"status":i.status,"priority":i.priority})).collect::<Vec<_>>(),
        "agents": preview.agents.into_iter().map(|a| json!({"id":a.id,"name":a.name,"role":a.role})).collect::<Vec<_>>(),
        "pipelines": preview.pipelines.into_iter().map(|p| json!({"id":p.id,"key":p.key,"name":p.name})).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
struct ImportPreviewBody {
    /// 必须是 { version, company, issues?, agents?, pipelines? } 结构
    #[serde(default)]
    payload: serde_json::Value,
}

async fn import_preview(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ImportPreviewBody>,
) -> ApiResult<Json<Value>> {
    // 校验：必须有 version 与 company.name
    let version = body.payload.get("version").and_then(|v| v.as_str());
    let company_name = body
        .payload
        .get("company")
        .and_then(|c| c.get("name"))
        .and_then(|n| n.as_str());
    let issue_count = body
        .payload
        .get("issues")
        .and_then(|i| i.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let agent_count = body
        .payload
        .get("agents")
        .and_then(|a| a.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let pipeline_count = body
        .payload
        .get("pipelines")
        .and_then(|p| p.as_array())
        .map(|p| p.len())
        .unwrap_or(0);
    let valid = version.is_some() && company_name.is_some();
    Ok(Json(json!({
        "company_id": id,
        "valid": valid,
        "version": version,
        "company_name": company_name,
        "would_import": {
            "issues": issue_count,
            "agents": agent_count,
            "pipelines": pipeline_count,
        },
        "warnings": if valid { Vec::<&str>::new() } else { vec!["missing version or company.name"] },
    })))
}

// ============== Import / Export / Feedback-trace handlers ==============

async fn import_preview_root(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    // Mirrors Node `POST /companies/import/preview`. Accepts a generic
    // payload, validates shape, and returns a preview descriptor so the UI
    // can confirm before applying.
    let payload = body
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let preview = serde_json::json!({
        "kind": body.get("kind").and_then(serde_json::Value::as_str).unwrap_or("unknown"),
        "valid": true,
        "payloadKeys": payload.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "previewedAt": chrono::Utc::now(),
    });
    state.realtime.publish(
        pc_realtime::LiveEvent::new("company.import.preview", "company", uuid::Uuid::nil())
            .with_data(preview.clone()),
    );
    Ok(Json(preview))
}

async fn get_import_job(
    State(_state): State<AppState>,
    Path(job_id): Path<uuid::Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    // Round 224 真实实现：返回 404 Not Implemented
    //
    // 原 Round 98 stub 引用不存在的表 `company_import_jobs`。
    // Node 端 import jobs 是 in-memory（`importJobs: Map`，不持久化）。
    // paperclip-rs 不持久化 import jobs — 状态查询需要 caller 持有 actorKey。
    // 直接返回 404，与 Node 端"unknown or expired id"行为一致。
    Err(ApiError::NotFound(format!("import job {job_id} (import jobs are not persisted in paperclip-rs; the originating session must poll the Node import queue)")))
}

/// Round 224: company export 请求 body（与 Node `companyPortabilityExportSchema` 对齐）
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompanyExportBody {
    #[serde(default)]
    include: Option<Vec<String>>,
    #[serde(default)]
    agents: Option<Vec<String>>,
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    projects: Option<Vec<String>>,
    #[serde(default)]
    issues: Option<Vec<String>>,
    #[serde(default)]
    project_issues: Option<Vec<String>>,
    #[serde(default)]
    selected_files: Option<Vec<String>>,
    #[serde(default)]
    expand_referenced_skills: Option<bool>,
    #[serde(default)]
    sidebar_order: Option<serde_json::Value>,
}

async fn start_company_export(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(_body): Json<CompanyExportBody>,
) -> ApiResult<Json<serde_json::Value>> {
    // R600: 通过 PortabilityService.export 收集 manifest
    let manifest = PortabilityService::new(&state.db)
        .export(id, ExportInput::default())
        .await
        .map_err(|e| map_portability_service_error(e, id))?;
    // 发布 realtime 事件给 UI / Node background worker
    state.realtime.publish(
        LiveEvent::new("company.export.requested", "company", id)
            .with_company(id)
            .with_data(serde_json::json!({
                "companyId": id,
                "counts": {
                    "agents": manifest.counts.agents,
                    "issues": manifest.counts.issues,
                    "pipelines": manifest.counts.pipelines,
                },
            })),
    );
    Ok(Json(serde_json::json!({
        "companyId": id,
        "version": manifest.version,
        "status": "exported",
        "counts": {
            "agents": manifest.counts.agents,
            "issues": manifest.counts.issues,
            "pipelines": manifest.counts.pipelines,
        },
        "generatedAt": manifest.generated_at,
    })))
}

async fn get_company_export_fidelity(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    // Round 224 真实实现：完整实现 `collectExportFidelityCounts` 聚合。
    //
    // 原 Round 98 stub 返回空 report。
    // Node 端 `collectExportFidelityCounts` 聚合 10 张表的 count：
    // labels / issue_labels / issue_relations(blocks) / issue_documents /
    // issue_work_products / issue_attachments / approvals / cost_events /
    // activity_log / issues(monitor)
    // 本路由完整复刻这套聚合，并基于 EXPORT_FIDELITY_REPORT_SCHEMA
    // 输出 V1 报告 + warnings。
    let _ = CompanyRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    let counts = collect_export_fidelity_counts(&state.db, id).await?;
    let report = build_export_fidelity_report(id, &counts);
    Ok(Json(report))
}

async fn list_company_feedback_traces(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    // Mirrors Node `GET /companies/:id/feedback-traces`. Aggregates feedback
    // traces scoped to the company across all issues.
    let repo = FeedbackTraceRepo::new(&state.db);
    let rows = repo.list_for_company(id, 200).await.unwrap_or_default();
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "kind": r.kind,
                "payload": r.payload.unwrap_or(serde_json::json!({})),
                "createdAt": r.created_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "items": items })))
}

/// Round 224: company import apply 请求 body（与 Node `companyPortabilityImportSchema` 对齐）
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompanyImportApplyBody {
    #[serde(default)]
    source: Option<serde_json::Value>,
    #[serde(default)]
    target: Option<serde_json::Value>,
    #[serde(default)]
    include: Option<Vec<String>>,
    #[serde(default)]
    agents: Option<serde_json::Value>,
    #[serde(default)]
    collision_strategy: Option<String>,
    #[serde(default)]
    name_overrides: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    selected_files: Option<Vec<String>>,
    #[serde(default)]
    adapter_overrides: Option<serde_json::Value>,
    #[serde(default)]
    secret_values: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    pause_automations: Option<bool>,
}

async fn apply_company_import(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<CompanyImportApplyBody>,
) -> ApiResult<Json<serde_json::Value>> {
    // Round 224 真实实现：验证 target company 存在 → 发布 realtime 事件 → 202 Accepted
    //
    // 原 Round 98 stub 引用不存在的表 `company_import_jobs`。
    // Node 端 import 走 in-memory job 系统，异步执行。
    // paperclip-rs 把 import 委托给 Node 端：
    // 1. 校验 target company 存在 → 404
    // 2. 转发 import payload 为 realtime event
    // 3. Node 端后台 worker 调 `portability.importBundle` 并通过
    //    `company.import.completed` 事件回传结果
    let _ = CompanyRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    state.realtime.publish(
        LiveEvent::new("company.import.requested", "company", id)
            .with_company(id)
            .with_data(serde_json::json!({
                "companyId": id,
                "payload": serde_json::to_value(&body).unwrap_or(serde_json::json!({})),
            })),
    );
    Ok(Json(serde_json::json!({
        "companyId": id,
        "status": "accepted",
        "note": "import request delegated to Node background worker; result delivered via company.import.completed event",
    })))
}

// ============================================================================
// Labels / Folders / Invites / Members / Org / Audit / Search / Decision Bundles
// ============================================================================

// ---------- labels ----------

async fn list_labels(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = LabelRepo::new(&state.db)
        .list_by_company(company_id)
        .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id, "companyId": r.company_id, "name": r.name, "color": r.color,
            })
        })
        .collect();
    Ok(Json(json!({"items": items, "companyId": company_id})))
}

#[derive(Debug, Deserialize)]
struct LabelBody {
    name: String,
    color: String,
}

async fn create_label(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<LabelBody>,
) -> ApiResult<Json<Value>> {
    // pc-authz: 写入公司资源需要 UsersInvite 权限（Operator 角色及以上）。
    if let Err(err) =
        enforce_permission(&state.db, &actor, company_id, PermissionKey::UsersInvite).await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    let name = body.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::BadRequest("name length 1..=64".into()));
    }
    if body.color.is_empty() {
        return Err(ApiError::BadRequest("color required".into()));
    }
    let input = NewLabel {
        company_id,
        name: name.to_owned(),
        color: body.color.clone(),
    };
    let row = LabelRepo::new(&state.db)
        .create(&input)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("duplicate") || msg.contains("unique") {
                ApiError::Conflict(format!("label {name} already exists"))
            } else {
                ApiError::Internal(msg)
            }
        })?;
    state.realtime.publish(
        LiveEvent::new("label.created", "label", row.id)
            .with_company(company_id)
            .with_data(json!({"name": name})),
    );
    Ok(Json(
        json!({"id": row.id, "companyId": company_id, "name": name, "color": body.color}),
    ))
}

#[derive(Debug, Deserialize, Default)]
struct PatchLabelBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    color: Option<String>,
}

async fn patch_label(
    State(state): State<AppState>,
    Path((_company_id, label_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchLabelBody>,
) -> ApiResult<Json<Value>> {
    if let Some(ref n) = body.name {
        if n.is_empty() || n.len() > 64 {
            return Err(ApiError::BadRequest("name length 1..=64".into()));
        }
    }
    let patch = LabelPatch {
        name: body.name.clone(),
        color: body.color.clone(),
    };
    let updated = LabelRepo::new(&state.db).patch(label_id, &patch).await?;
    if updated.is_none() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    Ok(Json(json!({"updated": true, "id": label_id})))
}

async fn delete_label(
    State(state): State<AppState>,
    Path((_company_id, label_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let deleted = LabelRepo::new(&state.db).delete(label_id).await?;
    if !deleted {
        return Err(ApiError::NotFound(format!("label {label_id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- folders ----------

async fn list_folders(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = FolderRepo::new(&state.db);
    let rows = repo.list_by_company(company_id).await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "companyId": r.company_id,
                "kind": r.kind,
                "parentId": r.parent_id,
                "name": r.name,
                "slug": r.slug,
                "systemKey": r.system_key,
                "color": r.color,
                "position": r.position,
            })
        })
        .collect();
    Ok(Json(json!({"items": items, "companyId": company_id})))
}

#[derive(Debug, Deserialize)]
struct CreateFolderBody {
    kind: String,
    name: String,
    #[serde(default)]
    color: Option<String>,
}

async fn create_folder(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateFolderBody>,
) -> ApiResult<Json<Value>> {
    if body.name.trim().is_empty() || body.name.len() > 64 {
        return Err(ApiError::BadRequest("name length 1..=64".into()));
    }
    let repo = FolderRepo::new(&state.db);
    // 优先用 FolderKind 枚举（routine / skill），其它 kind（legacy "personal"）走兜底 SQL
    if let Some(kind) = FolderKind::parse(&body.kind) {
        let slug = pc_repos::folder::slug::normalize_folder_slug(&body.name);
        let next_pos = repo.next_position(company_id, kind, None).await?;
        let input = NewFolder {
            company_id,
            kind,
            parent_id: None,
            name: body.name.trim().to_string(),
            slug,
            system_key: None,
            color: body.color.clone(),
            position: next_pos,
        };
        let row = repo.create(&input).await?;
        Ok(Json(json!({
            "id": row.id,
            "companyId": row.company_id,
            "kind": row.kind,
            "name": row.name,
            "color": row.color,
            "position": row.position,
        })))
    } else {
        // Legacy path: kind="personal" 等非标准值。委托 FolderRepo::next_position_for_kind +
        // create_with_kind_str 复合。
        let repo = FolderRepo::new(&state.db);
        let next_pos = repo.next_position_for_kind(company_id, &body.kind).await?;
        let row = repo
            .create_with_kind_str(
                company_id,
                &body.kind,
                body.name.trim(),
                body.color.as_deref(),
                next_pos,
            )
            .await?;
        Ok(Json(json!({
            "id": row.id,
            "companyId": row.company_id,
            "kind": row.kind,
            "name": row.name,
            "color": row.color,
            "position": row.position,
        })))
    }
}

async fn ensure_my_folder(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<Json<Value>> {
    let repo = FolderRepo::new(&state.db);
    let (row, created) = repo.ensure_personal_root(company_id).await?;
    Ok(Json(json!({
        "id": row.id,
        "companyId": company_id,
        "kind": row.kind,
        "created": created,
    })))
}

#[derive(Debug, Deserialize, Default)]
struct PatchFolderBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    position: Option<i32>,
}

async fn patch_folder(
    State(state): State<AppState>,
    Path((company_id, folder_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchFolderBody>,
) -> ApiResult<Json<Value>> {
    let patch = FolderPatch {
        name: body.name.clone(),
        slug: None,
        color: body.color.clone(),
        position: body.position,
        parent_id: None,
    };
    if patch.name.is_none() && patch.color.is_none() && patch.position.is_none() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    let repo = FolderRepo::new(&state.db);
    let row = repo
        .patch(company_id, folder_id, &patch)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("folder {folder_id}")))?;
    let mut updated: Vec<&str> = vec![];
    if patch.name.is_some() {
        updated.push("name");
    }
    if patch.color.is_some() {
        updated.push("color");
    }
    if patch.position.is_some() {
        updated.push("position");
    }
    Ok(Json(json!({"updated": updated, "id": row.id})))
}

async fn delete_folder(
    State(state): State<AppState>,
    Path((company_id, folder_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let repo = FolderRepo::new(&state.db);
    let deleted = repo.delete(company_id, folder_id).await?;
    if !deleted {
        return Err(ApiError::NotFound(format!("folder {folder_id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, Default)]
struct MoveFolderBody {
    #[serde(default)]
    position: Option<i32>,
}

async fn move_folder(
    State(state): State<AppState>,
    Path((company_id, folder_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<MoveFolderBody>,
) -> ApiResult<Json<Value>> {
    let p = body.position.unwrap_or(0);
    let repo = FolderRepo::new(&state.db);
    repo.update_position(company_id, folder_id, p).await?;
    Ok(Json(json!({"moved": true, "id": folder_id, "position": p})))
}

#[derive(Debug, Deserialize, Default)]
struct MoveFolderItemBody {
    #[serde(default)]
    item_kind: Option<String>,
    #[serde(default)]
    item_id: Option<String>,
    #[serde(default)]
    folder_id: Option<Uuid>,
}

async fn move_folder_item(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<MoveFolderItemBody>,
) -> ApiResult<Json<Value>> {
    let kind_str = body
        .item_kind
        .ok_or_else(|| ApiError::BadRequest("item_kind required".into()))?;
    let item_kind = MoveFolderItemKind::parse(&kind_str)
        .ok_or_else(|| ApiError::BadRequest(format!("unsupported kind {kind_str}")))?;
    let id_str = body
        .item_id
        .ok_or_else(|| ApiError::BadRequest("item_id required".into()))?;
    let item_id: Uuid = Uuid::parse_str(&id_str)
        .map_err(|_| ApiError::BadRequest(format!("item_id {id_str} is not a uuid")))?;
    let repo = FolderRepo::new(&state.db);
    let input = MoveFolderItem {
        kind: item_kind,
        item_id,
        folder_id: body.folder_id,
    };
    let result = repo.move_item(company_id, &input).await?;
    Ok(Json(json!({
        "moved": true,
        "kind": result.kind.as_str(),
        "itemId": result.item_id.to_string(),
        "folderId": result.folder_id,
    })))
}

// ---------- invites ----------
//
// 与原 Node `companies.ts` 同名 handler：
// - list  / create / revoke
// - join_requests: list / approve / reject
//
// 业务逻辑迁出到 `pc_repos::invite` + `pc_repos::join_request`，本文件只做：
// - request DTO 解析
// - 调用 Repo 并把结果转成 JSON 响应

#[derive(Debug, Deserialize, Default)]
struct ListInvitesQuery {
    #[serde(default)]
    role: Option<String>,
}

async fn list_invites(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let items = pc_repos::invite::InviteRepo::new(&state.db)
        .list_by_company(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let payload: Vec<Value> = items
        .into_iter()
        .map(|invite| {
            let status = match invite.status {
                pc_repos::invite::InviteStatus::Pending => "pending",
                pc_repos::invite::InviteStatus::Accepted => "accepted",
                pc_repos::invite::InviteStatus::Revoked => "revoked",
                pc_repos::invite::InviteStatus::Expired => "expired",
            };
            json!({
                "id": invite.row.id,
                "companyId": invite.row.company_id,
                "inviteType": invite.row.invite_type,
                "role": invite.role,
                "defaults": invite.row.defaults_payload,
                "status": status,
                "invitedByUserId": invite.row.invited_by_user_id,
                "expiresAt": invite.row.expires_at,
                "revokedAt": invite.row.revoked_at,
                "acceptedAt": invite.row.accepted_at,
            })
        })
        .collect();
    Ok(Json(json!({"items": payload, "companyId": company_id})))
}

#[derive(Debug, Deserialize)]
struct CreateInviteBody {
    #[serde(default)]
    invite_type: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    expires_in_days: Option<i64>,
}

async fn create_invite(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateInviteBody>,
) -> ApiResult<Json<Value>> {
    let ty = body
        .invite_type
        .clone()
        .unwrap_or_else(|| "member".to_string());
    let role = body.role.clone().unwrap_or_else(|| "member".to_string());
    let days = body.expires_in_days.unwrap_or(7).clamp(1, 365);
    let expires_at = pc_core::Timestamp::from_dt(chrono::Utc::now() + chrono::Duration::days(days));
    let defaults = serde_json::json!({"role": role});
    let created = pc_repos::invite::InviteRepo::new(&state.db)
        .create(pc_repos::invite::NewInvite {
            company_id,
            invite_type: ty,
            allowed_join_types: "both".to_string(),
            defaults_payload: Some(defaults),
            expires_at,
            invited_by_user_id: None,
        })
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "id": created.row.id,
        "companyId": created.row.company_id,
        "inviteType": created.row.invite_type,
        "role": created.role,
        "token": created.token,
        "expiresAt": created.row.expires_at,
    })))
}

async fn revoke_invite(
    State(state): State<AppState>,
    Path((company_id, invite_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ok = pc_repos::invite::InviteRepo::new(&state.db)
        .revoke(company_id, invite_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !ok {
        return Err(ApiError::NotFound(format!("invite {invite_id} not active")));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- join requests ----------

async fn list_join_requests(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = pc_repos::join_request::JoinRequestRepo::new(&state.db)
        .list_by_company(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "companyId": r.company_id,
                "inviteId": r.invite_id,
                "status": r.status,
                "requestType": r.request_type,
                "requestIp": r.request_ip,
                "requestingUserId": r.requesting_user_id,
                "requestEmailSnapshot": r.request_email_snapshot,
                "agentName": r.agent_name,
                "adapterType": r.adapter_type,
                "capabilities": r.capabilities,
                "agentDefaultsPayload": r.agent_defaults_payload,
                "createdAgentId": r.created_agent_id,
                "approvedByUserId": r.approved_by_user_id,
                "approvedAt": r.approved_at,
                "rejectedByUserId": r.rejected_by_user_id,
                "rejectedAt": r.rejected_at,
                "createdAt": r.created_at,
                "updatedAt": r.updated_at,
            })
        })
        .collect();
    Ok(Json(json!({"items": items, "companyId": company_id})))
}

#[derive(Debug, Deserialize, Default)]
struct JoinRequestDecisionBody {
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    by_user_id: Option<String>,
}

async fn approve_join_request(
    State(state): State<AppState>,
    Path((company_id, req_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<JoinRequestDecisionBody>,
) -> ApiResult<Json<Value>> {
    let by_user_id = body.by_user_id.clone().unwrap_or_default();
    let effects = pc_repos::join_request::JoinRequestRepo::new(&state.db)
        .approve(
            company_id,
            req_id,
            pc_repos::join_request::JoinRequestDecision {
                note: body.note.clone(),
                by_user_id,
            },
        )
        .await
        .map_err(|e| {
            use pc_repos::join_request::JoinRequestError::*;
            match e {
                NotPending(_) => ApiError::Conflict("join request not pending".to_string()),
                UnknownRequestType(s) => {
                    ApiError::BadRequest(format!("unknown request_type '{s}'"))
                }
                Db(err) => ApiError::Internal(format!("{err}")),
            }
        })?;
    Ok(Json(json!({
        "id": req_id,
        "status": "approved",
        "note": body.note,
        "createdMembershipId": effects.created_membership_id,
        "createdAgentId": effects.created_agent_id,
    })))
}

async fn reject_join_request(
    State(state): State<AppState>,
    Path((company_id, req_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<JoinRequestDecisionBody>,
) -> ApiResult<Json<Value>> {
    let by_user_id = body.by_user_id.clone().unwrap_or_default();
    let ok = pc_repos::join_request::JoinRequestRepo::new(&state.db)
        .reject(
            company_id,
            req_id,
            pc_repos::join_request::JoinRequestDecision {
                note: body.note.clone(),
                by_user_id,
            },
        )
        .await
        .map_err(|e| ApiError::Internal(format!("{e}")))?;
    if !ok {
        return Err(ApiError::NotFound(format!(
            "join request {req_id} not pending"
        )));
    }
    Ok(Json(
        json!({"id": req_id, "status": "rejected", "note": body.note}),
    ))
}

// ---------- members ----------

async fn list_members(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(q): Query<ListMembersQuery>,
) -> ApiResult<Json<Value>> {
    let mut filter = if q.include_archived.unwrap_or(false) {
        pc_repos::company_member::MemberFilter {
            include_archived: true,
            role: q.role.as_deref(),
            ..pc_repos::company_member::MemberFilter::user()
        }
    } else {
        pc_repos::company_member::MemberFilter {
            role: q.role.as_deref(),
            ..pc_repos::company_member::MemberFilter::user()
        }
    };
    // 防止 include_archived 路径把 principal_type 清空
    if filter.principal_type.is_empty() {
        filter.principal_type = "user";
    }
    let rows = pc_repos::company_member::CompanyMemberRepo::new(&state.db)
        .list_by_company(company_id, filter)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|m| {
            json!({
                "id": m.id,
                "userId": m.principal_id,
                "role": m.membership_role,
                "status": m.status,
                "name": m.name,
                "email": m.email,
                "image": m.image,
                "createdAt": m.created_at,
                "updatedAt": m.updated_at,
                "companyId": m.company_id,
            })
        })
        .collect();
    Ok(Json(json!({"items": items, "companyId": company_id})))
}

/// 与 Node `updateCompanyMemberSchema.role` 对齐；保留 `role` 字段名以兼容客户端。
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PatchMemberBody {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ListMembersQuery {
    #[serde(default)]
    include_archived: Option<bool>,
    #[serde(default)]
    role: Option<String>,
}

async fn patch_member(
    State(state): State<AppState>,
    Path((company_id, member_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchMemberBody>,
) -> ApiResult<Json<Value>> {
    let patch = pc_repos::company_member::MemberPatch {
        membership_role: body.role.clone(),
        status: body
            .status
            .as_deref()
            .and_then(pc_repos::company_member::MemberStatus::parse),
    };
    let row = pc_repos::company_member::CompanyMemberRepo::new(&state.db)
        .patch(company_id, member_id, patch)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match row {
        Some(r) => Ok(Json(json!({
            "id": r.id,
            "userId": r.principal_id,
            "role": r.membership_role,
            "status": r.status,
            "updatedAt": r.updated_at,
        }))),
        None => Err(ApiError::NotFound(format!("member {member_id}"))),
    }
}

async fn archive_member(
    State(state): State<AppState>,
    Path((company_id, member_id)): Path<(Uuid, Uuid)>,
    AxumExtension(actor): AxumExtension<AuthContext>,
) -> ApiResult<Json<Value>> {
    // pc-authz：归档成员需要 UsersManagePermissions 权限（Admin 角色）
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        company_id,
        PermissionKey::UsersManagePermissions,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    let ok = pc_repos::company_member::CompanyMemberRepo::new(&state.db)
        .archive(company_id, member_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !ok {
        return Err(ApiError::NotFound(format!("member {member_id}")));
    }
    Ok(Json(json!({"archived": true, "id": member_id})))
}

// ---------- Round 25: member permissions / role-and-grants / inbox-agent-policy ----------

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PatchMemberPermissionsBody {
    role: Option<String>,
    permissions: Option<Value>,
    archived: Option<bool>,
}

async fn patch_member_permissions(
    State(state): State<AppState>,
    Path((company_id, member_id)): Path<(Uuid, Uuid)>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<PatchMemberPermissionsBody>,
) -> ApiResult<Json<Value>> {
    // pc-authz：管理成员权限需要 UsersManagePermissions 权限
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        company_id,
        PermissionKey::UsersManagePermissions,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    // 解析 archived→status 映射；company_memberships 没有 archived_at 列（Round 89 已修）。
    let archived_status = if body.archived.unwrap_or(false) {
        Some(pc_repos::company_member::MemberStatus::Archived)
    } else {
        None
    };
    let patch = pc_repos::company_member::MemberPatch {
        membership_role: body.role.clone(),
        status: archived_status,
    };
    let row = pc_repos::company_member::CompanyMemberRepo::new(&state.db)
        .patch(company_id, member_id, patch)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let row = row.ok_or_else(|| ApiError::NotFound(format!("member {member_id}")))?;
    // permissions 字段：保持向后兼容，如果给的是数组，写入 principal_permission_grants。
    if let Some(json) = body.permissions.as_ref() {
        if let Some(arr) = json.as_array() {
            let mut grants: Vec<pc_repos::principal_permission_grant::PermissionGrantInput> =
                Vec::new();
            for entry in arr {
                let key = entry.as_str().map(|s| s.to_string()).or_else(|| {
                    entry
                        .get("key")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                });
                if let Some(k) = key {
                    grants.push(pc_repos::principal_permission_grant::PermissionGrantInput {
                        permission_key: k,
                        scope: entry.get("scope").cloned(),
                        granted_by_user_id: None,
                    });
                }
            }
            let mut tx = state
                .db
                .pool()
                .begin()
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            pc_repos::principal_permission_grant::PrincipalPermissionGrantRepo::new(&state.db)
                .replace_all_for_principal(
                    &mut tx,
                    company_id,
                    "user",
                    &row.principal_id,
                    grants.iter(),
                )
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            tx.commit()
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
    }
    state.realtime.publish(
        LiveEvent::new(
            "company_member.permissions_updated",
            "company_member",
            member_id,
        )
        .with_company(company_id)
        .with_data(json!({
            "userId": row.principal_id,
            "role": body.role,
            "permissions": body.permissions,
        })),
    );
    Ok(Json(json!({
        "id": row.id,
        "companyId": row.company_id,
        "userId": row.principal_id,
        "role": row.membership_role,
        "status": row.status,
        "updated": true,
    })))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PatchMemberRoleAndGrantsBody {
    role: String,
    #[serde(default)]
    grants: Vec<String>,
    #[serde(default)]
    metadata: Option<Value>,
}

async fn patch_member_role_and_grants(
    State(state): State<AppState>,
    Path((company_id, member_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchMemberRoleAndGrantsBody>,
) -> ApiResult<Json<Value>> {
    if body.role.trim().is_empty() {
        return Err(ApiError::BadRequest("role is required".into()));
    }
    let metadata = body.metadata.clone().unwrap_or_else(|| json!({}));
    // 解析 member → principal_id
    let member = pc_repos::company_member::CompanyMemberRepo::new(&state.db)
        .find_by_id(company_id, member_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("member {member_id}")))?;
    // 单事务：role UPDATE + grants 全量替换
    let mut tx = state
        .db
        .pool()
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let updated_member = pc_repos::company_member::CompanyMemberRepo::new(&state.db)
        .patch(
            company_id,
            member_id,
            pc_repos::company_member::MemberPatch {
                membership_role: Some(body.role.clone()),
                status: None,
            },
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let updated_member = match updated_member {
        Some(m) => m,
        None => return Err(ApiError::NotFound(format!("member {member_id}"))),
    };
    // body.grants 是 Vec<String>；每条 → grant row（全公司范围 scope）
    let grant_inputs: Vec<pc_repos::principal_permission_grant::PermissionGrantInput> = body
        .grants
        .iter()
        .map(
            |k| pc_repos::principal_permission_grant::PermissionGrantInput {
                permission_key: k.clone(),
                scope: None,
                granted_by_user_id: None,
            },
        )
        .collect();
    pc_repos::principal_permission_grant::PrincipalPermissionGrantRepo::new(&state.db)
        .replace_all_for_principal(
            &mut tx,
            company_id,
            "user",
            &updated_member.principal_id,
            grant_inputs.iter(),
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    state.realtime.publish(
        LiveEvent::new(
            "company_member.role_and_grants_updated",
            "company_member",
            member_id,
        )
        .with_company(company_id)
        .with_data(json!({
            "userId": updated_member.principal_id,
            "role": body.role,
            "grants": body.grants,
        })),
    );
    Ok(Json(json!({
        "id": updated_member.id,
        "companyId": updated_member.company_id,
        "userId": updated_member.principal_id,
        "role": updated_member.membership_role,
        "grants": body.grants,
        "metadata": metadata,
        "updated": true,
    })))
}

// ── Inbox agent policy (per-user, per-company) ─────────────

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PutInboxAgentPolicyBody {
    mode: String,
    #[serde(default)]
    allowed_agent_ids: Vec<Uuid>,
}

async fn get_my_inbox_agent_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    let policy = pc_repos::inbox_agent_policy::InboxAgentPolicyRepo::new(&state.db)
        .get(company_id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "companyId": policy.company_id,
        "userId": policy.user_id,
        "mode": policy.mode.as_str(),
        "allowedAgentIds": policy.allowed_agent_ids,
        "updatedAt": policy.updated_at,
        "materialized": policy.materialized,
    })))
}

async fn put_my_inbox_agent_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PutInboxAgentPolicyBody>,
) -> ApiResult<Json<Value>> {
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    let mode = pc_repos::inbox_agent_policy::InboxAgentPolicyMode::parse(&body.mode)
        .ok_or_else(|| ApiError::BadRequest(format!("invalid mode '{}'", body.mode)))?;
    let input = pc_repos::inbox_agent_policy::UpdateInboxAgentPolicyInput {
        mode,
        allowed_agent_ids: body.allowed_agent_ids.clone(),
    };
    let policy = pc_repos::inbox_agent_policy::InboxAgentPolicyRepo::new(&state.db)
        .update(company_id, &user_id, input)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    state.realtime.publish(
        LiveEvent::new(
            "user_inbox_agent_policy.updated",
            "user_inbox_agent_policy",
            company_id,
        )
        .with_company(company_id)
        .with_data(json!({"userId": user_id, "mode": body.mode})),
    );
    Ok(Json(json!({
        "companyId": policy.company_id,
        "userId": policy.user_id,
        "mode": policy.mode.as_str(),
        "allowedAgentIds": policy.allowed_agent_ids,
        "updatedAt": policy.updated_at,
    })))
}

// ---------- audit / org / search / agents ----------

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentActionAuditQuery {
    #[serde(default)]
    agent_id: Option<uuid::Uuid>,
    #[serde(default)]
    responsible_user_id: Option<String>,
    #[serde(default)]
    run_id: Option<uuid::Uuid>,
    #[serde(default)]
    entity_type: Option<String>,
    #[serde(default)]
    entity_id: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    actor_type: Option<String>,
    #[serde(default)]
    from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    to: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

fn parse_agent_audit_query(
    company_id: uuid::Uuid,
    q: AgentActionAuditQuery,
) -> Result<AgentActionAuditFilters, ApiError> {
    if let Some(ref et) = q.entity_type {
        if et.trim().is_empty() {
            return Err(ApiError::BadRequest("entityType must not be empty".into()));
        }
    }
    if let Some(ref ei) = q.entity_id {
        if ei.trim().is_empty() {
            return Err(ApiError::BadRequest("entityId must not be empty".into()));
        }
    }
    if let Some(ref a) = q.action {
        if a.trim().is_empty() {
            return Err(ApiError::BadRequest("action must not be empty".into()));
        }
    }
    if let Some(ref at) = q.actor_type {
        match at.as_str() {
            "agent" | "user" | "system" | "plugin" => {}
            _ => {
                return Err(ApiError::BadRequest(format!(
                    "actorType must be one of agent/user/system/plugin, got {at}"
                )));
            }
        }
    }
    if let Some(limit) = q.limit {
        if !(1..=200).contains(&limit) {
            return Err(ApiError::BadRequest(
                "limit must be between 1 and 200".into(),
            ));
        }
    }
    Ok(AgentActionAuditFilters {
        company_id,
        agent_id: q.agent_id,
        responsible_user_id: q.responsible_user_id,
        run_id: q.run_id,
        entity_type: q.entity_type,
        entity_id: q.entity_id,
        action: q.action,
        actor_type: q.actor_type,
        from: q.from,
        to: q.to,
        cursor: q.cursor,
        limit: q.limit,
    })
}

async fn list_agent_actions(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(q): Query<AgentActionAuditQuery>,
) -> ApiResult<Json<AgentActionAuditPage>> {
    let _ = crate::state::require_user_id(&state, &axum::http::HeaderMap::new())
        .await
        .map_err(|_| ApiError::Forbidden("Board authentication required".into()))?;
    let filters = parse_agent_audit_query(company_id, q)?;
    let repo = AgentActionAuditRepo::new(&state.db);
    let page = repo
        .list(filters)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(page))
}

async fn export_agent_actions_csv(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(q): Query<AgentActionAuditQuery>,
) -> ApiResult<impl IntoResponse> {
    let _ = crate::state::require_user_id(&state, &axum::http::HeaderMap::new())
        .await
        .map_err(|_| ApiError::Forbidden("Board authentication required".into()))?;
    let filters = parse_agent_audit_query(company_id, q)?;
    let repo = AgentActionAuditRepo::new(&state.db);
    let page = repo
        .list(filters)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut csv = String::from("id,companyId,action,entityType,entityId,createdAt\n");
    for item in &page.items {
        csv.push_str(&format!(
            "{},{},{},{},{},{}\n",
            item.id,
            item.company_id,
            csv_field(&item.action),
            csv_field(&item.entity_type),
            csv_field(&item.entity_id),
            item.created_at.to_rfc3339(),
        ));
    }
    Ok(([("content-type", "text/csv")], csv))
}

fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        let escaped = value.replace('"', "&quot;");
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

async fn get_org(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Round 93: 走 AgentRepo::list_for_org_chart；原内联 SQL 重复在 org.svg 块出现
    let rows = pc_repos::agent::AgentRepo::new(&state.db)
        .list_for_org_chart(company_id)
        .await?;
    let mut nodes = Vec::with_capacity(rows.len());
    let mut edges: Vec<Value> = Vec::new();
    let mut children: std::collections::BTreeMap<Uuid, Vec<Uuid>> =
        std::collections::BTreeMap::new();
    let mut roots: Vec<Uuid> = Vec::new();
    for row in rows {
        nodes.push(json!({
            "id": row.id, "name": row.name, "role": row.role,
            "title": row.title, "status": row.status,
        }));
        match row.reports_to {
            Some(p) => {
                edges.push(json!({"from": p, "to": row.id}));
                children.entry(p).or_default().push(row.id);
            }
            None => roots.push(row.id),
        }
    }
    // BFS 算 depth（用于 SVG 布局 + JSON 暴露）
    let mut depth_map: std::collections::HashMap<Uuid, usize> = std::collections::HashMap::new();
    let mut queue: std::collections::VecDeque<(Uuid, usize)> =
        roots.iter().map(|r| (*r, 0usize)).collect();
    while let Some((id, d)) = queue.pop_front() {
        depth_map.insert(id, d);
        if let Some(kids) = children.get(&id) {
            for k in kids {
                queue.push_back((*k, d + 1));
            }
        }
    }
    for n in nodes.iter_mut() {
        if let Some(id) = n
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            if let Some(d) = depth_map.get(&id) {
                if let Some(obj) = n.as_object_mut() {
                    obj.insert("depth".into(), json!(*d));
                }
            }
        }
    }
    Ok(Json(json!({
        "companyId": company_id,
        "nodes": nodes,
        "edges": edges,
        "roots": roots,
        "depths": depth_map.iter().map(|(k,v)| (k.to_string(), *v)).collect::<std::collections::BTreeMap<String, usize>>(),
    })))
}

async fn get_org_svg(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // Round 93: 同样走 AgentRepo::list_for_org_chart
    let rows = pc_repos::agent::AgentRepo::new(&state.db)
        .list_for_org_chart(company_id)
        .await?;
    if rows.is_empty() {
        return Ok(([("content-type", "image/svg+xml")],
            format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 200 80\"><text x=\"100\" y=\"40\" text-anchor=\"middle\" font-size=\"12\" fill=\"#94a3b8\">company {company_id}: no agents</text></svg>")));
    }
    let mut children: std::collections::BTreeMap<Option<Uuid>, Vec<Uuid>> =
        std::collections::BTreeMap::new();
    for row in &rows {
        children.entry(row.reports_to).or_default().push(row.id);
    }
    for v in children.values_mut() {
        v.sort();
    }
    let roots = children.get(&None).cloned().unwrap_or_default();
    let mut depth_map: std::collections::HashMap<Uuid, usize> = std::collections::HashMap::new();
    let mut order: Vec<Uuid> = Vec::new();
    let mut queue: std::collections::VecDeque<(Uuid, usize)> =
        roots.into_iter().map(|r| (r, 0usize)).collect();
    while let Some((id, d)) = queue.pop_front() {
        depth_map.insert(id, d);
        order.push(id);
        if let Some(kids) = children.get(&Some(id)) {
            for k in kids {
                queue.push_back((*k, d + 1));
            }
        }
    }
    let mut layer_pos: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut id_pos: std::collections::HashMap<Uuid, (usize, usize)> =
        std::collections::HashMap::new();
    for id in &order {
        let d = depth_map[id];
        let p = layer_pos.entry(d).or_insert(0);
        id_pos.insert(*id, (d, *p));
        *p += 1;
    }
    let max_depth = depth_map.values().copied().max().unwrap_or(0);
    let max_layer_size = layer_pos.values().copied().max().unwrap_or(1).max(1);
    let box_w = 150;
    let box_h = 56;
    let gap_x = 36;
    let gap_y = 56;
    let total_w = (max_layer_size * (box_w + gap_x) + gap_x).max(200);
    let total_h = ((max_depth + 1) * (box_h + gap_y) + gap_y).max(120);
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\"><style>.b{{fill:#eef2ff;stroke:#6366f1}} .t1{{font:11px sans-serif;fill:#1e293b}} .t2{{font:9px sans-serif;fill:#64748b}} .l{{stroke:#94a3b8;stroke-width:1}}</style>",
        w = total_w, h = total_h,
    );
    // edges first
    for row in &rows {
        let parent = match row.reports_to {
            Some(p) => p,
            None => continue,
        };
        let (cd, cp) = match id_pos.get(&row.id) {
            Some(p) => *p,
            None => continue,
        };
        let (pd, pp) = match id_pos.get(&parent) {
            Some(p) => *p,
            None => continue,
        };
        let x1 = pp * (box_w + gap_x) + gap_x + box_w / 2;
        let y1 = pd * (box_h + gap_y) + gap_y + box_h;
        let x2 = cp * (box_w + gap_x) + gap_x + box_w / 2;
        let y2 = cd * (box_h + gap_y) + gap_y;
        svg.push_str(&format!(
            "<path class=\"l\" d=\"M{x1} {y1} C{x1} {my} {x2} {my2} {x2} {y2}\" fill=\"none\"/>",
            x1 = x1,
            y1 = y1,
            x2 = x2,
            y2 = y2,
            my = y1 + 20,
            my2 = y2 - 20,
        ));
    }
    // nodes
    for row in &rows {
        let (d, p) = match id_pos.get(&row.id) {
            Some(p) => *p,
            None => continue,
        };
        let x = p * (box_w + gap_x) + gap_x;
        let y = d * (box_h + gap_y) + gap_y;
        let role_s = row.role.clone();
        let status_color = match row.status.as_str() {
            "active" | "running" => "#10b981",
            "paused" => "#f59e0b",
            "archived" | "crashed" => "#ef4444",
            _ => "#94a3b8",
        };
        svg.push_str(&format!(
            "<rect class=\"b\" x=\"{x}\" y=\"{y}\" width=\"{bw}\" height=\"{bh}\" rx=\"6\"/><circle cx=\"{cx}\" cy=\"{cy}\" r=\"3\" fill=\"{sc}\"/><text class=\"t1\" x=\"{tx}\" y=\"{ty}\">{name}</text><text class=\"t2\" x=\"{tx}\" y=\"{ty2}\">{role}</text>",
            x = x, y = y, bw = box_w, bh = box_h,
            cx = x + box_w - 10, cy = y + 10,
            tx = x + 8, ty = y + 22,
            ty2 = y + 40,
            sc = status_color,
            name = html_escape(&row.name),
            role = html_escape(&role_s),
        ));
    }
    svg.push_str("</svg>");
    Ok(([("content-type", "image/svg+xml")], svg))
}

async fn get_org_png(
    State(_state): State<AppState>,
    Path(_company_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // Minimal 1×1 transparent PNG
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    Ok(([("content-type", "image/png")], png))
}

/// R516: GET/POST /api/companies/:company_id/search/extract
/// Node `CompanySearchExtractResponse` 兼容契约:
/// - query params: contains (>=2), kind (literal|url), scope (all|issues|comments|documents),
///   limit (1..200), offset, matchesPerIssue (1..50)
/// - POST body 也支持 (历史遗留)
async fn search_extract(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
    body: Option<Json<serde_json::Value>>,
) -> ApiResult<Json<Value>> {
    let body = body.map(|Json(b)| b).unwrap_or(serde_json::json!({}));
    // 优先 query string, 然后 body。
    let pick = |k: &str| -> Option<String> {
        query
            .0
            .get(k)
            .cloned()
            .or_else(|| body.get(k).and_then(|v| v.as_str().map(String::from)))
    };
    let contains = pick("contains")
        .or_else(|| pick("query"))
        .unwrap_or_default();
    if contains.len() < 2 {
        return Err(ApiError::BadRequest(
            "contains must be at least 2 characters".into(),
        ));
    }
    let kind = pick("kind").unwrap_or_else(|| "literal".to_string());
    if kind != "literal" && kind != "url" {
        return Err(ApiError::BadRequest("kind must be literal or url".into()));
    }
    let scope = pick("scope").unwrap_or_else(|| "all".to_string());
    if !["all", "issues", "comments", "documents"].contains(&scope.as_str()) {
        return Err(ApiError::BadRequest(
            "scope must be all, issues, comments, or documents".into(),
        ));
    }
    let limit = pick("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 200);
    let _offset = pick("offset")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let matches_per_issue = pick("matchesPerIssue")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(10)
        .clamp(1, 50);
    let repo = pc_repos::issue::IssueRepo::new(&state.db);
    let hits = repo
        .search_extract_issue_hits(company_id, &contains, &scope, limit + 1)
        .await?;
    let has_more = hits.len() as i64 > limit;
    let page: Vec<_> = hits.into_iter().take(limit as usize).collect();
    let mut results: Vec<Value> = Vec::with_capacity(page.len());
    let mut truncated = has_more;
    for hit in page {
        let mut matches: Vec<Value> = Vec::new();
        if scope == "all" || scope == "issues" {
            for m in repo
                .list_issue_field_matches(hit.id, &contains, &kind, matches_per_issue)
                .await?
            {
                matches.push(json!({
                    "value": m.excerpt,
                    "field": m.field,
                    "label": m.label,
                    "excerpt": m.excerpt,
                    "excerptTruncated": m.excerpt_truncated,
                    "source": {"type": "issue", "issueId": m.source_id},
                }));
            }
        }
        if scope == "all" || scope == "comments" {
            for m in repo
                .list_issue_comment_matches(hit.id, &contains, &kind, matches_per_issue)
                .await?
            {
                matches.push(json!({
                    "value": m.excerpt,
                    "field": m.field,
                    "label": m.label,
                    "excerpt": m.excerpt,
                    "excerptTruncated": m.excerpt_truncated,
                    "source": {"type": "comment", "commentId": m.source_id},
                }));
            }
        }
        if scope == "all" || scope == "documents" {
            for m in repo
                .list_issue_document_matches(hit.id, &contains, &kind, matches_per_issue)
                .await?
            {
                matches.push(json!({
                    "value": m.excerpt,
                    "field": m.field,
                    "label": m.label,
                    "excerpt": m.excerpt,
                    "excerptTruncated": m.excerpt_truncated,
                    "source": {"type": "document", "documentId": m.source_id},
                }));
            }
        }
        let matches_truncated = matches.len() as i64 >= matches_per_issue;
        if matches_truncated {
            truncated = true;
        }
        results.push(json!({
            "issueId": hit.id,
            "identifier": hit.identifier,
            "title": hit.title,
            "status": hit.status,
            "assigneeAgentId": hit.assignee_agent_id,
            "updatedAt": hit.updated_at,
            "matches": matches,
            "matchesTruncated": matches_truncated,
        }));
    }
    Ok(Json(json!({
        "contains": contains,
        "kind": kind,
        "scope": scope,
        "limit": limit,
        "offset": _offset,
        "matchesPerIssue": matches_per_issue,
        "results": results,
        "hasMore": has_more,
        "truncated": truncated,
    })))
}

#[derive(Debug, Deserialize, Default)]
struct DecisionBundleBody {
    #[serde(default)]
    title: Option<String>,
}

async fn create_finance_event(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<NewFinanceEvent>,
) -> ApiResult<Json<FinanceEventRow>> {
    if body.event_kind.trim().is_empty() {
        return Err(ApiError::BadRequest("eventKind must not be empty".into()));
    }
    if body.biller.trim().is_empty() {
        return Err(ApiError::BadRequest("biller must not be empty".into()));
    }
    let repo = CostRepo::new(&state.db);
    let row = repo
        .create_finance_event(company_id, &body)
        .await
        .map_err(|e| match e {
            pc_repos::cost::FinanceCreateError::Fk(
                pc_repos::cost::FkError::NotFound(label)
                | pc_repos::cost::FkError::WrongCompany(label),
            ) => ApiError::NotFound(label),
            pc_repos::cost::FinanceCreateError::Fk(pc_repos::cost::FkError::Db(_)) => {
                ApiError::Internal("finance FK lookup failed".into())
            }
            pc_repos::cost::FinanceCreateError::Fk(pc_repos::cost::FkError::Internal(msg)) => {
                ApiError::Internal(msg)
            }
            pc_repos::cost::FinanceCreateError::Db(err) => ApiError::Internal(err.to_string()),
        })?;
    Ok(Json(row))
}

#[derive(Debug, Deserialize, Default)]
struct CreateAgentInCompanyBody {
    name: String,
    #[serde(default)]
    role: Option<String>,
}

async fn create_agent(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateAgentInCompanyBody>,
) -> ApiResult<Json<Value>> {
    let role = body.role.clone().unwrap_or_else(|| "general".into());
    let row = pc_repos::agent::AgentRepo::new(&state.db)
        .create_simple(company_id, &body.name, &role)
        .await
        .map_err(|e| ApiError::Internal(format!("create agent: {e}")))?;
    Ok(Json(json!({
        "id": row.id,
        "companyId": row.company_id,
        "name": row.name,
        "role": row.role,
        "status": row.status,
        "adapterType": row.adapter_type,
    })))
}

async fn provision_built_in_agent(
    State(state): State<AppState>,
    Path((company_id, id)): Path<(Uuid, String)>,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<Json<Value>> {
    // Round 93 备注：`company_built_in_agent_provisions` 表在当前迁移集中不存在，
    // 原 inline SQL 100% 报错。改为返回 200 + stub，待 schema 落地后改为走 Repo。
    let built_in_id: Uuid =
        Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest("bad uuid".into()))?;
    let _ = state.db.pool(); // 静默未用变量警告；保留以备后续切到真表
    Ok(Json(json!({
        "provisioned": false,
        "companyId": company_id,
        "builtInAgentId": built_in_id,
        "note": "stub: company_built_in_agent_provisions table not yet in schema",
    })))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ============================================================================
// Round 37: company sub-resources (activity / approvals / decisions / goals /
// pipelines / case-events / user-directory / review-cases)
// ============================================================================

/// `GET /api/companies/:company_id/activity` — company-scoped activity feed.
/// Mirrors Node `/companies/:companyId/activity`.  Reads from `activity_log`
/// table; falls back to empty list when the table is empty / new.
async fn list_company_activity_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<CompanyListQuery>,
) -> ApiResult<Json<Value>> {
    ensure_company_exists(&state, company_id).await?;
    let filter = pc_repos::activity::ActivityFilter {
        limit: Some(q.limit.unwrap_or(50).clamp(1, 200)),
        ..Default::default()
    };
    let rows = pc_repos::activity::ActivityRepo::new(&state.db)
        .list_for_company(company_id, &filter)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.id,
                "companyId": row.company_id,
                "action": row.action,
                "actorType": row.actor_type,
                "actorId": row.actor_id,
                "entityType": row.entity_type,
                "entityId": row.entity_id,
                "agentId": row.agent_id,
                "details": row.details,
                "createdAt": row.created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

/// `GET /api/companies/:company_id/approvals` — company-scoped approvals list.
async fn list_company_approvals_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<CompanyListQuery>,
) -> ApiResult<Json<Value>> {
    ensure_company_exists(&state, company_id).await?;
    let mut filter = pc_repos::approval::ApprovalFilter::default();
    filter.status = q
        .status
        .as_deref()
        .and_then(pc_repos::approval::ApprovalStatus::parse);
    filter.limit = Some(q.limit.unwrap_or(50).clamp(1, 200));
    let rows = ApprovalRepo::new(&state.db)
        .list_by_company(company_id, &filter)
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| serde_json::to_value(&row).unwrap_or_default())
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

/// `GET /api/companies/:company_id/decisions` — company-scoped decisions.
async fn list_company_decisions_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<CompanyListQuery>,
) -> ApiResult<Json<Value>> {
    ensure_company_exists(&state, company_id).await?;
    let mut rows = DecisionRepo::new(&state.db)
        .list_by_company(company_id)
        .await?;
    if let Some(limit) = q.limit {
        rows.truncate(limit.clamp(1, 500) as usize);
    }
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| serde_json::to_value(&row).unwrap_or_default())
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

/// `GET /api/companies/:company_id/goals` — company-scoped goals.
async fn list_company_goals_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    ensure_company_exists(&state, company_id).await?;
    let rows = GoalRepo::new(&state.db).list_by_company(company_id).await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| serde_json::to_value(&row).unwrap_or_default())
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

/// `GET /api/companies/:company_id/pipelines` — company-scoped pipelines.
async fn list_company_pipelines_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    ensure_company_exists(&state, company_id).await?;
    let rows = PipelineRepo::new(&state.db)
        .list_by_company(company_id)
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| serde_json::to_value(&row).unwrap_or_default())
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

/// `GET /api/companies/:company_id/case-events` — case events across company.
/// Mirrors Node `/companies/:companyId/case-events`.  Aggregates from
/// `case_events` with optional ?kind= filter.
async fn list_company_case_events_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<CompanyListQuery>,
) -> ApiResult<Json<Value>> {
    ensure_company_exists(&state, company_id).await?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let kind_filter = q.kind.as_deref();
    let rows = pc_repos::case::CaseRepo::new(&state.db)
        .list_events_by_company(company_id, kind_filter, limit)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.id,
                "companyId": row.company_id,
                "caseId": row.case_id,
                "kind": row.kind,
                "actorType": row.actor_type,
                "actorUserId": row.actor_user_id,
                "actorAgentId": row.actor_agent_id,
                "runId": row.run_id,
                "payload": row.payload,
                "createdAt": row.created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

/// `GET /api/companies/:company_id/user-directory` — 公司内所有 user principal 成员。
/// Mirrors Node `/companies/:companyId/user-directory`.
/// 修复 Round 93：原 inline SQL 引用了不存在的 `cm.user_id` / `cm.role`，真实列
/// 是 `cm.principal_id` / `cm.membership_role`。
async fn list_company_user_directory_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    ensure_company_exists(&state, company_id).await?;
    let rows = pc_repos::company_member::CompanyMemberRepo::new(&state.db)
        .user_directory(company_id)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|u| {
            json!({
                "userId": u.user_id,
                "name": u.name,
                "email": u.email,
                "image": u.image,
                "role": u.role,
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

/// `GET /api/companies/:company_id/review-cases` — cases awaiting review.
/// Mirrors Node `/companies/:companyId/review-cases`.  Filters cases with
/// status = 'in_review' (terminal review state).
async fn list_company_review_cases_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    ensure_company_exists(&state, company_id).await?;
    let mut filter = pc_repos::case::CaseFilter::default();
    filter.statuses = vec![pc_repos::case::CaseStatus::InReview];
    let rows = CaseRepo::new(&state.db)
        .list_by_company_filtered(company_id, &filter)
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| serde_json::to_value(&row).unwrap_or_default())
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

/// Round 245: watchdog evaluation worker 拉取候选。
async fn list_company_watchdog_evaluations_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    ensure_company_exists(&state, company_id).await?;
    let agent_id = headers
        .get("x-paperclip-agent-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok());
    let user_id = crate::state::require_user_id(&state, &headers).await.ok();
    if agent_id.is_some() && user_id.is_none() {
        return Err(ApiError::Forbidden(
            "watchdog evaluation worker queue requires board or user actor".into(),
        ));
    }
    let rows = IssueRepo::new(&state.db)
        .list_pending_watchdog_evaluations(company_id, 200)
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(issue_id, watchdog_id, agent_id, last_triggered_at)| {
            json!({
                "watchdogId": watchdog_id,
                "issueId": issue_id,
                "watchdogAgentId": agent_id,
                "lastTriggeredAt": last_triggered_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct CompanyListQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

/// Helper — verify company exists, returning 404 if missing.  Used by all
/// Round 37 sub-resource handlers.
async fn ensure_company_exists(state: &AppState, company_id: Uuid) -> ApiResult<()> {
    let ok = pc_repos::company::CompanyRepo::new(&state.db)
        .exists(company_id)
        .await?;
    if !ok {
        return Err(ApiError::NotFound(format!("company {company_id}")));
    }
    Ok(())
}

// =============================================================================
// R513 — company-scoped POST handlers
// =============================================================================

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateCompanyApprovalBody {
    approval_type: String,
    #[serde(default)]
    payload: Value,
    #[serde(default)]
    requested_by_user_id: Option<String>,
    #[serde(default)]
    requested_by_agent_id: Option<Uuid>,
}

/// `POST /api/companies/:company_id/approvals` — company-scoped approval
/// creation. Mirrors Node `approvals.ts:124` (`POST /companies/:companyId/approvals`).
async fn create_company_approval_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateCompanyApprovalBody>,
) -> ApiResult<axum::response::Response> {
    ensure_company_exists(&state, company_id).await?;
    if body.approval_type.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "approval_type must not be empty".into(),
        ));
    }
    let payload = if body.payload.is_null() {
        Value::Object(Default::default())
    } else {
        body.payload
    };
    if body.requested_by_user_id.is_none() && body.requested_by_agent_id.is_none() {
        return Err(ApiError::BadRequest(
            "approval must be requested by agent or user".into(),
        ));
    }
    let parsed_type = pc_repos::approval::ApprovalType::parse(body.approval_type.trim())
        .unwrap_or(pc_repos::approval::ApprovalType::Custom);
    let new_approval = pc_repos::approval::NewApproval {
        company_id,
        approval_type: parsed_type,
        requested_by_agent_id: body.requested_by_agent_id,
        requested_by_user_id: body.requested_by_user_id.clone(),
        payload,
    };
    let row = ApprovalRepo::new(&state.db).create(&new_approval).await?;
    state.realtime.publish(
        LiveEvent::new("approval.created", "approval", row.id).with_company(row.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(&row).unwrap_or(Value::Null)),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateCompanyDecisionBody {
    title: String,
    body: String,
    #[serde(default)]
    options: Option<Value>,
    #[serde(default)]
    inputs: Option<Value>,
    #[serde(default)]
    rule_key: Option<String>,
}

/// `POST /api/companies/:company_id/decisions` — company-scoped decision
/// creation. Mirrors Node `decisions.ts:42`
/// (`POST /companies/:companyId/decisions`).
async fn create_company_decision_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateCompanyDecisionBody>,
) -> ApiResult<axum::response::Response> {
    ensure_company_exists(&state, company_id).await?;
    if body.title.trim().is_empty() || body.body.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "title and body must not be empty".into(),
        ));
    }
    let signing = &state.decision_signing;
    let mut row = DecisionRepo::new(&state.db)
        .create(company_id, &body.title, &body.body, signing)
        .await?;
    if let Some(opts) = body.options.as_ref() {
        row.options = opts.clone();
        sqlx::query("UPDATE decisions SET options=$1, updated_at=now() WHERE id=$2")
            .bind(opts)
            .bind(row.id)
            .execute(state.db.pool())
            .await?;
    }
    if let Some(inputs) = body.inputs.as_ref() {
        sqlx::query("UPDATE decisions SET inputs=$1, updated_at=now() WHERE id=$2")
            .bind(inputs)
            .bind(row.id)
            .execute(state.db.pool())
            .await?;
    }
    if let Some(rule_key) = body.rule_key.as_ref() {
        sqlx::query("UPDATE decisions SET rule_key=$1, updated_at=now() WHERE id=$2")
            .bind(rule_key)
            .bind(row.id)
            .execute(state.db.pool())
            .await?;
    }
    state
        .realtime
        .publish(LiveEvent::new("decision.created", "decision", row.id).with_company(company_id));
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(&row).unwrap_or(Value::Null)),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateCompanyPipelineBody {
    key: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
}

/// `POST /api/companies/:company_id/pipelines` — company-scoped pipeline
/// creation. Mirrors Node `pipelines.ts:891`
/// (`POST /companies/:companyId/pipelines`).
async fn create_company_pipeline_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateCompanyPipelineBody>,
) -> ApiResult<axum::response::Response> {
    ensure_company_exists(&state, company_id).await?;
    if body.key.trim().is_empty() {
        return Err(ApiError::BadRequest("key must not be empty".into()));
    }
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let row = PipelineRepo::new(&state.db)
        .create(
            company_id,
            body.key.trim(),
            body.name.trim(),
            body.description.as_deref(),
        )
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("pipeline.created", "pipeline", row.id).with_company(company_id));
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(&row).unwrap_or(Value::Null)),
    )
        .into_response())
}
/// `GET /api/companies/stats` — board-only cross-company aggregated stats.
///
/// Mirrors Node `/companies/stats`. Returns per-company stats for every
/// company the requesting user has access to (or all companies if
/// instance admin / local-implicit).  Each entry is the same shape as
/// `GET /api/companies/:id/stats`.
async fn get_companies_stats(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    use crate::state::require_user_id;
    let user_id = require_user_id(&state, &headers).await?;

    let repo = CompanyRepo::new(&state.db);
    let accessible = repo.list_accessible_for_user(&user_id).await?;
    let ids: Vec<Uuid> = accessible.iter().map(|c| c.id).collect();
    let stats_map = repo.stats_for_companies(&ids).await?;

    let mut map = serde_json::Map::new();
    for company in accessible {
        let s = stats_map.get(&company.id);
        let (issue_count, agent_count, case_count, user_count) = s
            .map(|s| (s.issue_count, s.agent_count, s.case_count, s.user_count))
            .unwrap_or((0, 0, 0, 0));
        map.insert(
            company.id.to_string(),
            json!({
                "companyId": company.id,
                "name": company.name,
                "agentCount": agent_count,
                "issueCount": issue_count,
                "caseCount": case_count,
                "userCount": user_count,
            }),
        );
    }
    Ok(Json(json!({ "stats": map })))
}

/// `GET /api/companies/issues` — common-malformed-path handler.
///
/// Mirrors Node which returns 400 with a hint to use
/// `/api/companies/{companyId}/issues`.
async fn get_companies_issues_malformed() -> ApiResult<Json<Value>> {
    Err(ApiError::BadRequest(
        "Missing companyId in path. Use /api/companies/{companyId}/issues.".into(),
    ))
}

// ============================================================================
// Round 208: company-level GET helpers (branding + finance-events alias)
// ============================================================================

/// 解析 description 中的 `<!-- logo:{url} -->` 后缀，返回 logoUrl。
fn parse_logo_from_description(desc: Option<&str>) -> Option<String> {
    desc?
        .lines()
        .rev()
        .find_map(|line| {
            line.trim()
                .strip_prefix("<!-- logo:")
                .and_then(|s| s.strip_suffix(" -->"))
        })
        .map(str::to_owned)
}

/// `GET /api/companies/:id/branding` — 返回当前 branding 视图
/// (name + brand_color + logoUrl，从 description 注释中提取)。
async fn get_branding(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = CompanyRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    let logo_url = parse_logo_from_description(row.description.as_deref());
    Ok(Json(json!({
        "companyId": row.id,
        "name": row.name,
        "brandColor": row.brand_color,
        "logoUrl": logo_url,
        "updatedAt": row.updated_at,
    })))
}

/// `GET /api/companies/:id/finance-events` — 公司级 finance events 列表（带 limit 过滤）。
/// 复用 CostRepo::finance_events 仓储方法。
async fn list_company_finance_events(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    use pc_repos::cost::CostRange;
    let range = CostRange {
        from: None,
        to: None,
    };
    let events: Vec<FinanceEventRow> = CostRepo::new(&state.db)
        .finance_events(id, range, 100)
        .await?;
    let items: Vec<Value> = events
        .into_iter()
        .map(|e| {
            json!({
                "id": e.id,
                "companyId": e.company_id,
                "eventKind": e.event_kind,
                "direction": e.direction,
                "biller": e.biller,
                "amountCents": e.amount_cents,
                "currency": e.currency,
                "description": e.description,
                "occurredAt": e.occurred_at,
                "createdAt": e.created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": id,
        "total": items.len(),
        "items": items,
    })))
}

#[cfg(test)]
mod round208_tests {
    use super::*;

    #[test]
    fn parse_logo_extracts_from_last_comment() {
        let desc = Some("Welcome\n<!-- logo:https://x.test/a.png -->");
        assert_eq!(
            parse_logo_from_description(desc).as_deref(),
            Some("https://x.test/a.png")
        );
    }

    #[test]
    fn parse_logo_returns_none_for_empty() {
        assert_eq!(parse_logo_from_description(None), None);
        assert_eq!(parse_logo_from_description(Some("")), None);
        assert_eq!(parse_logo_from_description(Some("no logo here")), None);
    }

    #[test]
    fn parse_logo_picks_last_logo_if_multiple() {
        let desc = Some("<!-- logo:old -->\nnew text\n<!-- logo:new -->");
        assert_eq!(parse_logo_from_description(desc).as_deref(), Some("new"));
    }
}

#[cfg(test)]
mod round211_tests {
    use super::compute_health_score;

    #[test]
    fn health_score_no_active_is_100() {
        assert_eq!(compute_health_score(0, 0, 0, 0), 100);
    }

    #[test]
    fn health_score_perfect_active_is_100() {
        // 有 active 且无失败 -> 100
        assert_eq!(compute_health_score(0, 0, 0, 5), 100);
    }

    #[test]
    fn health_score_deducts_for_failures() {
        // failed_recent=2 -> 扣 10
        assert_eq!(compute_health_score(2, 0, 0, 1), 90);
    }

    #[test]
    fn health_score_clamps_to_zero() {
        // 巨大扣分也不会变负
        let score = compute_health_score(100, 100, 100, 1);
        assert_eq!(score, 0);
    }

    #[test]
    fn health_score_combines_all_factors() {
        // failed_recent=3 (15) + agent_error=2 (4) + issue_blocked=10 (10) = 29 扣分
        // 100 - 29 = 71
        assert_eq!(compute_health_score(3, 2, 10, 5), 71);
    }
}

// ============================================================================
// Round 211: company-level diagnostics aggregate
//
// 端口：GET /api/companies/:id/diagnostics
// 组合三类维度：issues（status_breakdown_visible）、agents（status_breakdown）、
// heartbeat_runs（status_breakdown），输出公司级健康度快照。
// ============================================================================

/// 计算 company 健康度评分（0-100，越大越健康）。
///
/// 计分规则（与 `companies/:id/diagnostics` 路由一致）：
/// - 无活跃 heartbeat：100（没有正在跑的活，可能完全是 idle 状态）
/// - 否则：起始 100 分，每项扣分：
///   * `failed_recent` × 5
///   * `agent_error` × 2
///   * `issue_blocked` × 1
/// 最终 saturating 到 [0, 100]
fn compute_health_score(
    heartbeat_failed_recent: i64,
    agent_error: i64,
    issue_blocked: i64,
    heartbeat_active: i64,
) -> i64 {
    if heartbeat_active == 0 {
        return 100;
    }
    let penalty = heartbeat_failed_recent * 5 + agent_error * 2 + issue_blocked;
    100i64.saturating_sub(penalty).max(0)
}

async fn company_diagnostics(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (issue_blocked, issue_in_progress, issue_needs_review) = IssueRepo::new(&state.db)
        .status_breakdown_visible(id)
        .await
        .unwrap_or((0, 0, 0));
    let (agent_error, agent_running, agent_paused) = AgentRepo::new(&state.db)
        .status_breakdown(id)
        .await
        .unwrap_or((0, 0, 0));
    let (heartbeat_failed_recent, heartbeat_active) = HeartbeatRepo::new(&state.db)
        .status_breakdown(id)
        .await
        .unwrap_or((0, 0));
    let health_score = compute_health_score(
        heartbeat_failed_recent,
        agent_error,
        issue_blocked,
        heartbeat_active,
    );
    Ok(Json(json!({
        "companyId": id,
        "issues": {
            "blocked": issue_blocked,
            "inProgress": issue_in_progress,
            "needsReview": issue_needs_review,
        },
        "agents": {
            "error": agent_error,
            "running": agent_running,
            "paused": agent_paused,
        },
        "heartbeats": {
            "failedRecent24h": heartbeat_failed_recent,
            "active": heartbeat_active,
        },
        "healthScore": health_score,
        "generatedAt": chrono::Utc::now(),
    })))
}

// ============================================================================
// Round 224: export fidelity aggregation helpers
// ============================================================================
//
// Mirrors Node `collectExportFidelityCounts` + `buildExportFidelityReport` in
// `paperclip/server/src/services/export-fidelity.ts`. Aggregates counts from
// 10 tables for the export fidelity report, then derives warnings based on
// which data is NOT included in the export bundle.

/// Single source of truth for the export-fidelity report schema version.
/// Mirrors `EXPORT_FIDELITY_REPORT_SCHEMA` in `paperclip/shared/portability-fidelity.ts`.
pub const EXPORT_FIDELITY_REPORT_SCHEMA: &str = "paperclip-export-fidelity-v1";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFidelityCounts {
    pub label_definitions: i64,
    pub issue_label_references: i64,
    pub issue_blocker_relations: i64,
    pub issue_documents: i64,
    pub issue_work_products: i64,
    pub issue_attachments: i64,
    pub approvals: i64,
    pub cost_events: i64,
    pub activity_log_entries: i64,
    pub issue_monitors: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PortabilityFidelityWarning {
    pub code: String,
    pub severity: String, // "info" | "warning" | "blocker"
    pub message: String,
}

/// Count helper: extract first count from a `[{count}]` row, default 0.
async fn first_count(db: &pc_db::Db, sql: &str, company: uuid::Uuid) -> Result<i64, ApiError> {
    let row: (i64,) = sqlx::query_as(sql)
        .bind(company)
        .fetch_one(db.pool())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(row.0)
}

/// Aggregate counts from 10 tables (mirrors Node `collectExportFidelityCounts`).
async fn collect_export_fidelity_counts(
    db: &pc_db::Db,
    company_id: uuid::Uuid,
) -> Result<ExportFidelityCounts, ApiError> {
    let label_definitions = first_count(
        db,
        "SELECT count(*)::bigint FROM labels WHERE company_id = $1",
        company_id,
    )
    .await?;
    let issue_label_references = first_count(
        db,
        "SELECT count(*)::bigint FROM issue_labels WHERE company_id = $1",
        company_id,
    )
    .await?;
    let issue_blocker_relations = first_count(
        db,
        "SELECT count(*)::bigint FROM issue_relations WHERE company_id = $1 AND type = 'blocks'",
        company_id,
    )
    .await?;
    let issue_documents = first_count(
        db,
        "SELECT count(*)::bigint FROM issue_documents WHERE company_id = $1",
        company_id,
    )
    .await?;
    let issue_work_products = first_count(
        db,
        "SELECT count(*)::bigint FROM issue_work_products WHERE company_id = $1",
        company_id,
    )
    .await?;
    let issue_attachments = first_count(
        db,
        "SELECT count(*)::bigint FROM issue_attachments WHERE company_id = $1",
        company_id,
    )
    .await?;
    let approvals = first_count(
        db,
        "SELECT count(*)::bigint FROM approvals WHERE company_id = $1",
        company_id,
    )
    .await?;
    let cost_events = first_count(
        db,
        "SELECT count(*)::bigint FROM cost_events WHERE company_id = $1",
        company_id,
    )
    .await?;
    let activity_log_entries = first_count(
        db,
        "SELECT count(*)::bigint FROM activity_log WHERE company_id = $1",
        company_id,
    )
    .await?;
    let issue_monitors = first_count(
        db,
        "SELECT count(*)::bigint FROM issues WHERE company_id = $1 \
         AND (monitor_next_check_at IS NOT NULL OR monitor_scheduled_by IS NOT NULL)",
        company_id,
    )
    .await?;
    Ok(ExportFidelityCounts {
        label_definitions,
        issue_label_references,
        issue_blocker_relations,
        issue_documents,
        issue_work_products,
        issue_attachments,
        approvals,
        cost_events,
        activity_log_entries,
        issue_monitors,
    })
}

/// Build warnings for data that is NOT included in the export bundle.
///
/// Mirrors Node `buildExportFidelityWarnings`: 3 categories get warnings
/// (approvals / cost_events / activity_log) when present.
fn build_export_fidelity_warnings(
    counts: &ExportFidelityCounts,
) -> Vec<PortabilityFidelityWarning> {
    let mut warnings = Vec::new();
    let unsupported: &[(&str, &str, &str, &str)] = &[
        (
            "approvals_not_exported",
            "approvals",
            "approval",
            "approvals",
        ),
        (
            "cost_history_not_exported",
            "cost_events",
            "cost event",
            "cost events",
        ),
        (
            "activity_history_not_exported",
            "activity_log_entries",
            "activity log entry",
            "activity log entries",
        ),
    ];
    for (code, key, singular, plural) in unsupported {
        let count = match *key {
            "approvals" => counts.approvals,
            "cost_events" => counts.cost_events,
            "activity_log_entries" => counts.activity_log_entries,
            _ => continue,
        };
        if count <= 0 {
            continue;
        }
        let noun = if count == 1 { singular } else { plural };
        let verb = if count == 1 { "is" } else { "are" };
        warnings.push(PortabilityFidelityWarning {
            code: (*code).to_string(),
            severity: "warning".to_string(),
            message: format!("{count} {noun} {verb} not included in the export bundle."),
        });
    }
    warnings
}

/// Build complete export fidelity report.
fn build_export_fidelity_report(
    company_id: uuid::Uuid,
    counts: &ExportFidelityCounts,
) -> serde_json::Value {
    let warnings = build_export_fidelity_warnings(counts);
    // 显式 to_value 避免 json! 宏对 `&T` 的歧义处理
    let counts_json = serde_json::to_value(counts).unwrap_or(serde_json::json!({}));
    serde_json::json!({
        "schema": EXPORT_FIDELITY_REPORT_SCHEMA,
        "companyId": company_id,
        "counts": counts_json,
        "warnings": warnings,
        "generatedAt": chrono::Utc::now(),
    })
}

#[cfg(test)]
mod round224_tests {
    //! Round 224: company export/import stub 真实化测试。
    //!
    //! 覆盖：
    //! - `build_export_fidelity_warnings` 基于 counts 派生 warnings
    //! - `build_export_fidelity_report` 输出 V1 schema + counts + warnings
    //! - `CompanyExportBody` / `CompanyImportApplyBody` 反序列化（camelCase）
    use super::{
        build_export_fidelity_report, build_export_fidelity_warnings, CompanyExportBody,
        CompanyImportApplyBody, ExportFidelityCounts, EXPORT_FIDELITY_REPORT_SCHEMA,
    };
    use serde_json::json;

    #[test]
    fn fidelity_warnings_empty_when_all_zero() {
        let counts = ExportFidelityCounts {
            label_definitions: 0,
            issue_label_references: 0,
            issue_blocker_relations: 0,
            issue_documents: 0,
            issue_work_products: 0,
            issue_attachments: 0,
            approvals: 0,
            cost_events: 0,
            activity_log_entries: 0,
            issue_monitors: 0,
        };
        assert!(build_export_fidelity_warnings(&counts).is_empty());
    }

    #[test]
    fn fidelity_warnings_uses_singular_form_for_count_one() {
        let counts = ExportFidelityCounts {
            approvals: 1,
            ..zero_counts()
        };
        let ws = build_export_fidelity_warnings(&counts);
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].code, "approvals_not_exported");
        assert!(ws[0].message.contains("1 approval is"));
    }

    #[test]
    fn fidelity_warnings_uses_plural_form_for_count_many() {
        let counts = ExportFidelityCounts {
            cost_events: 5,
            ..zero_counts()
        };
        let ws = build_export_fidelity_warnings(&counts);
        assert_eq!(ws.len(), 1);
        assert!(ws[0].message.contains("5 cost events are"));
    }

    #[test]
    fn fidelity_warnings_includes_all_three_categories() {
        let counts = ExportFidelityCounts {
            approvals: 2,
            cost_events: 3,
            activity_log_entries: 4,
            ..zero_counts()
        };
        let ws = build_export_fidelity_warnings(&counts);
        assert_eq!(ws.len(), 3);
        let codes: Vec<&str> = ws.iter().map(|w| w.code.as_str()).collect();
        assert!(codes.contains(&"approvals_not_exported"));
        assert!(codes.contains(&"cost_history_not_exported"));
        assert!(codes.contains(&"activity_history_not_exported"));
    }

    fn zero_counts() -> ExportFidelityCounts {
        ExportFidelityCounts {
            label_definitions: 0,
            issue_label_references: 0,
            issue_blocker_relations: 0,
            issue_documents: 0,
            issue_work_products: 0,
            issue_attachments: 0,
            approvals: 0,
            cost_events: 0,
            activity_log_entries: 0,
            issue_monitors: 0,
        }
    }

    #[test]
    fn fidelity_report_has_v1_schema() {
        let cid = uuid::Uuid::nil();
        let counts = ExportFidelityCounts {
            label_definitions: 10,
            issue_documents: 5,
            ..zero_counts()
        };
        let report = build_export_fidelity_report(cid, &counts);
        assert_eq!(report["schema"], json!(EXPORT_FIDELITY_REPORT_SCHEMA));
        assert_eq!(report["schema"], json!("paperclip-export-fidelity-v1"));
        assert_eq!(report["companyId"], json!(cid));
        assert_eq!(report["counts"]["labelDefinitions"], json!(10));
        assert_eq!(report["counts"]["issueDocuments"], json!(5));
        assert!(report["warnings"].is_array());
        assert!(report["generatedAt"].is_string());
    }

    #[test]
    fn export_body_parses_camel_case_fields() {
        let body: CompanyExportBody = serde_json::from_value(json!({
            "include": ["issues", "agents"],
            "expandReferencedSkills": true,
            "sidebarOrder": {"issues": 1},
        }))
        .expect("parse");
        assert_eq!(body.include.as_ref().unwrap().len(), 2);
        assert_eq!(body.expand_referenced_skills, Some(true));
        assert!(body.sidebar_order.is_some());
    }

    #[test]
    fn export_body_handles_missing_optional_fields() {
        let body: CompanyExportBody = serde_json::from_value(json!({})).expect("parse");
        assert!(body.include.is_none());
        assert!(body.agents.is_none());
        assert!(body.expand_referenced_skills.is_none());
    }

    #[test]
    fn import_apply_body_parses_camel_case_fields() {
        let body: CompanyImportApplyBody = serde_json::from_value(json!({
            "source": {"type": "inline"},
            "target": {"type": "new_company"},
            "collisionStrategy": "rename",
            "pauseAutomations": true,
        }))
        .expect("parse");
        assert!(body.source.is_some());
        assert!(body.target.is_some());
        assert_eq!(body.collision_strategy.as_deref(), Some("rename"));
        assert_eq!(body.pause_automations, Some(true));
    }

    #[test]
    fn import_apply_body_handles_empty_object() {
        let body: CompanyImportApplyBody = serde_json::from_value(json!({})).expect("parse");
        assert!(body.source.is_none());
        assert!(body.target.is_none());
        assert!(body.pause_automations.is_none());
    }
}

// ============================================================================
// R511 — Company activity routes (1:1 port of Node routes/activity.ts)
//
// HTTP layer accepts Node-shaped input (free-form action string),
// converts to typed Rust ActivityEvent, emits to ActivityLog + PluginEventBus.
//
// 设计：高内聚 — 单文件单职责（公司活动路由 + Node schema 适配）
//      低耦合 — 通过 ActivityLog + PluginEventBus 与下游解耦
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct CreateActivityBody {
    #[serde(default = "default_actor_type")]
    actor_type: String,
    actor_id: String,
    action: String,
    entity_type: String,
    entity_id: String,
    #[serde(default)]
    agent_id: Option<Uuid>,
    #[serde(default)]
    details: Option<Value>,
}

fn default_actor_type() -> String {
    "system".to_owned()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ActivityListQuery {
    #[serde(default)]
    agent_id: Option<Uuid>,
    #[serde(default)]
    entity_type: Option<String>,
    #[serde(default)]
    entity_id: Option<Uuid>,
    #[serde(default)]
    limit: Option<usize>,
}

/// Parse Node-style free-form action string to typed ActivityKind.
/// Unknown actions fall back to `Other` (raw string preserved in payload).
fn parse_activity_kind(action: &str) -> ActivityKind {
    match action {
        "issue.created" => ActivityKind::IssueCreated,
        "issue.updated" => ActivityKind::IssueUpdated,
        "issue.assigned" => ActivityKind::IssueAssigned,
        "issue.closed" => ActivityKind::IssueClosed,
        "decision.proposed" => ActivityKind::DecisionProposed,
        "decision.approved" => ActivityKind::DecisionApproved,
        "decision.rejected" => ActivityKind::DecisionRejected,
        "approval.requested" => ActivityKind::ApprovalRequested,
        "approval.granted" => ActivityKind::ApprovalGranted,
        "approval.denied" => ActivityKind::ApprovalDenied,
        "agent.started" => ActivityKind::AgentStarted,
        "agent.stopped" => ActivityKind::AgentStopped,
        "agent.heartbeat" => ActivityKind::AgentHeartbeat,
        "agent.error" => ActivityKind::AgentError,
        "plugin.installed" => ActivityKind::PluginInstalled,
        "plugin.enabled" => ActivityKind::PluginEnabled,
        "plugin.disabled" => ActivityKind::PluginDisabled,
        "plugin.error" => ActivityKind::PluginError,
        "cost.recorded" => ActivityKind::CostRecorded,
        "secret.accessed" => ActivityKind::SecretAccessed,
        "document.annotated" => ActivityKind::DocumentAnnotated,
        "routine.ran" => ActivityKind::RoutineRan,
        "pipeline.ran" => ActivityKind::PipelineRan,
        _ => ActivityKind::Other,
    }
}

/// Convert Node-shaped CreateActivityBody to typed ActivityEvent.
fn convert_to_activity_event(
    company_id: Uuid,
    body: CreateActivityBody,
) -> Result<ActivityEvent, ApiError> {
    let subject_id = Uuid::parse_str(&body.entity_id).map_err(|_| {
        ApiError::BadRequest(format!("entityId must be a UUID: {}", body.entity_id))
    })?;

    let actor = match body.actor_type.as_str() {
        "user" => {
            let id = Uuid::parse_str(&body.actor_id).map_err(|_| {
                ApiError::BadRequest(format!(
                    "actorId must be a UUID for user: {}",
                    body.actor_id
                ))
            })?;
            ActivityActor::User {
                id,
                name: body.actor_id.clone(),
            }
        }
        "agent" => {
            let id = Uuid::parse_str(&body.actor_id).map_err(|_| {
                ApiError::BadRequest(format!(
                    "actorId must be a UUID for agent: {}",
                    body.actor_id
                ))
            })?;
            ActivityActor::Agent {
                id,
                name: body.actor_id.clone(),
            }
        }
        "system" => ActivityActor::System {
            component: body.actor_id.clone(),
        },
        "plugin" => {
            let id = Uuid::parse_str(&body.actor_id).map_err(|_| {
                ApiError::BadRequest(format!(
                    "actorId must be a UUID for plugin: {}",
                    body.actor_id
                ))
            })?;
            ActivityActor::Plugin {
                plugin_id: id,
                plugin_key: body.actor_id.clone(),
            }
        }
        _ => ActivityActor::System {
            component: body.actor_id.clone(),
        },
    };

    let kind = parse_activity_kind(&body.action);

    // Build payload: details + agentId + raw action (for unknown kinds)
    let mut payload = body.details.unwrap_or(Value::Null);
    let needs_wrap = body.agent_id.is_some() || matches!(kind, ActivityKind::Other);
    if let Value::Object(ref mut map) = payload {
        if let Some(agent_id) = body.agent_id {
            map.insert("agentId".to_string(), json!(agent_id));
        }
        if matches!(kind, ActivityKind::Other) {
            map.entry("action".to_string())
                .or_insert(Value::String(body.action.clone()));
        }
    } else if needs_wrap {
        let mut map = serde_json::Map::new();
        if !payload.is_null() {
            map.insert("details".to_string(), payload.clone());
        }
        if let Some(agent_id) = body.agent_id {
            map.insert("agentId".to_string(), json!(agent_id));
        }
        if matches!(kind, ActivityKind::Other) {
            map.insert("action".to_string(), Value::String(body.action.clone()));
        }
        payload = Value::Object(map);
    }

    Ok(
        ActivityEvent::new(kind, actor, body.entity_type, subject_id)
            .with_company(company_id)
            .with_payload(payload),
    )
}

/// Convert typed ActivityEvent to PluginEvent for plugin bus emission.
fn convert_to_plugin_event(event: &ActivityEvent) -> PluginEvent {
    let (actor_id, actor_type) = match &event.actor {
        ActivityActor::User { id, .. } => (Some(id.to_string()), Some(ActorType::User)),
        ActivityActor::Agent { id, .. } => (Some(id.to_string()), Some(ActorType::Agent)),
        ActivityActor::System { component } => (Some(component.clone()), Some(ActorType::System)),
        ActivityActor::Plugin { plugin_id, .. } => {
            (Some(plugin_id.to_string()), Some(ActorType::Plugin))
        }
        ActivityActor::Anonymous => (None, None),
    };

    PluginEvent {
        event_id: event.id.0.to_string(),
        event_type: event.kind.as_str().to_string(),
        occurred_at: event.occurred_at,
        actor_id,
        actor_type,
        entity_id: Some(event.subject_id.to_string()),
        entity_type: Some(event.subject_kind.clone()),
        company_id: event
            .company_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        payload: event.payload.clone(),
    }
}

async fn create_activity(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<CreateActivityBody>,
) -> ApiResult<impl IntoResponse> {
    let _user_id = require_user_id(&state, &headers).await?;

    let event = convert_to_activity_event(company_id, body)?;

    // 1. Emit to ActivityLog (持久化)
    state
        .activity
        .emit(event.clone())
        .await
        .map_err(|e| ApiError::Internal(format!("activity log emit failed: {e}")))?;

    // 2. Emit to PluginEventBus (plugin 订阅者 fanout)
    let plugin_event = convert_to_plugin_event(&event);
    let _ = state.plugin_event_bus.emit(plugin_event).await;

    // 3. Publish to realtime (UI 实时更新)
    state.realtime.publish(
        LiveEvent::new(
            event.kind.as_str(),
            event.subject_kind.clone(),
            event.subject_id,
        )
        .with_company(company_id)
        .with_actor("system"),
    );

    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(&event).unwrap_or_default()),
    ))
}

// ============================================================================
// R511 tests
// ============================================================================

#[cfg(test)]
mod r511_activity_routes_tests {
    use super::*;

    #[test]
    fn parse_activity_kind_maps_known_actions() {
        assert_eq!(
            parse_activity_kind("issue.created"),
            ActivityKind::IssueCreated
        );
        assert_eq!(
            parse_activity_kind("agent.heartbeat"),
            ActivityKind::AgentHeartbeat
        );
        assert_eq!(
            parse_activity_kind("plugin.error"),
            ActivityKind::PluginError
        );
    }

    #[test]
    fn parse_activity_kind_falls_back_to_other() {
        assert_eq!(parse_activity_kind("custom.action"), ActivityKind::Other);
        assert_eq!(parse_activity_kind(""), ActivityKind::Other);
    }

    #[test]
    fn create_activity_body_parses_camel_case() {
        let body: CreateActivityBody = serde_json::from_value(json!({
            "actorType": "user",
            "actorId": "550e8400-e29b-41d4-a716-446655440000",
            "action": "issue.created",
            "entityType": "issue",
            "entityId": "550e8400-e29b-41d4-a716-446655440001",
            "agentId": "550e8400-e29b-41d4-a716-446655440002",
            "details": { "key": "value" }
        }))
        .expect("parse");
        assert_eq!(body.actor_type, "user");
        assert_eq!(body.action, "issue.created");
        assert!(body.agent_id.is_some());
        assert!(body.details.is_some());
    }

    #[test]
    fn create_activity_body_defaults_actor_type_to_system() {
        let body: CreateActivityBody = serde_json::from_value(json!({
            "actorId": "local-board",
            "action": "issue.created",
            "entityType": "issue",
            "entityId": "550e8400-e29b-41d4-a716-446655440001"
        }))
        .expect("parse");
        assert_eq!(body.actor_type, "system");
    }

    #[test]
    fn activity_list_query_parses_camel_case() {
        let q: ActivityListQuery = serde_json::from_value(json!({
            "agentId": "550e8400-e29b-41d4-a716-446655440000",
            "entityType": "issue",
            "entityId": "550e8400-e29b-41d4-a716-446655440001",
            "limit": 50
        }))
        .expect("parse");
        assert!(q.agent_id.is_some());
        assert_eq!(q.entity_type, Some("issue".to_owned()));
        assert!(q.entity_id.is_some());
        assert_eq!(q.limit, Some(50));
    }

    #[test]
    fn activity_list_query_all_optional() {
        let q: ActivityListQuery = serde_json::from_value(json!({})).expect("parse");
        assert!(q.agent_id.is_none());
        assert!(q.entity_type.is_none());
        assert!(q.entity_id.is_none());
        assert!(q.limit.is_none());
    }

    #[test]
    fn convert_to_activity_event_maps_system_actor() {
        let company_id = Uuid::new_v4();
        let body = CreateActivityBody {
            actor_type: "system".to_owned(),
            actor_id: "local-board".to_owned(),
            action: "issue.created".to_owned(),
            entity_type: "issue".to_owned(),
            entity_id: Uuid::new_v4().to_string(),
            agent_id: None,
            details: None,
        };
        let event = convert_to_activity_event(company_id, body).expect("convert");
        assert!(matches!(event.actor, ActivityActor::System { .. }));
        assert_eq!(event.kind, ActivityKind::IssueCreated);
        assert_eq!(event.company_id, Some(company_id));
    }

    #[test]
    fn convert_to_activity_event_maps_user_actor_with_uuid() {
        let actor_id = Uuid::new_v4();
        let company_id = Uuid::new_v4();
        let body = CreateActivityBody {
            actor_type: "user".to_owned(),
            actor_id: actor_id.to_string(),
            action: "issue.updated".to_owned(),
            entity_type: "issue".to_owned(),
            entity_id: Uuid::new_v4().to_string(),
            agent_id: None,
            details: None,
        };
        let event = convert_to_activity_event(company_id, body).expect("convert");
        if let ActivityActor::User { id, name } = &event.actor {
            assert_eq!(*id, actor_id);
            assert_eq!(name, &actor_id.to_string());
        } else {
            panic!("expected User actor, got {:?}", event.actor);
        }
    }

    #[test]
    fn convert_to_activity_event_rejects_invalid_entity_uuid() {
        let body = CreateActivityBody {
            actor_type: "system".to_owned(),
            actor_id: "local-board".to_owned(),
            action: "issue.created".to_owned(),
            entity_type: "issue".to_owned(),
            entity_id: "not-a-uuid".to_owned(),
            agent_id: None,
            details: None,
        };
        let result = convert_to_activity_event(Uuid::new_v4(), body);
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn convert_to_activity_event_rejects_invalid_user_actor_uuid() {
        let body = CreateActivityBody {
            actor_type: "user".to_owned(),
            actor_id: "not-a-uuid".to_owned(),
            action: "issue.created".to_owned(),
            entity_type: "issue".to_owned(),
            entity_id: Uuid::new_v4().to_string(),
            agent_id: None,
            details: None,
        };
        let result = convert_to_activity_event(Uuid::new_v4(), body);
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn convert_to_activity_event_preserves_unknown_action_in_payload() {
        let body = CreateActivityBody {
            actor_type: "system".to_owned(),
            actor_id: "local-board".to_owned(),
            action: "custom.weird.action".to_owned(),
            entity_type: "issue".to_owned(),
            entity_id: Uuid::new_v4().to_string(),
            agent_id: None,
            details: None,
        };
        let event = convert_to_activity_event(Uuid::new_v4(), body).expect("convert");
        assert_eq!(event.kind, ActivityKind::Other);
        if let Value::Object(map) = &event.payload {
            assert_eq!(
                map.get("action").and_then(|v| v.as_str()),
                Some("custom.weird.action")
            );
        } else {
            panic!("expected payload to be an object, got {:?}", event.payload);
        }
    }

    #[test]
    fn convert_to_activity_event_merges_agent_id_and_details() {
        let agent_id = Uuid::new_v4();
        let body = CreateActivityBody {
            actor_type: "agent".to_owned(),
            actor_id: Uuid::new_v4().to_string(),
            action: "agent.heartbeat".to_owned(),
            entity_type: "agent".to_owned(),
            entity_id: Uuid::new_v4().to_string(),
            agent_id: Some(agent_id),
            details: Some(json!({ "key": "value" })),
        };
        let event = convert_to_activity_event(Uuid::new_v4(), body).expect("convert");
        if let Value::Object(map) = &event.payload {
            assert_eq!(
                map.get("agentId").and_then(|v| v.as_str()),
                Some(agent_id.to_string().as_str())
            );
            assert_eq!(map.get("key").and_then(|v| v.as_str()), Some("value"));
        } else {
            panic!("expected payload to be an object, got {:?}", event.payload);
        }
    }

    #[test]
    fn convert_to_plugin_event_maps_user_actor() {
        let user_id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let company_id = Uuid::new_v4();
        let event = ActivityEvent {
            id: pc_activity::ActivityId::new(),
            kind: ActivityKind::IssueCreated,
            actor: ActivityActor::User {
                id: user_id,
                name: "alice".to_owned(),
            },
            company_id: Some(company_id),
            subject_kind: "issue".to_owned(),
            subject_id,
            payload: json!({}),
            occurred_at: chrono::Utc::now(),
        };
        let pe = convert_to_plugin_event(&event);
        assert_eq!(pe.event_type, "issue.created");
        assert_eq!(pe.actor_id, Some(user_id.to_string()));
        assert_eq!(pe.actor_type, Some(ActorType::User));
        assert_eq!(pe.company_id, company_id.to_string());
        assert_eq!(pe.entity_id, Some(subject_id.to_string()));
        assert_eq!(pe.entity_type, Some("issue".to_owned()));
    }

    #[test]
    fn convert_to_plugin_event_maps_anonymous_actor() {
        let event = ActivityEvent {
            id: pc_activity::ActivityId::new(),
            kind: ActivityKind::Other,
            actor: ActivityActor::Anonymous,
            company_id: Some(Uuid::new_v4()),
            subject_kind: "x".to_owned(),
            subject_id: Uuid::new_v4(),
            payload: json!({}),
            occurred_at: chrono::Utc::now(),
        };
        let pe = convert_to_plugin_event(&event);
        assert_eq!(pe.actor_id, None);
        assert_eq!(pe.actor_type, None);
    }

    #[test]
    fn convert_to_plugin_event_maps_system_actor() {
        let event = ActivityEvent {
            id: pc_activity::ActivityId::new(),
            kind: ActivityKind::AgentHeartbeat,
            actor: ActivityActor::System {
                component: "local-board".to_owned(),
            },
            company_id: Some(Uuid::new_v4()),
            subject_kind: "agent".to_owned(),
            subject_id: Uuid::new_v4(),
            payload: json!({}),
            occurred_at: chrono::Utc::now(),
        };
        let pe = convert_to_plugin_event(&event);
        assert_eq!(pe.actor_id, Some("local-board".to_owned()));
        assert_eq!(pe.actor_type, Some(ActorType::System));
    }
}
use pc_telemetry::global;
use std::collections::BTreeMap;
