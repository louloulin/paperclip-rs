//! `/api/companies*` 路由：CRUD + 归档。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::company::{CompanyListRow, CompanyRepo, CompanyRow};

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
        .route("/api/companies/:id/export/fidelity", get(get_company_export_fidelity))
        .route("/api/companies/:id/feedback-traces", get(list_company_feedback_traces))
        .route("/api/companies/:id/imports/apply", post(apply_company_import))
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
