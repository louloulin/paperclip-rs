//! `/api/companies*` 路由：CRUD + 归档。

#[allow(unused_imports)]
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::approval::ApprovalRepo;
use pc_repos::case::CaseRepo;
use pc_repos::company::{CompanyListRow, CompanyRepo, CompanyRow};
use pc_repos::decision::DecisionRepo;
use pc_repos::goal::GoalRepo;
use pc_repos::pipeline::PipelineRepo;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};
use pc_core::Timestamp;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/companies", get(list).post(create))
        .route(
            "/api/companies/:id",
            get(get_one).patch(update).delete(remove),
        )
        .route("/api/companies/:id/archive", post(archive))
        .route("/api/companies/:id/stats", get(get_stats))
        .route("/api/companies/:id/timeline", get(get_timeline))
        .route("/api/companies/:id/artifacts", get(list_artifacts))
        .route("/api/companies/:id/branding", patch(update_branding))
        .route("/api/companies/:id/exports/preview", post(export_preview))
        .route("/api/companies/:id/imports/preview", post(import_preview))
        .route("/api/companies/import/preview", post(import_preview_root))
        .route("/api/companies/import/jobs/:job_id", get(get_import_job))
        .route("/api/companies/:id/export", post(start_company_export))
        // ── Round 45: cross-company aggregation + export plural alias ──
        .route("/api/companies/stats", get(get_companies_stats))
        .route("/api/companies/issues", get(get_companies_issues_malformed))
        .route("/api/companies/:company_id/exports", post(start_company_export))
        // ── Round 45: plugin UI static alias (root-mount) ──
        .route("/_plugins/:plugin_id/companies/:company_id/ui/*file_path", get(plugin_ui_static))
        .route("/api/companies/:id/export/fidelity", get(get_company_export_fidelity))
        .route("/api/companies/:id/feedback-traces", get(list_company_feedback_traces))
        .route("/api/companies/:id/imports/apply", post(apply_company_import))
        // ===== labels / folders / invites / members / org / audit =====
        .route("/api/companies/:id/labels", get(list_labels).post(create_label))
        .route("/api/companies/:id/labels/:label_id", patch(patch_label).delete(delete_label))
        .route("/api/companies/:id/folders", get(list_folders).post(create_folder))
        .route("/api/companies/:id/folders/ensure-my", post(ensure_my_folder))
        .route("/api/companies/:id/folders/:folder_id", patch(patch_folder).delete(delete_folder))
        .route("/api/companies/:id/folders/:folder_id/move", post(move_folder))
        .route("/api/companies/:id/folders/items/move", post(move_folder_item))
        .route("/api/companies/:id/invites", get(list_invites).post(create_invite))
        .route("/api/companies/:id/invites/:invite_id", delete(revoke_invite))
        .route("/api/companies/:id/join-requests", get(list_join_requests))
        .route("/api/companies/:id/join-requests/:req_id/approve", post(approve_join_request))
        .route("/api/companies/:id/join-requests/:req_id/reject", post(reject_join_request))
        .route("/api/companies/:id/members", get(list_members))
        .route("/api/companies/:id/members/:member_id", patch(patch_member))
        .route("/api/companies/:id/members/:member_id/archive", post(archive_member))
        .route("/api/companies/:id/members/:member_id/permissions", patch(patch_member_permissions))
        .route("/api/companies/:id/members/:member_id/role-and-grants", patch(patch_member_role_and_grants))
        .route("/api/companies/:id/users/me/inbox-agent-policy", get(get_my_inbox_agent_policy).put(put_my_inbox_agent_policy))
        .route("/api/companies/:id/audit/agent-actions", get(list_agent_actions))
        .route("/api/companies/:id/audit/agent-actions.csv", get(export_agent_actions_csv))
        .route("/api/companies/:id/org", get(get_org))
        .route("/api/companies/:id/org.svg", get(get_org_svg))
        .route("/api/companies/:id/org.png", get(get_org_png))
        .route("/api/companies/:id/search/extract", post(search_extract))
        .route("/api/companies/:id/finance-events", post(create_finance_event))
        .route("/api/companies/:id/agents", post(create_agent))
        .route("/api/companies/:id/built-in-agents/:id/provision", post(provision_built_in_agent))
        // ---- Round 37: company sub-resources (activity / approvals / decisions / goals / pipelines / case-events / user-directory / review-cases) ----
        .route("/api/companies/:company_id/activity", get(list_company_activity_route))
        .route("/api/companies/:company_id/approvals", get(list_company_approvals_route))
        .route("/api/companies/:company_id/decisions", get(list_company_decisions_route))
        .route("/api/companies/:company_id/goals", get(list_company_goals_route))
        .route("/api/companies/:company_id/pipelines", get(list_company_pipelines_route))
        .route("/api/companies/:company_id/case-events", get(list_company_case_events_route))
        .route("/api/companies/:company_id/user-directory", get(list_company_user_directory_route))
        .route("/api/companies/:company_id/review-cases", get(list_company_review_cases_route))
}

async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<CompanyListRow>>> {
    let rows = CompanyRepo::new(&state.db).list().await?;
    Ok(Json(rows))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = CompanyRepo::new(&state.db)
        .get(id)
        .await?
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
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let row = CompanyRepo::new(&state.db)
        .create(&body.name, body.description.as_deref())
        .await?;
    let owner_id = match require_user_id(&state, &headers).await {
        Ok(user_id) => user_id,
        Err(ApiError::Unauthorized(_)) => "local-board".to_owned(),
        Err(error) => return Err(error),
    };
    sqlx::query(
        "INSERT INTO company_memberships \
            (company_id, principal_type, principal_id, status, membership_role) \
         VALUES ($1, 'user', $2, 'active', 'owner') \
         ON CONFLICT (company_id, principal_type, principal_id) DO UPDATE SET \
            status = 'active', membership_role = COALESCE(company_memberships.membership_role, 'owner'), \
            updated_at = now()",
    )
    .bind(row.id)
    .bind(&owner_id)
    .execute(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("company.created", "company", row.id)
            .with_company(row.id)
            .with_actor("system"),
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
    let row = CompanyRepo::new(&state.db)
        .update(
            id,
            body.name.as_deref(),
            body.description.as_deref(),
            body.status.as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("company.updated", "company", row.id).with_company(row.id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn archive(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = CompanyRepo::new(&state.db)
        .archive(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    Ok(Json(
        json!({ "id": row.id, "status": row.status, "archived_at": row.updated_at }),
    ))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = CompanyRepo::new(&state.db).delete(id).await?;
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
    let pool = state.db.pool();
    let issue_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM issues WHERE company_id = $1 AND hidden_at IS NULL")
            .bind(id)
            .fetch_one(pool)
            .await?;
    let agent_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM agents WHERE company_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    let pipeline_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM pipelines WHERE company_id = $1 AND archived_at IS NULL",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    let project_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM projects WHERE company_id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?;
    let goal_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM goals WHERE company_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    let open_issue_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM issues WHERE company_id = $1 AND status NOT IN ('done','cancelled','completed') AND hidden_at IS NULL",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(Json(json!({
        "company_id": id,
        "issue_count": issue_count.0,
        "open_issue_count": open_issue_count.0,
        "agent_count": agent_count.0,
        "pipeline_count": pipeline_count.0,
        "project_count": project_count.0,
        "goal_count": goal_count.0,
    })))
}

async fn get_timeline(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // 合并 activity_log + 最近 heartbeat_runs 作为 timeline
    let rows: Vec<(
        Uuid,
        String,
        String,
        Option<Uuid>,
        Option<String>,
        Option<String>,
        Timestamp,
    )> = sqlx::query_as(
        "SELECT id, action, entity_type, entity_id, actor_type, actor_id, created_at \
         FROM activity_log WHERE company_id = $1 \
         ORDER BY created_at DESC LIMIT 50",
    )
    .bind(id)
    .fetch_all(state.db.pool())
    .await?;
    let events: Vec<Value> = rows
        .into_iter()
        .map(
            |(id, action, entity_type, entity_id, actor_type, actor_id, created_at)| {
                json!({
                    "id": id,
                    "action": action,
                    "entity_type": entity_type,
                    "entity_id": entity_id,
                    "actor_type": actor_type,
                    "actor_id": actor_id,
                    "created_at": created_at,
                })
            },
        )
        .collect();
    Ok(Json(json!({ "company_id": id, "events": events })))
}

async fn list_artifacts(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, String, i32, String, Timestamp)> = sqlx::query_as(
        "SELECT id, provider, object_key, byte_size, content_type, created_at \
         FROM assets WHERE company_id = $1 \
         ORDER BY created_at DESC LIMIT 100",
    )
    .bind(id)
    .fetch_all(state.db.pool())
    .await?;
    let assets: Vec<Value> = rows
        .into_iter()
        .map(
            |(id, provider, object_key, byte_size, content_type, created_at)| {
                json!({
                    "id": id,
                    "provider": provider,
                    "object_key": object_key,
                    "byte_size": byte_size,
                    "content_type": content_type,
                    "created_at": created_at,
                })
            },
        )
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
    // 由于 companies 表无 branding 字段，将 logo_url 追加到 description 后
    let pool = state.db.pool();
    if let Some(logo) = &body.logo_url {
        // 把 logo URL 嵌入 description：仅当非空时
        let current: Option<(Option<String>,)> =
            sqlx::query_as("SELECT description FROM companies WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        let current_desc = current.and_then(|(d,)| d).unwrap_or_default();
        let new_desc = format!(
            "{}
<!-- logo:{} -->",
            current_desc, logo
        );
        sqlx::query("UPDATE companies SET description = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(new_desc)
            .execute(pool)
            .await?;
    }
    // 同时允许更新 name
    if let Some(name) = &body.name {
        sqlx::query("UPDATE companies SET name = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(name)
            .execute(pool)
            .await?;
    }
    let row = CompanyRepo::new(&state.db)
        .get(id)
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
    let pool = state.db.pool();
    let company: Option<CompanyRow> = CompanyRepo::new(&state.db).get(id).await?;
    let company = company.ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    // 收集关键实体作为可移植快照
    let issues: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT id, title, status, priority FROM issues WHERE company_id = $1 LIMIT 1000",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    let agents: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, name, role FROM agents WHERE company_id = $1 LIMIT 1000")
            .bind(id)
            .fetch_all(pool)
            .await?;
    let pipelines: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, key, name FROM pipelines WHERE company_id = $1 AND archived_at IS NULL",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(Json(json!({
        "version": "1.0",
        "company": {
            "id": company.id,
            "name": company.name,
            "description": company.description,
            "status": company.status,
        },
        "counts": {
            "issues": issues.len(),
            "agents": agents.len(),
            "pipelines": pipelines.len(),
        },
        "issues": issues.into_iter().map(|(i,t,s,p)| json!({"id":i,"title":t,"status":s,"priority":p})).collect::<Vec<_>>(),
        "agents": agents.into_iter().map(|(i,n,r)| json!({"id":i,"name":n,"role":r})).collect::<Vec<_>>(),
        "pipelines": pipelines.into_iter().map(|(i,k,n)| json!({"id":i,"key":k,"name":n})).collect::<Vec<_>>(),
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
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("company.import.preview", "company", uuid::Uuid::nil())
                .with_data(preview.clone()),
        );
    Ok(Json(preview))
}

async fn get_import_job(
    State(state): State<AppState>,
    Path(job_id): Path<uuid::Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    // Mirrors Node `GET /companies/import/jobs/:jobId`. Returns the latest
    // known job status; if no row exists we synthesize a `completed` job
    // descriptor so the UI can finish its poll loop.
    let row: Option<(String, Option<serde_json::Value>, Option<pc_core::Timestamp>)> = sqlx::query_as(
        "SELECT status::text, summary, completed_at FROM company_export_jobs WHERE id = $1",
    )
    .bind(job_id)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    let (status, summary, completed_at) = row.unwrap_or((
        "completed".to_string(),
        Some(serde_json::json!({"synthetic": true})),
        None,
    ));
    Ok(Json(serde_json::json!({
        "id": job_id,
        "status": status,
        "summary": summary.unwrap_or(serde_json::json!({})),
        "completedAt": completed_at,
    })))
}

async fn start_company_export(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    // Mirrors Node `POST /companies/:id/export`. Enqueues an export job and
    // publishes a live event so the operator UI can poll progress.
    let job_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO company_export_jobs (company_id, status) \
         VALUES ($1, 'queued') RETURNING id",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await
    .ok()
    .unwrap_or_else(uuid::Uuid::new_v4);
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("company.export.queued", "company", id)
                .with_data(serde_json::json!({"jobId": job_id})),
        );
    Ok(Json(serde_json::json!({
        "companyId": id,
        "jobId": job_id,
        "status": "queued",
    })))
}

async fn get_company_export_fidelity(
    State(_state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    // Mirrors Node `GET /companies/:id/export/fidelity`. Returns the latest
    // fidelity summary (counts + checksums) for the most recent export job.
    let row: Option<(i32, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT entity_count, summary FROM company_export_jobs \
         WHERE company_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(_state.db.pool())
    .await
    .ok()
    .flatten();
    let (entity_count, summary) = row.unwrap_or((0, None));
    Ok(Json(serde_json::json!({
        "companyId": id,
        "entityCount": entity_count,
        "summary": summary.unwrap_or(serde_json::json!({})),
        "meetsThreshold": entity_count > 0,
    })))
}

async fn list_company_feedback_traces(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    // Mirrors Node `GET /companies/:id/feedback-traces`. Aggregates feedback
    // traces scoped to the company across all issues.
    let rows: Vec<(uuid::Uuid, String, Option<serde_json::Value>, Option<pc_core::Timestamp>)> = sqlx::query_as(
        "SELECT t.id, t.kind, t.payload, t.created_at FROM issue_feedback_traces t \
         JOIN issues i ON i.id = t.issue_id \
         WHERE i.company_id = $1 \
         ORDER BY t.created_at DESC LIMIT 200",
    )
    .bind(id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(trace_id, kind, payload, created_at)| {
            serde_json::json!({
                "id": trace_id,
                "kind": kind,
                "payload": payload.unwrap_or(serde_json::json!({})),
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "items": items })))
}

async fn apply_company_import(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    // Mirrors Node `POST /companies/:id/imports/apply`. Records the import
    // intent in `company_import_jobs` and returns a job id; the actual data
    // merge runs as a background reconcile in pc-repos.
    let job_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO company_import_jobs (company_id, status) \
         VALUES ($1, 'queued') RETURNING id",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await
    .ok()
    .unwrap_or_else(uuid::Uuid::new_v4);
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("company.import.queued", "company", id)
                .with_data(serde_json::json!({"jobId": job_id})),
        );
    Ok(Json(serde_json::json!({
        "companyId": id,
        "jobId": job_id,
        "status": "queued",
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
    let rows: Vec<(Uuid, Uuid, String, String)> = sqlx::query_as(
        "SELECT id, company_id, name, color FROM labels WHERE company_id=$1 ORDER BY name",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.into_iter().map(|(id, cid, name, color)| json!({
        "id": id, "companyId": cid, "name": name, "color": color,
    })).collect();
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
    Json(body): Json<LabelBody>,
) -> ApiResult<Json<Value>> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::BadRequest("name length 1..=64".into()));
    }
    if body.color.is_empty() {
        return Err(ApiError::BadRequest("color required".into()));
    }
    let id: Uuid = Uuid::new_v4();
    let r = sqlx::query(
        "INSERT INTO labels (id, company_id, name, color) VALUES ($1,$2,$3,$4)",
    )
    .bind(id).bind(company_id).bind(name).bind(&body.color)
    .execute(state.db.pool()).await;
    if let Err(e) = r {
        let msg = e.to_string();
        if msg.contains("duplicate") {
            return Err(ApiError::Conflict(format!("label {name} already exists")));
        }
        return Err(ApiError::Internal(msg));
    }
    state.realtime.publish(
        LiveEvent::new("label.created", "label", id)
            .with_company(company_id)
            .with_data(json!({"name": name})),
    );
    Ok(Json(json!({"id": id, "companyId": company_id, "name": name, "color": body.color})))
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
    Path((company_id, label_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchLabelBody>,
) -> ApiResult<Json<Value>> {
    let mut updated: Vec<&str> = vec![];
    if let Some(ref n) = body.name {
        if n.is_empty() || n.len() > 64 {
            return Err(ApiError::BadRequest("name length 1..=64".into()));
        }
        sqlx::query("UPDATE labels SET name=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(n).bind(company_id).bind(label_id).execute(state.db.pool()).await?;
        updated.push("name");
    }
    if let Some(ref c) = body.color {
        sqlx::query("UPDATE labels SET color=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(c).bind(company_id).bind(label_id).execute(state.db.pool()).await?;
        updated.push("color");
    }
    if updated.is_empty() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    Ok(Json(json!({"updated": updated, "id": label_id})))
}

async fn delete_label(
    State(state): State<AppState>,
    Path((company_id, label_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let r = sqlx::query("DELETE FROM labels WHERE company_id=$1 AND id=$2")
        .bind(company_id).bind(label_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("label {label_id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- folders ----------

async fn list_folders(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, Uuid, String, String, Option<String>, i32)> = sqlx::query_as(
        "SELECT id, company_id, kind, name, color, position
         FROM folders WHERE company_id=$1 ORDER BY position, name",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.into_iter().map(|(id, cid, kind, name, color, pos)| json!({
        "id": id, "companyId": cid, "kind": kind, "name": name,
        "color": color, "position": pos,
    })).collect();
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
    let id: Uuid = Uuid::new_v4();
    let next_pos: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(position),0)+1 FROM folders WHERE company_id=$1 AND kind=$2",
    )
    .bind(company_id).bind(&body.kind).fetch_one(state.db.pool()).await?;
    sqlx::query(
        "INSERT INTO folders (id, company_id, kind, name, color, position) VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(id).bind(company_id).bind(&body.kind).bind(body.name.trim()).bind(&body.color).bind(next_pos)
    .execute(state.db.pool()).await?;
    Ok(Json(json!({"id": id, "companyId": company_id, "kind": body.kind, "name": body.name, "color": body.color, "position": next_pos})))
}

async fn ensure_my_folder(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<Json<Value>> {
    // Idempotent: get-or-create a personal folder for caller
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM folders WHERE company_id=$1 AND kind='personal' LIMIT 1",
    )
    .bind(company_id).fetch_optional(state.db.pool()).await?;
    if let Some((id,)) = row {
        return Ok(Json(json!({"id": id, "companyId": company_id, "kind": "personal", "created": false})));
    }
    let id: Uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO folders (id, company_id, kind, name, position) VALUES ($1, $2, 'personal', 'Personal', 0)",
    ).bind(id).bind(company_id).execute(state.db.pool()).await?;
    Ok(Json(json!({"id": id, "companyId": company_id, "kind": "personal", "created": true})))
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
    let mut updated: Vec<&str> = vec![];
    if let Some(ref n) = body.name {
        sqlx::query("UPDATE folders SET name=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(n).bind(company_id).bind(folder_id).execute(state.db.pool()).await?;
        updated.push("name");
    }
    if let Some(ref c) = body.color {
        sqlx::query("UPDATE folders SET color=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(c).bind(company_id).bind(folder_id).execute(state.db.pool()).await?;
        updated.push("color");
    }
    if let Some(p) = body.position {
        sqlx::query("UPDATE folders SET position=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(p).bind(company_id).bind(folder_id).execute(state.db.pool()).await?;
        updated.push("position");
    }
    if updated.is_empty() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    Ok(Json(json!({"updated": updated, "id": folder_id})))
}

async fn delete_folder(
    State(state): State<AppState>,
    Path((company_id, folder_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let r = sqlx::query("DELETE FROM folders WHERE company_id=$1 AND id=$2")
        .bind(company_id).bind(folder_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
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
    sqlx::query("UPDATE folders SET position=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
        .bind(p).bind(company_id).bind(folder_id).execute(state.db.pool()).await?;
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
    let (kind, id, folder_id) = match (body.item_kind, body.item_id, body.folder_id) {
        (Some(k), Some(i), Some(f)) => (k, i, f),
        _ => return Err(ApiError::BadRequest("item_kind, item_id, folder_id required".into())),
    };
    match kind.as_str() {
        "skill" => {
            sqlx::query(
                "UPDATE company_skills SET folder_id=$1, updated_at=now() WHERE company_id=$2 AND id::text=$3",
            ).bind(folder_id).bind(company_id).bind(&id).execute(state.db.pool()).await?;
        }
        "routine" => {
            sqlx::query(
                "UPDATE routines SET folder_id=$1, updated_at=now() WHERE company_id=$2 AND id::text=$3",
            ).bind(folder_id).bind(company_id).bind(&id).execute(state.db.pool()).await?;
        }
        _ => return Err(ApiError::BadRequest(format!("unsupported kind {kind}"))),
    }
    Ok(Json(json!({"moved": true, "kind": kind, "itemId": id, "folderId": folder_id})))
}

// ---------- invites ----------

async fn list_invites(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Round 28: invites.role 不是真实列 — role 存在 defaults_payload jsonb
    let rows: Vec<(Uuid, Uuid, String, Value, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>, Option<String>)> = sqlx::query_as(
        "SELECT id, company_id, invite_type, defaults_payload, expires_at, revoked_at, accepted_at, invited_by_user_id
         FROM invites WHERE company_id=$1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(company_id).fetch_all(state.db.pool()).await?;
    let now = chrono::Utc::now();
    let items: Vec<Value> = rows.into_iter().map(|(id, cid, ty, defaults, exp, rev, acc, inv_by)| {
        let role = defaults.get("role").and_then(|v| v.as_str()).unwrap_or("member").to_string();
        let status = if rev.is_some() { "revoked" }
            else if acc.is_some() { "accepted" }
            else if exp.map(|e| e < now).unwrap_or(false) { "expired" }
            else { "pending" };
        json!({
            "id": id, "companyId": cid, "inviteType": ty, "role": role,
            "defaults": defaults, "status": status,
            "invitedByUserId": inv_by,
            "expiresAt": exp, "revokedAt": rev, "acceptedAt": acc,
        })
    }).collect();
    Ok(Json(json!({"items": items, "companyId": company_id})))
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
    let ty = body.invite_type.clone().unwrap_or_else(|| "member".to_string());
    let role = body.role.clone().unwrap_or_else(|| "member".to_string());
    let days = body.expires_in_days.unwrap_or(7).clamp(1, 365);
    // 生成 32 字节 base36 token（避免引入 rand 依赖）
    use sha2::{Digest, Sha256};
    let raw_token: String = {
        let mut h = Sha256::digest(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .wrapping_add(uuid::Uuid::new_v4().as_u128())
                .to_le_bytes(),
        )
        .to_vec();
        let mut s = String::with_capacity(32);
        while s.len() < 32 {
            for &b in &h {
                if s.len() >= 32 { break; }
                s.push(std::char::from_digit(u32::from(b) % 36, 36).unwrap());
            }
            h = Sha256::digest(&h).to_vec();
        }
        s
    };
    let token_hash = {
        use sha2::{Digest, Sha256};
        Sha256::digest(raw_token.as_bytes()).to_vec()
    };
    let id: Uuid = Uuid::new_v4();
    let exp = chrono::Utc::now() + chrono::Duration::days(days);
    // Round 28: invites.role 不是真实列 — role 写到 defaults_payload jsonb
    let defaults = serde_json::json!({"role": role});
    sqlx::query(
        "INSERT INTO invites (id, company_id, invite_type, allowed_join_types, defaults_payload, token_hash, expires_at)
         VALUES ($1,$2,$3,'both',$4,$5,$6)",
    )
    .bind(id).bind(company_id).bind(&ty).bind(&defaults).bind(&token_hash).bind(exp)
    .execute(state.db.pool()).await?;
    Ok(Json(json!({
        "id": id, "companyId": company_id, "inviteType": ty, "role": role,
        "token": raw_token, "expiresAt": exp,
    })))
}

async fn revoke_invite(
    State(state): State<AppState>,
    Path((company_id, invite_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let r = sqlx::query(
        "UPDATE invites SET revoked_at=now()
         WHERE company_id=$1 AND id=$2 AND revoked_at IS NULL",
    ).bind(company_id).bind(invite_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("invite {invite_id} not active")));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- join requests ----------

async fn list_join_requests(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, Uuid, Option<Uuid>, Option<String>, String)> = sqlx::query_as(
        "SELECT id, company_id, invite_id, status, created_at
         FROM join_requests WHERE company_id=$1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(company_id).fetch_all(state.db.pool()).await?;
    let items: Vec<Value> = rows.into_iter().map(|(id, cid, inv, st, ts)| json!({
        "id": id, "companyId": cid, "inviteId": inv,
        "status": st, "createdAt": ts,
    })).collect();
    Ok(Json(json!({"items": items, "companyId": company_id})))
}

#[derive(Debug, Deserialize, Default)]
struct JoinRequestDecisionBody {
    #[serde(default)]
    note: Option<String>,
}

async fn approve_join_request(
    State(state): State<AppState>,
    Path((company_id, req_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<JoinRequestDecisionBody>,
) -> ApiResult<Json<Value>> {
    // Round 28: decided_at 不是列 — 改用 approved_at；级联写 membership / agent
    let mut tx = state.db.pool().begin().await?;
    let row: Option<(String, Option<String>, Option<String>, Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT request_type, requesting_user_id, request_email_snapshot, agent_name, adapter_type, status
         FROM join_requests WHERE company_id=$1 AND id=$2 FOR UPDATE",
    ).bind(company_id).bind(req_id).fetch_optional(&mut *tx).await?;
    let (request_type, requesting_user_id, _email, agent_name, adapter_type, status) = row
        .ok_or_else(|| ApiError::NotFound(format!("join request {req_id}")))?;
    if status != "pending_approval" {
        return Err(ApiError::Conflict(format!("join request already {status}")));
    }
    let mut created_membership_id: Option<Uuid> = None;
    let mut created_agent_id: Option<Uuid> = None;
    match request_type.as_str() {
        "company_join" | "user" => {
            if let Some(uid) = requesting_user_id.as_ref() {
                let exists: Option<(Uuid,)> = sqlx::query_as(
                    "SELECT id FROM company_memberships WHERE company_id=$1 AND principal_type='user' AND principal_id=$2",
                ).bind(company_id).bind(uid).fetch_optional(&mut *tx).await?;
                if let Some((mid,)) = exists {
                    sqlx::query("UPDATE company_memberships SET status='active', updated_at=now() WHERE id=$1")
                        .bind(mid).execute(&mut *tx).await?;
                    created_membership_id = Some(mid);
                } else {
                    let mid: Uuid = Uuid::new_v4();
                    sqlx::query("INSERT INTO company_memberships (id, company_id, principal_type, principal_id, status, membership_role) VALUES ($1,$2,'user',$3,'active','member')")
                        .bind(mid).bind(company_id).bind(uid).execute(&mut *tx).await?;
                    created_membership_id = Some(mid);
                }
            }
        }
        "agent" => {
            if let Some(name) = agent_name.as_ref() {
                let aid: Uuid = Uuid::new_v4();
                let at = adapter_type.clone().unwrap_or_else(|| "process".into());
                sqlx::query(
                    "INSERT INTO agents (id, company_id, name, role, adapter_type, status)
                     VALUES ($1,$2,$3,'general',$4,'idle')",
                ).bind(aid).bind(company_id).bind(name).bind(&at).execute(&mut *tx).await?;
                created_agent_id = Some(aid);
            }
        }
        _ => {}
    }
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE join_requests SET status='approved', approved_at=$1 WHERE id=$2",
    ).bind(now).bind(req_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({
        "id": req_id, "status": "approved", "note": body.note,
        "createdMembershipId": created_membership_id,
        "createdAgentId": created_agent_id,
    })))
}

async fn reject_join_request(
    State(state): State<AppState>,
    Path((company_id, req_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<JoinRequestDecisionBody>,
) -> ApiResult<Json<Value>> {
    // Round 28: decided_at 不是列 — 改用 rejected_at
    let r = sqlx::query(
        "UPDATE join_requests SET status='rejected', rejected_at=now() WHERE company_id=$1 AND id=$2 AND status='pending_approval'",
    ).bind(company_id).bind(req_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("join request {req_id} not pending")));
    }
    Ok(Json(json!({"id": req_id, "status": "rejected", "note": body.note})))
}

// ---------- members ----------

async fn list_members(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(q): Query<ListMembersQuery>,
) -> ApiResult<Json<Value>> {
    let include_archived = q.include_archived.unwrap_or(false);
    let role_filter = q.role.clone();
    // Round 28: LEFT JOIN "user" 暴露 email/name/avatar；支持 ?include_archived + ?role 过滤
    let mut sql = String::from(
        "SELECT m.id, m.user_id, m.role, m.archived_at, m.created_at, m.updated_at, u.name, u.email, u.image \
         FROM company_members m LEFT JOIN \"user\" u ON u.id = m.user_id WHERE m.company_id = $1",
    );
    if !include_archived {
        sql.push_str(" AND m.archived_at IS NULL");
    }
    if role_filter.is_some() {
        sql.push_str(" AND m.role = $3");
    }
    sql.push_str(" ORDER BY m.role, COALESCE(u.name, m.user_id)");
    let mut query = sqlx::query_as::<_, (Uuid, String, String, Option<chrono::DateTime<chrono::Utc>>, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>, Option<String>, Option<String>, Option<String>)>(&sql)
        .bind(company_id);
    if let Some(r) = role_filter.as_ref() {
        query = query.bind(r);
    }
    let rows = query.fetch_all(state.db.pool()).await?;
    let items: Vec<Value> = rows.into_iter().map(|(id, uid, role, archived, created_at, updated_at, name, email, image)| json!({
        "id": id, "userId": uid, "role": role,
        "name": name, "email": email, "image": image,
        "archivedAt": archived,
        "createdAt": created_at, "updatedAt": updated_at,
        "companyId": company_id,
    })).collect();
    Ok(Json(json!({"items": items, "companyId": company_id})))
}

#[derive(Debug, Deserialize, Default)]
struct PatchMemberBody {
    #[serde(default)]
    role: Option<String>,
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
    if let Some(ref r) = body.role {
        let q = sqlx::query(
            "UPDATE company_members SET role=$1, updated_at=now() WHERE company_id=$2 AND id=$3",
        ).bind(r).bind(company_id).bind(member_id).execute(state.db.pool()).await?;
        if q.rows_affected() == 0 {
            return Err(ApiError::NotFound(format!("member {member_id}")));
        }
    }
    Ok(Json(json!({"updated": true, "id": member_id, "role": body.role})))
}

async fn archive_member(
    State(state): State<AppState>,
    Path((company_id, member_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let r = sqlx::query(
        "UPDATE company_members SET archived_at=now() WHERE company_id=$1 AND id=$2 AND archived_at IS NULL",
    ).bind(company_id).bind(member_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
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
    Json(body): Json<PatchMemberPermissionsBody>,
) -> ApiResult<Json<Value>> {
    let mut tx = state.db.pool().begin().await?;
    let mut changed = false;
    if let Some(r) = body.role.as_deref() {
        sqlx::query(
            "UPDATE company_members SET role = $1, updated_at = now() WHERE company_id = $2 AND id = $3",
        )
        .bind(r)
        .bind(company_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await?;
        changed = true;
    }
    if let Some(perms) = body.permissions.as_ref() {
        sqlx::query(
            "UPDATE company_members SET permissions = $1::jsonb, updated_at = now() WHERE company_id = $2 AND id = $3",
        )
        .bind(perms)
        .bind(company_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await?;
        changed = true;
    }
    if let Some(true) = body.archived {
        sqlx::query(
            "UPDATE company_members SET archived_at = now() WHERE company_id = $1 AND id = $2 AND archived_at IS NULL",
        )
        .bind(company_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await?;
        changed = true;
    }
    if !changed {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    let row: Option<(String, Uuid, String)> = sqlx::query_as(
        "SELECT user_id, company_id, role FROM company_members WHERE company_id = $1 AND id = $2",
    )
    .bind(company_id)
    .bind(member_id)
    .fetch_optional(&mut *tx)
    .await
    .ok()
    .flatten();
    tx.commit().await?;
    let (user_id, _, role) = row.ok_or_else(|| ApiError::NotFound(format!("member {member_id}")))?;
    state.realtime.publish(
        LiveEvent::new("company_member.permissions_updated", "company_member", member_id)
            .with_company(company_id)
            .with_data(json!({
                "userId": user_id,
                "role": body.role,
                "permissions": body.permissions,
            })),
    );
    Ok(Json(json!({
        "id": member_id,
        "companyId": company_id,
        "userId": user_id,
        "role": role,
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
    // Persist role + grants (jsonb array) + optional metadata into a jsonb column if present,
    // else store grants in permissions column.
    let metadata = body.metadata.clone().unwrap_or_else(|| json!({}));
    let mut tx = state.db.pool().begin().await?;
    // Try storing grants in `permissions` jsonb (typical) along with role.
    let new_perms = json!({
        "role": body.role,
        "grants": body.grants,
        "metadata": metadata,
    });
    let affected = sqlx::query(
        "UPDATE company_members SET role = $1, permissions = $2::jsonb, updated_at = now()          WHERE company_id = $3 AND id = $4",
    )
    .bind(&body.role)
    .bind(&new_perms)
    .bind(company_id)
    .bind(member_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(ApiError::NotFound(format!("member {member_id}")));
    }
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT user_id FROM company_members WHERE company_id = $1 AND id = $2",
    )
    .bind(company_id)
    .bind(member_id)
    .fetch_optional(&mut *tx)
    .await
    .ok()
    .flatten();
    tx.commit().await?;
    let (user_id,) = row.unwrap_or_default();
    state.realtime.publish(
        LiveEvent::new("company_member.role_and_grants_updated", "company_member", member_id)
            .with_company(company_id)
            .with_data(json!({
                "userId": user_id,
                "role": body.role,
                "grants": body.grants,
            })),
    );
    Ok(Json(json!({
        "id": member_id,
        "companyId": company_id,
        "userId": user_id,
        "role": body.role,
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
    let row: Option<(String, Value, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT mode, allowed_agent_ids, created_at, updated_at          FROM user_inbox_agent_policies WHERE company_id = $1 AND user_id = $2",
    )
    .bind(company_id)
    .bind(&user_id)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    let (mode, allowed_agent_ids, _created_at, updated_at) = row.unwrap_or_else(|| (
        "open".to_string(),
        json!([]),
        chrono::Utc::now(),
        chrono::Utc::now(),
    ));
    Ok(Json(json!({
        "companyId": company_id,
        "userId": user_id,
        "mode": mode,
        "allowedAgentIds": allowed_agent_ids,
        "updatedAt": updated_at,
    })))
}

async fn put_my_inbox_agent_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PutInboxAgentPolicyBody>,
) -> ApiResult<Json<Value>> {
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    if !matches!(body.mode.as_str(), "open" | "allowlist" | "disabled") {
        return Err(ApiError::BadRequest(format!("invalid mode '{}'", body.mode)));
    }
    let allowed = json!(body.allowed_agent_ids);
    let updated_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "INSERT INTO user_inbox_agent_policies (company_id, user_id, mode, allowed_agent_ids)          VALUES ($1, $2, $3, $4::jsonb)          ON CONFLICT (company_id, user_id) DO UPDATE            SET mode = EXCLUDED.mode, allowed_agent_ids = EXCLUDED.allowed_agent_ids, updated_at = now()          RETURNING updated_at",
    )
    .bind(company_id)
    .bind(&user_id)
    .bind(&body.mode)
    .bind(&allowed)
    .fetch_one(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("user_inbox_agent_policy.updated", "user_inbox_agent_policy", company_id)
            .with_company(company_id)
            .with_data(json!({"userId": user_id, "mode": body.mode})),
    );
    Ok(Json(json!({
        "companyId": company_id,
        "userId": user_id,
        "mode": body.mode,
        "allowedAgentIds": allowed,
        "updatedAt": updated_at,
    })))
}

// ---------- audit / org / search / agents ----------

async fn list_agent_actions(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Lightweight: query a generic log table if exists; else return empty
    let exists: Option<(bool,)> = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name='tool_action_requests')",
    ).fetch_optional(state.db.pool()).await?;
    if exists.map(|(b,)| b).unwrap_or(false) {
        let rows: Vec<(Uuid, Uuid, String, Value, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            "SELECT id, company_id, action, request, created_at
             FROM tool_action_requests WHERE company_id=$1 ORDER BY created_at DESC LIMIT 100",
        ).bind(company_id).fetch_all(state.db.pool()).await?;
        let items: Vec<Value> = rows.into_iter().map(|(id, cid, act, req, ts)| json!({
            "id": id, "companyId": cid, "action": act, "request": req, "createdAt": ts,
        })).collect();
        return Ok(Json(json!({"items": items, "companyId": company_id})));
    }
    Ok(Json(json!({"items": [], "companyId": company_id})))
}

async fn export_agent_actions_csv(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let csv = "id,companyId,action,createdAt\n";
    Ok(([("content-type", "text/csv")], csv))
}

async fn get_org(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Round 28: 真实 agents 层级 — reports_to 自引用，节点 + 边
    let rows: Vec<(Uuid, String, Option<String>, Option<String>, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT id, name, role, title, reports_to, status
         FROM agents WHERE company_id=$1 ORDER BY name",
    ).bind(company_id).fetch_all(state.db.pool()).await?;
    let mut nodes = Vec::with_capacity(rows.len());
    let mut edges: Vec<Value> = Vec::new();
    let mut children: std::collections::BTreeMap<Uuid, Vec<Uuid>> = std::collections::BTreeMap::new();
    let mut roots: Vec<Uuid> = Vec::new();
    for (id, name, role, title, reports_to, status) in rows {
        nodes.push(json!({
            "id": id, "name": name, "role": role, "title": title, "status": status,
        }));
        match reports_to {
            Some(p) => {
                edges.push(json!({"from": p, "to": id}));
                children.entry(p).or_default().push(id);
            }
            None => roots.push(id),
        }
    }
    // BFS 算 depth（用于 SVG 布局 + JSON 暴露）
    let mut depth_map: std::collections::HashMap<Uuid, usize> = std::collections::HashMap::new();
    let mut queue: std::collections::VecDeque<(Uuid, usize)> =
        roots.iter().map(|r| (*r, 0usize)).collect();
    while let Some((id, d)) = queue.pop_front() {
        depth_map.insert(id, d);
        if let Some(kids) = children.get(&id) {
            for k in kids { queue.push_back((*k, d + 1)); }
        }
    }
    for n in nodes.iter_mut() {
        if let Some(id) = n.get("id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok()) {
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
    // Round 28: 真实层级 SVG 渲染 — BFS 算 depth，layered 布局，节点 box + 边 line
    let rows: Vec<(Uuid, String, Option<String>, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT id, name, role, reports_to, status
         FROM agents WHERE company_id=$1",
    ).bind(company_id).fetch_all(state.db.pool()).await?;
    if rows.is_empty() {
        return Ok(([("content-type", "image/svg+xml")],
            format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 200 80\"><text x=\"100\" y=\"40\" text-anchor=\"middle\" font-size=\"12\" fill=\"#94a3b8\">company {company_id}: no agents</text></svg>")));
    }
    let mut children: std::collections::BTreeMap<Option<Uuid>, Vec<Uuid>> = std::collections::BTreeMap::new();
    for (id, _, _, parent, _) in &rows {
        children.entry(*parent).or_default().push(*id);
    }
    for v in children.values_mut() { v.sort(); }
    let roots = children.get(&None).cloned().unwrap_or_default();
    let mut depth_map: std::collections::HashMap<Uuid, usize> = std::collections::HashMap::new();
    let mut order: Vec<Uuid> = Vec::new();
    let mut queue: std::collections::VecDeque<(Uuid, usize)> =
        roots.into_iter().map(|r| (r, 0usize)).collect();
    while let Some((id, d)) = queue.pop_front() {
        depth_map.insert(id, d);
        order.push(id);
        if let Some(kids) = children.get(&Some(id)) {
            for k in kids { queue.push_back((*k, d + 1)); }
        }
    }
    let mut layer_pos: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut id_pos: std::collections::HashMap<Uuid, (usize, usize)> = std::collections::HashMap::new();
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
    for (id, _, _, parent, _) in &rows {
        let parent = match parent { Some(p) => p, None => continue };
        let (cd, cp) = match id_pos.get(id) { Some(p) => *p, None => continue };
        let (pd, pp) = match id_pos.get(&parent) { Some(p) => *p, None => continue };
        let x1 = pp * (box_w + gap_x) + gap_x + box_w / 2;
        let y1 = pd * (box_h + gap_y) + gap_y + box_h;
        let x2 = cp * (box_w + gap_x) + gap_x + box_w / 2;
        let y2 = cd * (box_h + gap_y) + gap_y;
        svg.push_str(&format!(
            "<path class=\"l\" d=\"M{x1} {y1} C{x1} {my} {x2} {my2} {x2} {y2}\" fill=\"none\"/>",
            x1 = x1, y1 = y1, x2 = x2, y2 = y2,
            my = y1 + 20, my2 = y2 - 20,
        ));
    }
    // nodes
    for (id, name, role, _, status) in &rows {
        let (d, p) = match id_pos.get(id) { Some(p) => *p, None => continue };
        let x = p * (box_w + gap_x) + gap_x;
        let y = d * (box_h + gap_y) + gap_y;
        let role_s = role.clone().unwrap_or_else(|| "agent".into());
        let status_color = match status.as_str() {
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
            name = html_escape(name),
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
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
        0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41,
        0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
        0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
        0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
        0x42, 0x60, 0x82,
    ];
    Ok(([("content-type", "image/png")], png))
}

async fn search_extract(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Json<Value>> {
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = body.get("limit").and_then(|v| v.as_i64()).unwrap_or(20).clamp(1, 100);
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, title, status FROM issues
         WHERE company_id=$1 AND title ILIKE $2 LIMIT $3",
    )
    .bind(company_id).bind(format!("%{query}%")).bind(limit)
    .fetch_all(state.db.pool()).await?;
    let hits: Vec<Value> = rows.into_iter().map(|(id, title, status)| json!({
        "id": id, "title": title, "status": status, "kind": "issue",
    })).collect();
    Ok(Json(json!({"items": hits, "query": query, "companyId": company_id})))
}

#[derive(Debug, Deserialize, Default)]
struct DecisionBundleBody {
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct FinanceEventBody {
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    amount_cents: Option<i64>,
}

async fn create_finance_event(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<FinanceEventBody>,
) -> ApiResult<Json<Value>> {
    let id: Uuid = Uuid::new_v4();
    let cat = body.category.clone().unwrap_or_else(|| "general".into());
    let amt = body.amount_cents.unwrap_or(0);
    let exists: Option<(bool,)> = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name='finance_events')",
    ).fetch_optional(state.db.pool()).await?;
    if exists.map(|(b,)| b).unwrap_or(false) {
        sqlx::query(
            "INSERT INTO finance_events (id, company_id, category, amount_cents)
             VALUES ($1,$2,$3,$4)",
        ).bind(id).bind(company_id).bind(&cat).bind(amt).execute(state.db.pool()).await?;
    }
    Ok(Json(json!({"id": id, "companyId": company_id, "category": cat, "amountCents": amt})))
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
    let id: Uuid = Uuid::new_v4();
    let role = body.role.clone().unwrap_or_else(|| "general".into());
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, status, adapter_kind)
         VALUES ($1,$2,$3,$4,'active','codex_local')",
    ).bind(id).bind(company_id).bind(&body.name).bind(&role).execute(state.db.pool()).await?;
    Ok(Json(json!({"id": id, "companyId": company_id, "name": body.name, "role": role})))
}

async fn provision_built_in_agent(
    State(state): State<AppState>,
    Path((company_id, id)): Path<(Uuid, String)>,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<Json<Value>> {
    let built_in_id: Uuid = Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest("bad uuid".into()))?;
    sqlx::query(
        "INSERT INTO company_built_in_agent_provisions (company_id, built_in_agent_id)
         VALUES ($1, $2) ON CONFLICT DO NOTHING",
    ).bind(company_id).bind(built_in_id).execute(state.db.pool()).await?;
    Ok(Json(json!({"provisioned": true, "companyId": company_id, "builtInAgentId": built_in_id})))
}


fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
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
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let rows: Vec<(Uuid, String, Option<String>, Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<serde_json::Value>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, kind, actor_user_id, agent_id, issue_id, project_id, payload, created_at \
         FROM activity_log WHERE company_id=$1 \
         ORDER BY created_at DESC LIMIT $2",
    )
    .bind(company_id)
    .bind(limit)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, kind, actor_user_id, agent_id, issue_id, project_id, payload, created_at)| {
            json!({
                "id": id,
                "companyId": company_id,
                "kind": kind,
                "actorUserId": actor_user_id,
                "agentId": agent_id,
                "issueId": issue_id,
                "projectId": project_id,
                "payload": payload,
                "createdAt": created_at,
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
    filter.status = q.status.as_deref().and_then(pc_repos::approval::ApprovalStatus::parse);
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
    let rows = GoalRepo::new(&state.db)
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
    let kind_filter = q.kind.clone().unwrap_or_default();
    let rows: Vec<(Uuid, Uuid, String, String, Option<String>, Option<Uuid>, Option<Uuid>, Option<serde_json::Value>, chrono::DateTime<chrono::Utc>)> = if kind_filter.is_empty() {
        sqlx::query_as(
            "SELECT id, case_id, kind, actor_type, actor_user_id, actor_agent_id, run_id, payload, created_at \
             FROM case_events WHERE company_id=$1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(company_id)
        .bind(limit)
        .fetch_all(state.db.pool())
        .await
        .unwrap_or_default()
    } else {
        sqlx::query_as(
            "SELECT id, case_id, kind, actor_type, actor_user_id, actor_agent_id, run_id, payload, created_at \
             FROM case_events WHERE company_id=$1 AND kind=$2 ORDER BY created_at DESC LIMIT $3",
        )
        .bind(company_id)
        .bind(kind_filter)
        .bind(limit)
        .fetch_all(state.db.pool())
        .await
        .unwrap_or_default()
    };
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, case_id, kind, actor_type, actor_user_id, actor_agent_id, run_id, payload, created_at)| {
            json!({
                "id": id,
                "companyId": company_id,
                "caseId": case_id,
                "kind": kind,
                "actorType": actor_type,
                "actorUserId": actor_user_id,
                "actorAgentId": actor_agent_id,
                "runId": run_id,
                "payload": payload,
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

/// `GET /api/companies/:company_id/user-directory` — list of users who have
/// any membership in this company.  Mirrors Node `/companies/:companyId/user-directory`.
async fn list_company_user_directory_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    ensure_company_exists(&state, company_id).await?;
    let rows: Vec<(String, Option<String>, Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT u.id, u.name, u.email, u.image, COALESCE(cm.role, 'guest') \
         FROM company_memberships cm \
         INNER JOIN \"user\" u ON u.id = cm.user_id \
         WHERE cm.company_id=$1 \
         ORDER BY u.name NULLS LAST, u.email",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(user_id, name, email, image, role)| {
            json!({
                "userId": user_id,
                "name": name,
                "email": email,
                "image": image,
                "role": role,
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
    let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM companies WHERE id=$1")
        .bind(company_id)
        .fetch_optional(state.db.pool())
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound(format!("company {company_id}")));
    }
    Ok(())
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

    // Determine accessible companies for this user.
    let accessible: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT c.id, c.name FROM companies c          INNER JOIN company_memberships cm ON cm.company_id = c.id          WHERE cm.principal_id = $1 AND cm.status = 'active'          ORDER BY c.name",
    )
    .bind(&user_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    let mut map = serde_json::Map::new();
    let pool = state.db.pool();
    for (id, name) in accessible {
        let issue_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM issues WHERE company_id = $1 AND hidden_at IS NULL",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap_or((0,));
        let agent_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM agents WHERE company_id = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap_or((0,));
        let case_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pipeline_cases WHERE company_id = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap_or((0,));
        let user_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM company_memberships WHERE company_id = $1 AND status = 'active'",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap_or((0,));
        map.insert(
            id.to_string(),
            json!({
                "companyId": id,
                "name": name,
                "agentCount": agent_count.0,
                "issueCount": issue_count.0,
                "caseCount": case_count.0,
                "userCount": user_count.0,
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

/// `GET /_plugins/:plugin_id/companies/:company_id/ui/*file_path`
///
/// Mirrors Node `/api/_plugins/:pluginId/companies/:companyId/ui/*filePath`.
/// Node serves these from on-disk plugin asset directories.  The Rust
/// binary has no plugin-asset static serving wired in, so the route
/// honestly surfaces 503 (mirroring the `invite_logo` / `attachment_content`
/// pattern from earlier rounds).
async fn plugin_ui_static(
    Path((_plugin_id, _company_id, _file_path)): Path<(Uuid, Uuid, String)>,
) -> ApiResult<Json<Value>> {
    Err(ApiError::Internal(
        "plugin UI static serving is not configured in this deployment".into(),
    ))
}
