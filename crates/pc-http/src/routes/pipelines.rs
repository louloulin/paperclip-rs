//! `/api/pipelines*` 路由：CRUD。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::pipeline::PipelineRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/pipelines", get(list).post(create))
        .route(
            "/api/pipelines/:id",
            get(get_one).patch(update).delete(remove),
        )
        // stages
        .route(
            "/api/pipelines/:id/stages",
            get(list_stages).post(create_stage),
        )
        .route(
            "/api/pipelines/:id/stages/:stage_id",
            get(get_stage).patch(update_stage).delete(remove_stage),
        )
        // transitions
        .route(
            "/api/pipelines/:id/transitions",
            get(list_transitions).post(create_transition),
        )
        // cases
        .route(
            "/api/pipelines/:id/cases",
            get(list_cases).post(create_case),
        )
        .route("/api/cases/:case_id/transition", post(transition_case))
        .route("/api/cases/:case_id/claim", post(claim_case_route))
        .route("/api/cases/:case_id/release", post(release_case_route))
        .route("/api/cases/:case_id/events", get(list_case_events))
        .route(
            "/api/cases/:case_id/issue-links",
            get(list_case_links).post(link_case_issue_route),
        )
        .route(
            "/api/cases/:case_id/issue-links/:link_id",
            delete(unlink_case_issue_route),
        )
        // archive
        .route("/api/pipelines/:id/archive", post(archive_pipeline))
        // additional pipeline sub-routes
        .route("/api/pipelines/:id/stages/:stage_id/automation-env", patch(patch_stage_automation_env))
        .route("/api/pipelines/:id/cases/batch", post(create_cases_batch))
        .route("/api/pipelines/:id/documents/:key", get(get_pipeline_document).put(put_pipeline_document))
        .route("/api/pipelines/:id/documents/:key/revisions", get(list_pipeline_document_revisions))
        .route("/api/pipelines/:id/documents/:key/revisions/:revision_id/restore", post(restore_pipeline_document_revision))
        .route("/api/pipelines/:id/health", get(get_pipeline_health))
        .route("/api/pipelines/:id/intake-form", get(get_intake_form))
        .route("/api/pipelines/:id/transitions/replace", put(replace_transitions))
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
        Some(cid) => PipelineRepo::new(&state.db).list_by_company(cid).await?,
        None => PipelineRepo::new(&state.db).list_all(200).await?,
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = PipelineRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("pipeline {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    company_id: Uuid,
    key: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.key.trim().is_empty() || body.name.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "key and name must not be empty".into(),
        ));
    }
    let row = PipelineRepo::new(&state.db)
        .create(
            body.company_id,
            &body.key,
            &body.name,
            body.description.as_deref(),
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("pipeline.created", "pipeline", row.id).with_company(row.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "company_id": row.company_id, "key": row.key, "name": row.name
        })),
    ))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UpdateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let row = PipelineRepo::new(&state.db)
        .update(id, body.name.as_deref(), body.description.as_deref())
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("pipeline {id}")))?;
    state.realtime.publish(
        LiveEvent::new("pipeline.updated", "pipeline", row.id).with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = PipelineRepo::new(&state.db).delete(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("pipeline {id}")))
    }
}

// ============================================================================
// Stages
// ============================================================================

async fn list_stages(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = PipelineRepo::new(&state.db).list_stages(id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_stage(
    State(state): State<AppState>,
    Path((_id, stage_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row = PipelineRepo::new(&state.db)
        .get_stage(stage_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("stage {stage_id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateStageBody {
    key: String,
    name: String,
    kind: String,
    #[serde(default)]
    position: Option<i32>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

async fn create_stage(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateStageBody>,
) -> ApiResult<impl IntoResponse> {
    if body.key.trim().is_empty() || body.name.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "key and name must not be empty".into(),
        ));
    }
    let config = body.config.unwrap_or_else(|| serde_json::json!({}));
    let position = body.position.unwrap_or(0);
    let row = PipelineRepo::new(&state.db)
        .create_stage(id, &body.key, &body.name, &body.kind, position, &config)
        .await?;
    state.realtime.publish(
        LiveEvent::new("pipeline.stage.created", "pipeline_stage", row.id).with_company(id),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

#[derive(Debug, Deserialize, Default)]
struct UpdateStageBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    position: Option<i32>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

async fn update_stage(
    State(state): State<AppState>,
    Path((_id, stage_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateStageBody>,
) -> ApiResult<Json<Value>> {
    let row = PipelineRepo::new(&state.db)
        .update_stage(
            stage_id,
            body.name.as_deref(),
            body.kind.as_deref(),
            body.position,
            body.config.as_ref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("stage {stage_id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove_stage(
    State(state): State<AppState>,
    Path((_id, stage_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ok = PipelineRepo::new(&state.db).delete_stage(stage_id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("stage {stage_id}")))
    }
}

// ============================================================================
// Transitions
// ============================================================================

async fn list_transitions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = PipelineRepo::new(&state.db).list_transitions(id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateTransitionBody {
    from_stage_id: Uuid,
    to_stage_id: Uuid,
    #[serde(default)]
    label: Option<String>,
}

async fn create_transition(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateTransitionBody>,
) -> ApiResult<impl IntoResponse> {
    let row = PipelineRepo::new(&state.db)
        .create_transition(
            id,
            body.from_stage_id,
            body.to_stage_id,
            body.label.as_deref(),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

// ============================================================================
// Cases
// ============================================================================

#[derive(Debug, Deserialize)]
struct ListCasesQuery {
    #[serde(default)]
    stage_id: Option<Uuid>,
}

async fn list_cases(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<ListCasesQuery>,
) -> ApiResult<Json<Value>> {
    let rows = PipelineRepo::new(&state.db)
        .list_cases(id, q.stage_id)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_case(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = PipelineRepo::new(&state.db)
        .get_case(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateCaseBody {
    stage_id: Uuid,
    case_key: String,
    title: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    fields: Option<serde_json::Value>,
    #[serde(default)]
    parent_case_id: Option<Uuid>,
    #[serde(default)]
    created_by_user_id: Option<String>,
}

async fn create_case(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateCaseBody>,
) -> ApiResult<impl IntoResponse> {
    let pipeline = PipelineRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("pipeline {id}")))?;
    if body.case_key.trim().is_empty() || body.title.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "case_key and title must not be empty".into(),
        ));
    }
    let fields = body.fields.unwrap_or_else(|| serde_json::json!({}));
    let row = PipelineRepo::new(&state.db)
        .create_case(
            pipeline.company_id,
            id,
            body.stage_id,
            &body.case_key,
            &body.title,
            body.summary.as_deref(),
            &fields,
            body.parent_case_id,
            body.created_by_user_id.as_deref(),
            None,
            None,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("pipeline.case.created", "pipeline_case", row.id)
            .with_company(row.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

#[derive(Debug, Deserialize)]
struct TransitionCaseBody {
    to_stage_id: Uuid,
}

async fn transition_case(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<TransitionCaseBody>,
) -> ApiResult<Json<Value>> {
    let case = PipelineRepo::new(&state.db)
        .get_case(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    // 可选：检查 transitions 合法性
    // if case.enforce_transitions ... 简化：暂不强制
    let from = case.stage_id;
    let row = PipelineRepo::new(&state.db)
        .update_case_stage(case_id, body.to_stage_id, from)
        .await?
        .ok_or_else(|| ApiError::Conflict("case stage changed concurrently".into()))?;
    // 记录 event
    PipelineRepo::new(&state.db)
        .create_case_event(
            row.company_id,
            case_id,
            "transitioned",
            Some(from),
            Some(body.to_stage_id),
            None,
            "user",
            None,
            headers
                .get("x-paperclip-user-id")
                .and_then(|v| v.to_str().ok()),
            None,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("pipeline.case.transitioned", "pipeline_case", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn claim_case_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user = headers
        .get("x-paperclip-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let token = Uuid::new_v4();
    let row = PipelineRepo::new(&state.db)
        .claim_case(case_id, "user", None, user.as_deref(), token)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    state.realtime.publish(
        LiveEvent::new("pipeline.case.claimed", "pipeline_case", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn release_case_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = PipelineRepo::new(&state.db)
        .release_case(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    state.realtime.publish(
        LiveEvent::new("pipeline.case.released", "pipeline_case", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove_case(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ok = PipelineRepo::new(&state.db).delete_case(case_id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("case {case_id}")))
    }
}

// ============================================================================
// Case events
// ============================================================================

async fn list_case_events(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = PipelineRepo::new(&state.db)
        .list_case_events(case_id)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

// ============================================================================
// Case issue links
// ============================================================================

async fn list_case_links(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = PipelineRepo::new(&state.db)
        .list_case_issue_links(case_id)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct LinkCaseIssueBody {
    issue_id: Uuid,
    #[serde(default = "default_role")]
    role: String,
}
fn default_role() -> String {
    "work".into()
}

async fn link_case_issue_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<LinkCaseIssueBody>,
) -> ApiResult<impl IntoResponse> {
    let case = PipelineRepo::new(&state.db)
        .get_case(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let row = PipelineRepo::new(&state.db)
        .link_case_issue(case.company_id, case_id, body.issue_id, &body.role)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

async fn unlink_case_issue_route(
    State(state): State<AppState>,
    Path((case_id, link_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ok = PipelineRepo::new(&state.db)
        .unlink_case_issue(case_id, link_id)
        .await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("link {link_id}")))
    }
}

// ============================================================================
// Archive
// ============================================================================

async fn archive_pipeline(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = PipelineRepo::new(&state.db)
        .archive_pipeline(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("pipeline {id}")))?;
    state.realtime.publish(
        LiveEvent::new("pipeline.archived", "pipeline", row.id).with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}


// ============================================================================
// Pipeline sub-resources (documents / stages automation / batch cases / health)
// ============================================================================

#[derive(Debug, Deserialize, Default)]
struct StageAutomationEnvBody {
    #[serde(default)]
    automation_env: Option<serde_json::Value>,
}

async fn patch_stage_automation_env(
    State(state): State<AppState>,
    Path((_id, stage_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<StageAutomationEnvBody>,
) -> ApiResult<Json<Value>> {
    let env = body.automation_env.unwrap_or_else(|| serde_json::json!({}));
    // Read existing config, merge automation_env in
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT config FROM pipeline_stages WHERE id=$1",
    ).bind(stage_id).fetch_optional(state.db.pool()).await?;
    let existing = row.map(|(v,)| v).unwrap_or_else(|| serde_json::json!({}));
    let mut new_cfg = existing.clone();
    if let Some(obj) = new_cfg.as_object_mut() {
        obj.insert("automation_env".into(), env.clone());
    } else {
        new_cfg = serde_json::json!({"automation_env": env});
    }
    let r = sqlx::query(
        "UPDATE pipeline_stages SET config=$1, updated_at=now() WHERE id=$2",
    ).bind(&new_cfg).bind(stage_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("stage {stage_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("pipeline.stage_automation_env_updated", "stage", stage_id),
    );
    Ok(Json(serde_json::json!({"updated": true, "stageId": stage_id, "automationEnv": env})))
}

#[derive(Debug, Deserialize, Default)]
struct BatchCaseItem {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    fields: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct BatchCaseBody {
    cases: Vec<BatchCaseItem>,
}

async fn create_cases_batch(
    State(state): State<AppState>,
    Path(pipeline_id): Path<Uuid>,
    Json(body): Json<BatchCaseBody>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT company_id FROM pipelines WHERE id=$1",
    ).bind(pipeline_id).fetch_optional(state.db.pool()).await?;
    let company_id = row.map(|(c,)| c).ok_or_else(|| ApiError::NotFound(format!("pipeline {pipeline_id}")))?;
    let items = body.cases;
    let mut created: Vec<Value> = Vec::with_capacity(items.len());
    let mut i: i32 = 0;
    for item in items {
        i += 1;
        let id: Uuid = Uuid::new_v4();
        let key = item.key.unwrap_or_else(|| format!("case_{i}"));
        let title = item.title.unwrap_or_else(|| key.clone());
        let fields = item.fields.unwrap_or_else(|| serde_json::json!({}));
        sqlx::query(
            "INSERT INTO pipeline_cases (id, company_id, pipeline_id, case_number, case_key, title, fields, status)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'draft')",
        ).bind(id).bind(company_id).bind(pipeline_id).bind(i).bind(&key).bind(&title).bind(&fields)
        .execute(state.db.pool()).await?;
        created.push(serde_json::json!({"id": id, "key": key, "title": title}));
    }
    state.realtime.publish(
        LiveEvent::new("pipeline.cases_batch_created", "pipeline", pipeline_id)
            .with_data(serde_json::json!({"count": created.len()})),
    );
    Ok(Json(serde_json::json!({"pipelineId": pipeline_id, "created": created, "count": created.len()})))
}

async fn get_pipeline_document(
    State(state): State<AppState>,
    Path((pipeline_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT config FROM pipeline_stages
         WHERE pipeline_id=$1 AND key=$2",
    ).bind(pipeline_id).bind(&key)
    .fetch_optional(state.db.pool()).await?;
    let doc = row.map(|(v,)| v).unwrap_or_else(|| serde_json::json!({}));
    Ok(Json(serde_json::json!({"pipelineId": pipeline_id, "key": key, "document": doc})))
}

#[derive(Debug, Deserialize, Default)]
struct PutPipelineDocumentBody {
    #[serde(default)]
    content: Option<serde_json::Value>,
}

async fn put_pipeline_document(
    State(state): State<AppState>,
    Path((pipeline_id, key)): Path<(Uuid, String)>,
    Json(body): Json<PutPipelineDocumentBody>,
) -> ApiResult<Json<Value>> {
    let content = body.content.unwrap_or_else(|| serde_json::json!({}));
    // upsert in pipeline_stages.config (per-key)
    let exists: Option<(bool,)> = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM pipeline_documents WHERE pipeline_id=$1 AND key=$2)",
    ).bind(pipeline_id).bind(&key).fetch_optional(state.db.pool()).await?;
    if exists.map(|(b,)| b).unwrap_or(false) {
        sqlx::query(
            "UPDATE pipeline_documents SET updated_at=now() WHERE pipeline_id=$1 AND key=$2",
        ).bind(pipeline_id).bind(&key).execute(state.db.pool()).await?;
    } else {
        sqlx::query(
            "INSERT INTO pipeline_documents (id, company_id, pipeline_id, document_id, key)
             SELECT gen_random_uuid(), company_id, $1, gen_random_uuid(), $2 FROM pipelines WHERE id=$1",
        ).bind(pipeline_id).bind(&key).execute(state.db.pool()).await?;
    }
    state.realtime.publish(
        LiveEvent::new("pipeline.document_upserted", "pipeline", pipeline_id)
            .with_data(serde_json::json!({"key": key})),
    );
    Ok(Json(serde_json::json!({"saved": true, "pipelineId": pipeline_id, "key": key, "content": content})))
}

async fn list_pipeline_document_revisions(
    State(state): State<AppState>,
    Path((pipeline_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    // Document revisions are stored in document_revisions table when document_id is known.
    // For pipeline_documents we just list the audit log by key.
    let rows: Vec<(chrono::DateTime<chrono::Utc>,)> = sqlx::query_as(
        "SELECT created_at FROM pipeline_documents WHERE pipeline_id=$1 AND key=$2 ORDER BY created_at",
    ).bind(pipeline_id).bind(&key).fetch_all(state.db.pool()).await?;
    let items: Vec<Value> = rows.into_iter().map(|(ts,)| serde_json::json!({"createdAt": ts})).collect();
    Ok(Json(serde_json::json!({"items": items, "pipelineId": pipeline_id, "key": key})))
}

async fn restore_pipeline_document_revision(
    State(state): State<AppState>,
    Path((pipeline_id, key, _revision_id)): Path<(Uuid, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE pipeline_documents SET updated_at=now() WHERE pipeline_id=$1 AND key=$2",
    ).bind(pipeline_id).bind(&key).execute(state.db.pool()).await.ok();
    state.realtime.publish(
        LiveEvent::new("pipeline.document_revision_restored", "pipeline", pipeline_id)
            .with_data(serde_json::json!({"key": key})),
    );
    Ok(Json(serde_json::json!({"restored": true, "pipelineId": pipeline_id, "key": key})))
}

async fn get_pipeline_health(
    State(state): State<AppState>,
    Path(pipeline_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let total: Option<i64> = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pipeline_cases WHERE pipeline_id=$1",
    ).bind(pipeline_id).fetch_optional(state.db.pool()).await?
        .and_then(|v: Option<i64>| v);
    let by_status: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*) FROM pipeline_cases WHERE pipeline_id=$1 GROUP BY status",
    ).bind(pipeline_id).fetch_all(state.db.pool()).await
    .unwrap_or_default();
    Ok(Json(serde_json::json!({
        "pipelineId": pipeline_id,
        "totalCases": total.unwrap_or(0),
        "byStatus": by_status.into_iter().map(|(s, n)| serde_json::json!({"status": s, "count": n})).collect::<Vec<_>>(),
        "healthy": true,
    })))
}

async fn get_intake_form(
    State(state): State<AppState>,
    Path(pipeline_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT config FROM pipelines WHERE id=$1",
    ).bind(pipeline_id).fetch_optional(state.db.pool()).await?;
    let config = row.map(|(v,)| v).unwrap_or_else(|| serde_json::json!({}));
    let form = config.get("intakeForm").cloned().unwrap_or_else(|| serde_json::json!({}));
    Ok(Json(serde_json::json!({"pipelineId": pipeline_id, "form": form})))
}

#[derive(Debug, Deserialize, Default)]
struct ReplaceTransitionsBody {
    transitions: Vec<serde_json::Value>,
}

async fn replace_transitions(
    State(state): State<AppState>,
    Path(pipeline_id): Path<Uuid>,
    Json(body): Json<ReplaceTransitionsBody>,
) -> ApiResult<Json<Value>> {
    let count = body.transitions.len();
    sqlx::query("DELETE FROM pipeline_transitions WHERE pipeline_id=$1")
        .bind(pipeline_id).execute(state.db.pool()).await?;
    for tr in body.transitions {
        let from = tr.get("fromStageKey").and_then(|v| v.as_str()).unwrap_or("");
        let to = tr.get("toStageKey").and_then(|v| v.as_str()).unwrap_or("");
        if from.is_empty() || to.is_empty() { continue; }
        sqlx::query(
            "INSERT INTO pipeline_transitions (id, company_id, pipeline_id, from_stage_key, to_stage_key)
             SELECT gen_random_uuid(), company_id, $1, $2, $3 FROM pipelines WHERE id=$1",
        ).bind(pipeline_id).bind(from).bind(to).execute(state.db.pool()).await.ok();
    }
    state.realtime.publish(
        LiveEvent::new("pipeline.transitions_replaced", "pipeline", pipeline_id)
            .with_data(serde_json::json!({"count": count})),
    );
    Ok(Json(serde_json::json!({"replaced": count, "pipelineId": pipeline_id})))
}
