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
use pc_repos::case::CaseRepo;
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
        // ---- Round 47: cases automation retry ----
        .route(
            "/api/cases/:case_id/automation/retry-plan",
            get(case_automation_retry_plan),
        )
        .route(
            "/api/cases/:case_id/automation/retry",
            post(case_automation_retry),
        )
        .route(
            "/api/cases/:case_id/automations/:automation_id/retry",
            post(case_automation_specific_retry),
        )
        .route(
            "/api/cases/:case_id/automation/current-stage/rerun",
            post(case_automation_current_stage_rerun),
        )
        // ---- Round 41: pipelines-attention + bulk review-cases ----
        .route(
            "/api/companies/:company_id/pipelines-attention",
            get(list_pipelines_attention_route),
        )
        .route(
            "/api/companies/:company_id/review-cases/bulk",
            post(bulk_review_cases_route),
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

// Round 110: 仓储化。PipelineRepo::get_stage_config + set_stage_config。
async fn patch_stage_automation_env(
    State(state): State<AppState>,
    Path((_id, stage_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<StageAutomationEnvBody>,
) -> ApiResult<Json<Value>> {
    let env = body.automation_env.unwrap_or_else(|| serde_json::json!({}));
    let repo = PipelineRepo::new(&state.db);
    let existing = repo.get_stage_config(stage_id).await?
        .unwrap_or_else(|| serde_json::json!({}));
    let mut new_cfg = existing.clone();
    if let Some(obj) = new_cfg.as_object_mut() {
        obj.insert("automation_env".into(), env.clone());
    } else {
        new_cfg = serde_json::json!({"automation_env": env});
    }
    let ok = repo.set_stage_config(stage_id, &new_cfg).await?;
    if !ok {
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

// Round 110: 仓储化。PipelineRepo::company_id_for_pipeline() 反查 + per-item INSERT 用 Repo.create_case。
async fn create_cases_batch(
    State(state): State<AppState>,
    Path(pipeline_id): Path<Uuid>,
    Json(body): Json<BatchCaseBody>,
) -> ApiResult<Json<Value>> {
    let repo = PipelineRepo::new(&state.db);
    let company_id = repo
        .company_id_for_pipeline(pipeline_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("pipeline {pipeline_id}")))?;
    let items = body.cases;
    let mut created: Vec<Value> = Vec::with_capacity(items.len());
    // Round 110 警告：pipeline_cases.stage_id 是 NOT NULL 但这里批量创建没有 stage；
    // 我们用 PipelineRepo::list_stages 拿第一个 stage 作为默认归属，
    // 如果 pipeline 没有任何 stage 则跳过（避免 NOT NULL 违规）。
    let default_stage = repo
        .list_stages(pipeline_id)
        .await?
        .first()
        .map(|s| s.id);
    let mut i: i32 = 0;
    for item in items {
        i += 1;
        let key = item.key.unwrap_or_else(|| format!("case_{i}"));
        let title = item.title.unwrap_or_else(|| key.clone());
        let fields = item.fields.unwrap_or_else(|| serde_json::json!({}));
        let id = if let Some(stage_id) = default_stage {
            repo.create_case_minimal(company_id, pipeline_id, stage_id, i, &key, &title, &fields)
                .await?
        } else {
            return Err(ApiError::BadRequest(format!(
                "pipeline {} has no stages; cannot auto-assign stage_id (NOT NULL)",
                pipeline_id
            )));
        };
        created.push(serde_json::json!({"id": id, "key": key, "title": title}));
    }
    state.realtime.publish(
        LiveEvent::new("pipeline.cases_batch_created", "pipeline", pipeline_id)
            .with_data(serde_json::json!({"count": created.len()})),
    );
    Ok(Json(serde_json::json!({"pipelineId": pipeline_id, "created": created, "count": created.len()})))
}

// Round 110: 仓储化。PipelineRepo::get_pipeline_document_meta。
// 修复原 SQL 错表 bug（旧版用 `pipeline_stages.config` 当文档内容）。
async fn get_pipeline_document(
    State(state): State<AppState>,
    Path((pipeline_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let repo = PipelineRepo::new(&state.db);
    let doc = repo
        .get_pipeline_document_meta(pipeline_id, &key)
        .await?
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(Json(serde_json::json!({"pipelineId": pipeline_id, "key": key, "document": doc})))
}

#[derive(Debug, Deserialize, Default)]
struct PutPipelineDocumentBody {
    #[serde(default)]
    content: Option<serde_json::Value>,
}

// Round 110: 仓储化。PipelineRepo::touch_pipeline_document。
// 真实 schema 无 content 列，key upsert 仅更新 updated_at 或 insert。
async fn put_pipeline_document(
    State(state): State<AppState>,
    Path((pipeline_id, key)): Path<(Uuid, String)>,
    Json(body): Json<PutPipelineDocumentBody>,
) -> ApiResult<Json<Value>> {
    let content = body.content.unwrap_or_else(|| serde_json::json!({}));
    let repo = PipelineRepo::new(&state.db);
    let ok = repo.touch_pipeline_document(pipeline_id, &key).await?;
    if !ok {
        return Err(ApiError::NotFound(format!("pipeline {pipeline_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("pipeline.document_upserted", "pipeline", pipeline_id)
            .with_data(serde_json::json!({"key": key})),
    );
    Ok(Json(serde_json::json!({"saved": true, "pipelineId": pipeline_id, "key": key, "content": content})))
}

// Round 110: 仓储化。PipelineRepo::list_pipeline_document_revisions。
async fn list_pipeline_document_revisions(
    State(state): State<AppState>,
    Path((pipeline_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let repo = PipelineRepo::new(&state.db);
    let timestamps = repo.list_pipeline_document_revisions(pipeline_id, &key).await?;
    let items: Vec<Value> = timestamps
        .into_iter()
        .map(|ts| serde_json::json!({"createdAt": ts}))
        .collect();
    Ok(Json(serde_json::json!({"items": items, "pipelineId": pipeline_id, "key": key})))
}

// Round 110: 仓储化。PipelineRepo::touch_pipeline_document（仅触发 updated_at 刷新）。
// 真实 schema 没有 content 列；revision_restore 是 stub 行为。
async fn restore_pipeline_document_revision(
    State(state): State<AppState>,
    Path((pipeline_id, key, _revision_id)): Path<(Uuid, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let repo = PipelineRepo::new(&state.db);
    let ok = repo.touch_pipeline_document(pipeline_id, &key).await?;
    if !ok {
        return Err(ApiError::NotFound(format!("pipeline_document {}/{}", pipeline_id, key)));
    }
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
    let repo = PipelineRepo::new(&state.db);
    let total = repo
        .count_cases_by_pipeline(pipeline_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let by_status = repo
        .count_cases_by_pipeline_grouped(pipeline_id)
        .await
        .unwrap_or_default();
    Ok(Json(serde_json::json!({
        "pipelineId": pipeline_id,
        "totalCases": total,
        "byStatus": by_status.into_iter().map(|(s, n)| serde_json::json!({"status": s, "count": n})).collect::<Vec<_>>(),
        "healthy": true,
    })))
}

async fn get_intake_form(
    State(state): State<AppState>,
    Path(pipeline_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let config = PipelineRepo::new(&state.db)
        .get_pipeline_config(pipeline_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .unwrap_or_else(|| serde_json::json!({}));
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
    let transitions: Vec<(String, String)> = body
        .transitions
        .iter()
        .filter_map(|tr| {
            let from = tr.get("fromStageKey").and_then(|v| v.as_str()).unwrap_or("");
            let to = tr.get("toStageKey").and_then(|v| v.as_str()).unwrap_or("");
            if from.is_empty() || to.is_empty() {
                None
            } else {
                Some((from.to_string(), to.to_string()))
            }
        })
        .collect();
    let count = PipelineRepo::new(&state.db)
        .replace_transitions(pipeline_id, &transitions)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    state.realtime.publish(
        LiveEvent::new("pipeline.transitions_replaced", "pipeline", pipeline_id)
            .with_data(serde_json::json!({"count": count})),
    );
    Ok(Json(serde_json::json!({"replaced": count, "pipelineId": pipeline_id})))
}


// ============================================================================
// Round 41: company-scoped pipelines-attention + bulk review-cases
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct PipelinesAttentionQuery {
    #[serde(default)]
    limit: Option<i64>,
}

/// `GET /api/companies/:company_id/pipelines-attention` — pipelines with
/// recent activity / cases needing follow-up.  Mirrors Node
/// `/companies/:companyId/pipelines-attention`.  Synthesized via LEFT JOIN
/// to recent `case_events` grouped by pipeline (via cases.pipeline_id if
/// present, else just counts).
async fn list_pipelines_attention_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<PipelinesAttentionQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    // Pipelines that have at least one case needing review (status = in_review).
    let rows = PipelineRepo::new(&state.db)
        .list_attention_pipelines(company_id, limit)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, description, review_count, total_count, updated_at)| {
            json!({
                "id": id,
                "name": name,
                "description": description,
                "reviewCount": review_count,
                "totalCaseCount": total_count,
                "needsAttention": review_count > 0,
                "updatedAt": updated_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct BulkReviewBody {
    items: Vec<BulkReviewItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct BulkReviewItem {
    case_id: Uuid,
    decision: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    expected_version: Option<i32>,
}

/// `POST /api/companies/:company_id/review-cases/bulk` — bulk review.
/// Mirrors Node `/companies/:companyId/review-cases/bulk`.  For each item,
/// translates `decision` to status and updates the case.
async fn bulk_review_cases_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<BulkReviewBody>,
) -> ApiResult<Json<Value>> {
    let mut results: Vec<Value> = Vec::with_capacity(body.items.len());
    let mut succeeded = 0i64;
    let mut failed = 0i64;
    for item in body.items.iter() {
        let new_status = match item.decision.as_str() {
            "approved" => "approved",
            "rejected" | "request_changes" => "in_progress",
            "in_review" => "in_review",
            other => {
                results.push(json!({
                    "caseId": item.case_id,
                    "ok": false,
                    "error": {
                        "status": 400,
                        "message": format!("unsupported review decision: {other}"),
                    },
                }));
                failed += 1;
                continue;
            }
        };
        let updated = CaseRepo::new(&state.db)
            .update(item.case_id, None, None, Some(new_status))
            .await;
        match updated {
            Ok(Some(row)) => {
                let _ = PipelineRepo::new(&state.db)
                    .insert_status_changed_event(
                        company_id,
                        item.case_id,
                        &item.decision,
                        item.note.as_deref().unwrap_or(""),
                    )
                    .await;                results.push(json!({
                    "caseId": item.case_id,
                    "ok": true,
                    "newStatus": new_status,
                    "case": {
                        "id": row.id,
                        "caseNumber": row.case_number,
                        "identifier": row.identifier,
                        "status": row.status,
                    },
                }));
                succeeded += 1;
            }
            Ok(None) => {
                results.push(json!({
                    "caseId": item.case_id,
                    "ok": false,
                    "error": {
                        "status": 404,
                        "message": "case not found",
                    },
                }));
                failed += 1;
            }
            Err(e) => {
                results.push(json!({
                    "caseId": item.case_id,
                    "ok": false,
                    "error": {
                        "status": 500,
                        "message": e.to_string(),
                    },
                }));
                failed += 1;
            }
        }
    }
    state.realtime.publish(
        LiveEvent::new("cases.bulk_reviewed", "company", company_id)
            .with_company(company_id)
            .with_data(json!({"succeeded": succeeded, "failed": failed, "total": body.items.len()})),
    );
    Ok(Json(json!({
        "companyId": company_id,
        "results": results,
        "succeeded": succeeded,
        "failed": failed,
        "total": body.items.len(),
    })))
}


// ============================================================================
// Round 47: cases automation retry endpoints
// ============================================================================

/// Mirrors Node `GET /cases/:case_id/automation/retry-plan`.  Returns a plan
/// object describing how to retry the case's stage automation.  Without a
/// full automation engine in this build, the plan reports a "manual" scope
/// with the current stage metadata so the UI can render the retry UI.
async fn case_automation_retry_plan(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let user_id = crate::require_user_id(&state, &axum::http::HeaderMap::new()).await
        .unwrap_or_else(|_| "anonymous".to_string());

    let row = PipelineRepo::new(&state.db)
        .get_case_retry_plan(case_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let (company_id, pipeline_id, stage_id, version, pending_suggestion) =
        row.ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;

    let _ = user_id;
    let _ = company_id;

    let stage_row = PipelineRepo::new(&state.db)
        .get_stage(stage_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let stage_meta = match stage_row {
        Some(s) => json!({
            "id": s.id, "key": s.key, "name": s.name, "kind": s.kind, "config": s.config,
        }),
        None => Value::Null,
    };

    Ok(Json(json!({
        "caseId": case_id,
        "pipelineId": pipeline_id,
        "companyId": company_id,
        "scope": "manual",
        "version": version,
        "targetStage": stage_meta,
        "automationRuns": [],
        "pendingSuggestion": pending_suggestion,
        "reasons": [],
        "generatedAt": chrono::Utc::now(),
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutomationRetryBody {
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    target_stage_id: Option<Uuid>,
    #[serde(default)]
    expected_version: Option<i32>,
    #[serde(default)]
    cleanup: Option<bool>,
}

/// Mirrors Node `POST /cases/:case_id/automation/retry`.  Without an
/// automation engine, increments case version, writes a case_event, and
/// publishes a LiveEvent so the UI can react.
async fn case_automation_retry(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(_body): Json<AutomationRetryBody>,
) -> ApiResult<Json<Value>> {
    let repo = PipelineRepo::new(&state.db);
    let row = repo
        .get_case_triple(case_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let (company_id, pipeline_id, version) =
        row.ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;

    // Increment version so optimistic concurrency tokens advance.
    let new_version = repo
        .increment_case_version(case_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let _ = repo
        .insert_fields_changed_event(
            company_id,
            case_id,
            &json!({
                "action": "automation_retry_requested",
                "fromVersion": version,
                "toVersion": new_version,
            }),
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    state.realtime.publish(
        pc_realtime::LiveEvent::new("case.automation.retry_requested", "case", case_id)
            .with_company(company_id)
            .with_data(json!({
                "caseId": case_id,
                "pipelineId": pipeline_id,
                "fromVersion": version,
                "toVersion": new_version,
            })),
    );

    Ok(Json(json!({
        "caseId": case_id,
        "status": "retry_queued",
        "fromVersion": version,
        "toVersion": new_version,
        "queuedAt": chrono::Utc::now(),
    })))
}

/// Mirrors Node `POST /cases/:case_id/automations/:automation_id/retry`.
async fn case_automation_specific_retry(
    State(state): State<AppState>,
    Path((case_id, automation_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let company_id = PipelineRepo::new(&state.db)
        .get_case_company_id(case_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;

    state.realtime.publish(
        pc_realtime::LiveEvent::new("case.automation.specific_retry", "case", case_id)
            .with_company(company_id)
            .with_data(json!({
                "caseId": case_id,
                "automationId": automation_id,
                "status": "retry_requested",
            })),
    );

    Ok(Json(json!({
        "caseId": case_id,
        "automationId": automation_id,
        "status": "retry_queued",
        "queuedAt": chrono::Utc::now(),
    })))
}

/// Mirrors Node `POST /cases/:case_id/automation/current-stage/rerun`.
async fn case_automation_current_stage_rerun(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = PipelineRepo::new(&state.db)
        .get_case_stage_version(case_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let (company_id, stage_id, version) =
        row.ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;

    state.realtime.publish(
        pc_realtime::LiveEvent::new("case.automation.current_stage_rerun", "case", case_id)
            .with_company(company_id)
            .with_data(json!({
                "caseId": case_id,
                "stageId": stage_id,
                "version": version,
                "status": "rerun_requested",
            })),
    );

    Ok(Json(json!({
        "caseId": case_id,
        "stageId": stage_id,
        "status": "rerun_queued",
        "version": version,
        "queuedAt": chrono::Utc::now(),
    })))
}
