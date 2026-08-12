//! `/api/pipelines*` 路由：CRUD。

use axum::Extension as AxumExtension;
#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
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

use pc_pipelines::{
    CreateCaseMinimalInput, CreatePipelineInput, CreateStageMinimalInput, CreateTransitionInput,
    PipelineHook, PipelineService, StageKind, UpdatePipelinePatch, UpdateStagePatch,
    case_events_db::{get_case_children_tree, get_direct_children_summary, list_company_case_events_page},
    case_events_enrichment::{enrich_cases_with_aggregation, enrich_pipelines_with_aggregation},
};
use pc_realtime::LiveEvent;
use pc_repos::case::CaseRepo;
use pc_repos::pipeline::PipelineRepo;

use crate::{ApiError, ApiResult, AppState};

// ===========================================================================
// R603 v5: service-layer helpers
// ===========================================================================

/// 构造一个自动触发 activity / realtime / plugin event 的 PipelineService。
///
/// 用 `Box::leak` 注入 `'static` — 因为 `PipelineActivityHook::new` 需要 `Arc<AppState>`
/// 而 service 借用 db（`&'a Db`）。`PipelineHook` 是 trait object（`Arc<dyn ...>`），
/// 所以可以在 service 生命周期之外继续存活。
fn pipeline_service_with_activity(state: &AppState) -> PipelineService<'static> {
    let state_arc = std::sync::Arc::new(state.clone());
    let hook: std::sync::Arc<dyn PipelineHook> =
        std::sync::Arc::new(crate::hooks::PipelineActivityHook::new(state_arc));
    let db_ref: &'static pc_repos::Db = Box::leak(Box::new(state.db.clone()));
    PipelineService::with_hooks(db_ref, vec![hook])
}

fn map_pipeline_service_error(e: pc_pipelines::PipelineServiceError, id: Uuid) -> ApiError {
    use pc_pipelines::PipelineServiceError::*;
    match e {
        NotFound(_) => ApiError::NotFound(format!("pipeline {id}")),
        InvalidInput(m) => ApiError::BadRequest(m),
        Forbidden(m) => ApiError::Forbidden(m),
        Repo(m) => ApiError::Internal(format!("repo: {m}")),
    }
}

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
        // ── R510: PUT aliased to both `/transitions` (Node parity) and
        // `/transitions/replace` (back-compat for existing Rust callers). Both
        // hit `replace_transitions` — a transactional DELETE all + INSERT new.
        .route(
            "/api/pipelines/:id/transitions",
            get(list_transitions)
                .post(create_transition)
                .put(replace_transitions),
        )
        // cases
        .route(
            "/api/pipelines/:id/cases",
            get(list_cases).post(create_case),
        )
        .route("/api/cases/:case_id/transition", post(transition_case))
        .route("/api/cases/:case_id/claim", post(claim_case_route))
        .route("/api/cases/:case_id/release", post(release_case_route))
        // NOTE: `/api/cases/:case_id/{events,issue-links,issue-links/:link_id}` are
        // registered by cases.rs (the canonical cases router module). The duplicate
        // registrations here were removed in Round 282 because they produced axum
        // "Overlapping method route" panics during integration tests. The local
        // handlers (`list_case_events`, `list_case_links`, `link_case_issue_route`,
        // `unlink_case_issue_route`) remain as dead code (kept for reference).
        // archive
        .route("/api/pipelines/:id/archive", post(archive_pipeline))
        // additional pipeline sub-routes
        .route(
            "/api/pipelines/:id/stages/:stage_id/automation-env",
            patch(patch_stage_automation_env),
        )
        .route("/api/pipelines/:id/cases/batch", post(create_cases_batch))
        .route(
            "/api/pipelines/:id/documents/:key",
            get(get_pipeline_document).put(put_pipeline_document),
        )
        .route(
            "/api/pipelines/:id/documents/:key/revisions",
            get(list_pipeline_document_revisions),
        )
        .route(
            "/api/pipelines/:id/documents/:key/revisions/:revision_id/restore",
            post(restore_pipeline_document_revision),
        )
        .route("/api/pipelines/:id/health", get(get_pipeline_health))
        .route("/api/pipelines/:id/intake-form", get(get_intake_form))
        .route(
            "/api/pipelines/:id/transitions/replace",
            put(replace_transitions),
        )
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
        // ---- R639.2.3: company-wide case events + direct children rollup ----
        .route(
            "/api/companies/:company_id/case-events",
            get(list_company_case_events_route),
        )
        .route(
            "/api/cases/:case_id/rollup",
            get(case_direct_children_rollup_route),
        )
        .route(
            "/api/cases/:case_id/children/tree",
            get(case_children_tree_route),
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
    // R603 v5: 业务下沉到 PipelineService
    let svc = pipeline_service_with_activity(&state);
    match q.company_id {
        // R639.2.7: 当指定 company_id 时,注入 R639.2.6 enrichment
        // (descendantActiveWorkCount + connections.upstreamPipelineIds/downstreamPipelineIds)
        // 与 Node 上游 /companies/:companyId/pipelines 端点 1:1 对齐
        Some(cid) => {
            let rows = svc
                .list_by_company(cid)
                .await
                .map_err(|e| map_pipeline_service_error(e, Uuid::nil()))?;
            let enriched =
                enrich_pipelines_with_aggregation(state.db.pool(), cid, rows).await?;
            Ok(Json(serde_json::to_value(enriched).unwrap_or_default()))
        }
        None => {
            let rows = svc
                .list_all(200)
                .await
                .map_err(|e| map_pipeline_service_error(e, Uuid::nil()))?;
            Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
        }
    }
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    // R603 v5: 业务下沉到 PipelineService
    // 跨公司访问通过 service.get 返回 None（service.get 内部 ensure_in_company 校验）
    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .get(id_company_id(&state, id).await?, id)
        .await
        .map_err(|e| map_pipeline_service_error(e, id))?
        .ok_or_else(|| ApiError::NotFound(format!("pipeline {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

/// 内部辅助：通过 pipeline row 反查 company_id（service.get 需要 company_id 参数）。
async fn id_company_id(state: &AppState, id: Uuid) -> ApiResult<Uuid> {
    let row = PipelineRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("pipeline {id}")))?;
    Ok(row.company_id)
}

/// 内部辅助：通过 pipeline_case row 反查 company_id（service 子资源需要 company_id）。
async fn case_company_id(state: &AppState, case_id: Uuid) -> ApiResult<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT company_id FROM pipeline_cases WHERE id = $1")
        .bind(case_id)
        .fetch_optional(state.db.pool())
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))
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
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    // pc-authz：创建 pipeline 需要 PipelinesWrite 权限
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        body.company_id,
        PermissionKey::PipelinesWrite,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    // R603 v5: 业务下沉到 PipelineService（key/name trim 校验、hook 触发均在 service 内）
    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .create(
            body.company_id,
            &CreatePipelineInput {
                key: body.key,
                name: body.name,
                description: body.description,
            },
        )
        .await
        .map_err(|e| map_pipeline_service_error(e, Uuid::nil()))?;
    // 注意：service 已通过 PipelineActivityHook 自动 publish 了 realtime（"pipeline.created"）。
    // 这里保留 telemetry track。
    global::track(
        "pipeline.created",
        BTreeMap::from([
            (
                "company_id".into(),
                serde_json::json!(row.company_id.to_string()),
            ),
            ("name".into(), serde_json::json!(row.name.clone())),
        ]),
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
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    // pc-authz：查公司以做权限检查
    let company_id = id_company_id(&state, id).await?;
    if let Err(err) =
        enforce_permission(&state.db, &actor, company_id, PermissionKey::PipelinesWrite).await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    // R603 v5: 业务下沉到 PipelineService
    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .update(
            company_id,
            id,
            &UpdatePipelinePatch {
                name: body.name,
                description: body.description,
            },
        )
        .await
        .map_err(|e| map_pipeline_service_error(e, id))?;
    // service 已通过 PipelineActivityHook 自动 publish realtime。
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
) -> ApiResult<StatusCode> {
    let company_id = id_company_id(&state, id).await?;
    if let Err(err) =
        enforce_permission(&state.db, &actor, company_id, PermissionKey::PipelinesWrite).await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    // R603 v5: 业务下沉到 PipelineService
    let svc = pipeline_service_with_activity(&state);
    let ok = svc
        .delete(company_id, id)
        .await
        .map_err(|e| map_pipeline_service_error(e, id))?;
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
    // R603 v5: 业务下沉到 PipelineService
    let company_id = id_company_id(&state, id).await?;
    let svc = pipeline_service_with_activity(&state);
    let rows = svc
        .list_stages(company_id, id)
        .await
        .map_err(|e| map_pipeline_service_error(e, id))?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_stage(
    State(state): State<AppState>,
    Path((_id, stage_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    // R603 v5: 业务下沉到 PipelineService
    // service.get_stage 需要 company_id；通过 stage 反查 pipeline_id → company_id。
    let pipeline_id_opt: Option<Uuid> =
        sqlx::query_scalar("SELECT pipeline_id FROM pipeline_stages WHERE id = $1")
            .bind(stage_id)
            .fetch_optional(state.db.pool())
            .await?;
    let pipeline_id =
        pipeline_id_opt.ok_or_else(|| ApiError::NotFound(format!("stage {stage_id}")))?;
    let company_id = id_company_id(&state, pipeline_id).await?;
    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .get_stage(company_id, stage_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, pipeline_id))?
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
    // R603 v5: 业务下沉到 PipelineService
    let company_id = id_company_id(&state, id).await?;
    let kind = StageKind::from_db_str(&body.kind).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "invalid stage kind: {} (allowed: working/review/done/cancelled)",
            body.kind
        ))
    })?;
    let config = body.config.unwrap_or_else(|| serde_json::json!({}));
    let position = body.position.unwrap_or(0);
    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .create_stage(
            company_id,
            id,
            &CreateStageMinimalInput {
                key: body.key,
                name: body.name,
                kind,
                position,
                config,
            },
        )
        .await
        .map_err(|e| map_pipeline_service_error(e, id))?;
    // service 已通过 PipelineActivityHook 自动 publish realtime。
    global::track(
        "pipeline.stage.created",
        BTreeMap::from([
            ("pipeline_id".into(), serde_json::json!(id.to_string())),
            ("name".into(), serde_json::json!(row.name.clone())),
        ]),
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
    // R603 v5: 业务下沉到 PipelineService
    // service.update_stage 需要 company_id；通过 stage 反查 pipeline_id → company_id。
    let pipeline_id_opt: Option<Uuid> =
        sqlx::query_scalar("SELECT pipeline_id FROM pipeline_stages WHERE id = $1")
            .bind(stage_id)
            .fetch_optional(state.db.pool())
            .await?;
    let pipeline_id =
        pipeline_id_opt.ok_or_else(|| ApiError::NotFound(format!("stage {stage_id}")))?;
    let company_id = id_company_id(&state, pipeline_id).await?;
    let kind = match body.kind.as_deref() {
        None => None,
        Some(s) => Some(
            StageKind::from_db_str(s)
                .ok_or_else(|| ApiError::BadRequest(format!("invalid stage kind: {s}")))?,
        ),
    };
    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .update_stage(
            company_id,
            stage_id,
            &UpdateStagePatch {
                name: body.name,
                kind,
                position: body.position,
                config: body.config,
            },
        )
        .await
        .map_err(|e| map_pipeline_service_error(e, pipeline_id))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove_stage(
    State(state): State<AppState>,
    Path((_id, stage_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    // R603 v5: 业务下沉到 PipelineService
    let pipeline_id_opt: Option<Uuid> =
        sqlx::query_scalar("SELECT pipeline_id FROM pipeline_stages WHERE id = $1")
            .bind(stage_id)
            .fetch_optional(state.db.pool())
            .await?;
    let pipeline_id =
        pipeline_id_opt.ok_or_else(|| ApiError::NotFound(format!("stage {stage_id}")))?;
    let company_id = id_company_id(&state, pipeline_id).await?;
    let svc = pipeline_service_with_activity(&state);
    let ok = svc
        .delete_stage(company_id, stage_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, pipeline_id))?;
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
    // R603 v5: 业务下沉到 PipelineService
    let company_id = id_company_id(&state, id).await?;
    let svc = pipeline_service_with_activity(&state);
    let rows = svc
        .list_transitions(company_id, id)
        .await
        .map_err(|e| map_pipeline_service_error(e, id))?;
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
    // R603 v5: 业务下沉到 PipelineService
    let company_id = id_company_id(&state, id).await?;
    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .create_transition(
            company_id,
            id,
            &CreateTransitionInput {
                from_stage_id: body.from_stage_id,
                to_stage_id: body.to_stage_id,
                label: body.label,
            },
        )
        .await
        .map_err(|e| map_pipeline_service_error(e, id))?;
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
    // R603 v5: 业务下沉到 PipelineService
    let company_id = id_company_id(&state, id).await?;
    let svc = pipeline_service_with_activity(&state);
    let rows = svc
        .list_cases(company_id, id, q.stage_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, id))?;
    // R639.2.8: 注入 case-level aggregation (activeWork + descendantActiveWorkCount)
    // 与 Node 上游 /companies/:companyId/cases 端点 1:1 对齐
    let enriched = enrich_cases_with_aggregation(state.db.pool(), company_id, rows).await?;
    Ok(Json(serde_json::to_value(enriched).unwrap_or_default()))
}

async fn get_case(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // R603 v5: 业务下沉到 PipelineService
    // service.get_case 需要 company_id；通过 case 反查。
    let company_id_opt: Option<Uuid> =
        sqlx::query_scalar("SELECT company_id FROM pipeline_cases WHERE id = $1")
            .bind(case_id)
            .fetch_optional(state.db.pool())
            .await?;
    let company_id = company_id_opt.ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .get_case(company_id, case_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, Uuid::nil()))?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    // R639.2.9: 注入 case enrichment (activeWork + descendantActiveWorkCount)
    // 与 Node 上游 getCaseDetail 端点 1:1 对齐 (核心字段)
    let mut enriched = enrich_cases_with_aggregation(state.db.pool(), company_id, vec![row])
        .await?;
    let first = enriched
        .pop()
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    Ok(Json(serde_json::to_value(first).unwrap_or_default()))
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
    // R603 v5: 业务下沉到 PipelineService
    let company_id = id_company_id(&state, id).await?;
    let fields = body.fields.unwrap_or_else(|| serde_json::json!({}));
    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .create_case(
            company_id,
            id,
            &CreateCaseMinimalInput {
                case_key: body.case_key,
                title: body.title,
                stage_id: body.stage_id,
                summary: body.summary,
                fields,
                parent_case_id: body.parent_case_id,
                created_by_user_id: body.created_by_user_id,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .map_err(|e| map_pipeline_service_error(e, id))?;
    // service 已通过 PipelineActivityHook 自动 publish realtime。
    global::track(
        "pipeline.case.created",
        BTreeMap::from([
            (
                "company_id".into(),
                serde_json::json!(row.company_id.to_string()),
            ),
            ("case_id".into(), serde_json::json!(row.id.to_string())),
        ]),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

// R603 v6.2: 业务下沉到 PipelineService.transition_case（事务化 update + event）。

#[derive(Debug, Deserialize)]
struct TransitionCaseBody {
    to_stage_id: Uuid,
    #[serde(default)]
    actor_user_id: Option<String>,
    #[serde(default = "default_transition_actor")]
    actor_type: String,
}
fn default_transition_actor() -> String {
    "user".into()
}

async fn transition_case(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<TransitionCaseBody>,
) -> ApiResult<Json<Value>> {
    // service.transition_case 需要 from_stage_id + company_id。
    let pipeline_id_opt: Option<Uuid> =
        sqlx::query_scalar("SELECT pipeline_id FROM pipeline_cases WHERE id = $1")
            .bind(case_id)
            .fetch_optional(state.db.pool())
            .await?;
    let pipeline_id =
        pipeline_id_opt.ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let company_id = id_company_id(&state, pipeline_id).await?;

    // 读取当前 case 的 stage_id 作为 from_stage_id（避免客户端传错）
    let from_stage_id: Option<Uuid> =
        sqlx::query_scalar("SELECT stage_id FROM pipeline_cases WHERE id = $1")
            .bind(case_id)
            .fetch_optional(state.db.pool())
            .await?;
    let from_stage_id =
        from_stage_id.ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;

    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .transition_case(
            company_id,
            case_id,
            &pc_pipelines::TransitionCaseInput {
                from_stage_id,
                to_stage_id: body.to_stage_id,
                actor_user_id: body.actor_user_id,
                actor_type: body.actor_type,
            },
        )
        .await
        .map_err(|e| match e {
            // 乐观锁失败 → 409 Conflict（保持与原行为一致）
            pc_pipelines::PipelineServiceError::InvalidInput(msg)
                if msg.contains("optimistic lock") =>
            {
                ApiError::Conflict(msg)
            }
            other => map_pipeline_service_error(other, pipeline_id),
        })?;
    // service 已通过 PipelineActivityHook 自动 publish realtime / activity。
    global::track(
        "pipeline.case.transitioned",
        BTreeMap::from([
            (
                "company_id".into(),
                serde_json::json!(row.company_id.to_string()),
            ),
            ("case_id".into(), serde_json::json!(row.id.to_string())),
        ]),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

// R603 v6.3: case 子资源（claim / release / remove / events）下沉到 PipelineService。

async fn claim_case_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user = headers
        .get("x-paperclip-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let company_id = case_company_id(&state, case_id).await?;
    let svc = pipeline_service_with_activity(&state);
    let owner = match user {
        Some(u) => pc_pipelines::CaseOwner::User(u),
        None => pc_pipelines::CaseOwner::User("anonymous".into()),
    };
    let row = svc
        .claim_case(
            company_id,
            case_id,
            &pc_pipelines::ClaimCaseInput {
                owner,
                lease_token: Uuid::new_v4(),
            },
        )
        .await
        .map_err(|e| map_pipeline_service_error(e, case_id))?;
    // service 不直接发 realtime（lease 不是 lifecycle event）；路由层保留 realtime publish。
    state.realtime.publish(
        LiveEvent::new("pipeline.case.claimed", "pipeline_case", row.id)
            .with_company(row.company_id),
    );
    global::track(
        "pipeline.case.claimed",
        BTreeMap::from([
            (
                "company_id".into(),
                serde_json::json!(row.company_id.to_string()),
            ),
            ("case_id".into(), serde_json::json!(row.id.to_string())),
        ]),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn release_case_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let company_id = case_company_id(&state, case_id).await?;
    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .release_case(company_id, case_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, case_id))?;
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
    // service.delete_case 通过 case_company_or 自动校验 company_id
    let company_id = case_company_id(&state, case_id).await?;
    let svc = pipeline_service_with_activity(&state);
    let ok = svc
        .delete_case(company_id, case_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, case_id))?;
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
    // R603 v6.3 备选：原实现（dead code，保留以备 reference）。
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
    AxumExtension(actor): AxumExtension<AuthContext>,
) -> ApiResult<Json<Value>> {
    let company_id = id_company_id(&state, id).await?;
    if let Err(err) =
        enforce_permission(&state.db, &actor, company_id, PermissionKey::PipelinesWrite).await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    // R603 v5: 业务下沉到 PipelineService
    let svc = pipeline_service_with_activity(&state);
    let row = svc
        .archive(company_id, id)
        .await
        .map_err(|e| map_pipeline_service_error(e, id))?;
    // service 已通过 PipelineActivityHook 自动 publish realtime（"pipeline.archived"）。
    global::track(
        "pipeline.archived",
        BTreeMap::from([
            (
                "company_id".into(),
                serde_json::json!(row.company_id.to_string()),
            ),
            ("pipeline_id".into(), serde_json::json!(row.id.to_string())),
        ]),
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
// R603 v6.4: 业务下沉到 PipelineService.patch_stage_automation_env。
async fn patch_stage_automation_env(
    State(state): State<AppState>,
    Path((_id, stage_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<StageAutomationEnvBody>,
) -> ApiResult<Json<Value>> {
    let svc = pipeline_service_with_activity(&state);
    let ok = svc
        .patch_stage_automation_env(
            stage_id,
            &pc_pipelines::PatchStageAutomationEnvInput {
                automation_env: body.automation_env.clone(),
            },
        )
        .await
        .map_err(|e| map_pipeline_service_error(e, stage_id))?;
    if !ok {
        return Err(ApiError::NotFound(format!("stage {stage_id}")));
    }
    let env = body.automation_env.unwrap_or_else(|| serde_json::json!({}));
    state.realtime.publish(LiveEvent::new(
        "pipeline.stage_automation_env_updated",
        "stage",
        stage_id,
    ));
    Ok(Json(
        serde_json::json!({"updated": true, "stageId": stage_id, "automationEnv": env}),
    ))
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
// R603 v6.4: 业务下沉到 PipelineService.create_cases_batch。
async fn create_cases_batch(
    State(state): State<AppState>,
    Path(pipeline_id): Path<Uuid>,
    Json(body): Json<BatchCaseBody>,
) -> ApiResult<Json<Value>> {
    let company_id = id_company_id(&state, pipeline_id).await?;
    let svc = pipeline_service_with_activity(&state);
    let input = pc_pipelines::CreateCasesBatchInput {
        cases: body
            .cases
            .into_iter()
            .map(|c| pc_pipelines::BatchCaseItem {
                key: c.key,
                title: c.title,
                fields: c.fields,
            })
            .collect(),
    };
    let rows = svc
        .create_cases_batch(company_id, pipeline_id, &input)
        .await
        .map_err(|e| map_pipeline_service_error(e, pipeline_id))?;
    let created: Vec<Value> = rows
        .iter()
        .map(|r| serde_json::json!({"id": r.id, "key": r.case_key, "title": r.title}))
        .collect();
    // service 已通过 PipelineActivityHook 自动 publish 每个 case 的 activity / realtime；
    // 这里只保留批量 realtime event。
    state.realtime.publish(
        LiveEvent::new("pipeline.cases_batch_created", "pipeline", pipeline_id)
            .with_data(serde_json::json!({"count": created.len()})),
    );
    Ok(Json(
        serde_json::json!({"pipelineId": pipeline_id, "created": created, "count": created.len()}),
    ))
}

// R603 v6.5: 业务下沉到 PipelineService.get_pipeline_document。
async fn get_pipeline_document(
    State(state): State<AppState>,
    Path((pipeline_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let company_id = id_company_id(&state, pipeline_id).await?;
    let svc = pipeline_service_with_activity(&state);
    let doc = svc
        .get_pipeline_document(company_id, pipeline_id, &key)
        .await
        .map_err(|e| map_pipeline_service_error(e, pipeline_id))?
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(Json(
        serde_json::json!({"pipelineId": pipeline_id, "key": key, "document": doc}),
    ))
}

#[derive(Debug, Deserialize, Default)]
struct PutPipelineDocumentBody {
    #[serde(default)]
    content: Option<serde_json::Value>,
}

// R603 v6.5: 业务下沉到 PipelineService.put_pipeline_document。
async fn put_pipeline_document(
    State(state): State<AppState>,
    Path((pipeline_id, key)): Path<(Uuid, String)>,
    Json(body): Json<PutPipelineDocumentBody>,
) -> ApiResult<Json<Value>> {
    let company_id = id_company_id(&state, pipeline_id).await?;
    let content = body
        .content
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    let svc = pipeline_service_with_activity(&state);
    let ok = svc
        .put_pipeline_document(
            company_id,
            pipeline_id,
            &pc_pipelines::UpsertPipelineDocumentInput {
                key: key.clone(),
                content: content.clone(),
            },
        )
        .await
        .map_err(|e| map_pipeline_service_error(e, pipeline_id))?;
    if !ok {
        return Err(ApiError::NotFound(format!("pipeline {pipeline_id}")));
    }
    Ok(Json(
        serde_json::json!({"saved": true, "pipelineId": pipeline_id, "key": key, "content": content}),
    ))
}

// R603 v6.5: 业务下沉到 PipelineService.list_pipeline_document_revisions。
async fn list_pipeline_document_revisions(
    State(state): State<AppState>,
    Path((pipeline_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let company_id = id_company_id(&state, pipeline_id).await?;
    let svc = pipeline_service_with_activity(&state);
    let timestamps = svc
        .list_pipeline_document_revisions(company_id, pipeline_id, &key)
        .await
        .map_err(|e| map_pipeline_service_error(e, pipeline_id))?;
    let items: Vec<Value> = timestamps
        .into_iter()
        .map(|ts| serde_json::json!({"createdAt": ts}))
        .collect();
    Ok(Json(
        serde_json::json!({"items": items, "pipelineId": pipeline_id, "key": key}),
    ))
}

// R603 v6.5: 业务下沉到 PipelineService.restore_pipeline_document_revision。
// 真实 schema 缺 content 列；restore 是 stub（仅 touch updated_at）。
async fn restore_pipeline_document_revision(
    State(state): State<AppState>,
    Path((pipeline_id, key, revision_id)): Path<(Uuid, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let company_id = id_company_id(&state, pipeline_id).await?;
    let svc = pipeline_service_with_activity(&state);
    let ok = svc
        .restore_pipeline_document_revision(company_id, pipeline_id, &key, revision_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, pipeline_id))?;
    if !ok {
        return Err(ApiError::NotFound(format!(
            "pipeline_document {pipeline_id}/{key}"
        )));
    }
    Ok(Json(
        serde_json::json!({"restored": true, "pipelineId": pipeline_id, "key": key}),
    ))
}

// R603 v6.4: 业务下沉到 PipelineService.get_pipeline_health。
async fn get_pipeline_health(
    State(state): State<AppState>,
    Path(pipeline_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let svc = pipeline_service_with_activity(&state);
    let health = svc
        .get_pipeline_health(pipeline_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, pipeline_id))?;
    Ok(Json(serde_json::json!({
        "pipelineId": health.pipeline_id,
        "totalCases": health.total_cases,
        "byStatus": health.by_status.into_iter().map(|(s, n)| serde_json::json!({"status": s, "count": n})).collect::<Vec<_>>(),
        "healthy": health.healthy,
    })))
}

// R603 v6.4: 业务下沉到 PipelineService.get_intake_form。
async fn get_intake_form(
    State(state): State<AppState>,
    Path(pipeline_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let svc = pipeline_service_with_activity(&state);
    let form = svc
        .get_intake_form(pipeline_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, pipeline_id))?;
    Ok(Json(
        serde_json::json!({"pipelineId": pipeline_id, "form": form}),
    ))
}

#[derive(Debug, Deserialize, Default)]
struct ReplaceTransitionsBody {
    transitions: Vec<serde_json::Value>,
}

// R603 v6.4: 业务下沉到 PipelineService.replace_transitions（事务化）。
async fn replace_transitions(
    State(state): State<AppState>,
    Path(pipeline_id): Path<Uuid>,
    Json(body): Json<ReplaceTransitionsBody>,
) -> ApiResult<Json<Value>> {
    let company_id = id_company_id(&state, pipeline_id).await?;
    let svc = pipeline_service_with_activity(&state);
    let count = svc
        .replace_transitions(
            company_id,
            pipeline_id,
            &pc_pipelines::ReplaceTransitionsInput {
                transitions: body.transitions.clone(),
            },
        )
        .await
        .map_err(|e| map_pipeline_service_error(e, pipeline_id))?;
    state.realtime.publish(
        LiveEvent::new("pipeline.transitions_replaced", "pipeline", pipeline_id)
            .with_data(serde_json::json!({"count": count})),
    );
    Ok(Json(
        serde_json::json!({"replaced": count, "pipelineId": pipeline_id}),
    ))
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
// R603 v6.6: 业务下沉到 PipelineService.list_attention_pipelines。
async fn list_pipelines_attention_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<PipelinesAttentionQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let svc = pipeline_service_with_activity(&state);
    let rows = svc
        .list_attention_pipelines(company_id, limit)
        .await
        .map_err(|e| map_pipeline_service_error(e, company_id))?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(
            |(id, name, description, review_count, total_count, updated_at)| {
                json!({
                    "id": id,
                    "name": name,
                    "description": description,
                    "reviewCount": review_count,
                    "totalCaseCount": total_count,
                    "needsAttention": review_count > 0,
                    "updatedAt": updated_at,
                })
            },
        )
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct CaseEventsQuery {
    #[serde(default)]
    types: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
}

/// `GET /api/companies/:company_id/case-events` - 1:1 with Node listCompanyCaseEvents.
async fn list_company_case_events_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<CaseEventsQuery>,
) -> ApiResult<Json<Value>> {
    let types: Vec<String> = q
        .types
        .as_deref()
        .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
        .unwrap_or_default();
    let page = list_company_case_events_page(
        state.db.pool(),
        company_id,
        &types,
        q.limit,
        q.offset,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("list_company_case_events_page: {e}")))?;
    Ok(Json(serde_json::to_value(page).unwrap_or_default()))
}

/// `GET /api/cases/:case_id/rollup` - 1:1 with Node getDirectChildrenSummary.
async fn case_direct_children_rollup_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let company_id = CaseRepo::new(&state.db)
        .get_case_company_id(case_id)
        .await
        .map_err(|e| ApiError::Internal(format!("lookup case company: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("pipeline case {case_id}")))?;
    let rollup = get_direct_children_summary(state.db.pool(), company_id, case_id)
        .await
        .map_err(|e| ApiError::Internal(format!("get_direct_children_summary: {e}")))?;
    Ok(Json(serde_json::to_value(rollup).unwrap_or_default()))
}

/// `GET /api/cases/:case_id/children/tree` - 1:1 with Node getCaseChildrenTree.
async fn case_children_tree_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let company_id = CaseRepo::new(&state.db)
        .get_case_company_id(case_id)
        .await
        .map_err(|e| ApiError::Internal(format!("lookup case company: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("pipeline case {case_id}")))?;
    let tree = get_case_children_tree(state.db.pool(), company_id, case_id)
        .await
        .map_err(|e| ApiError::Internal(format!("get_case_children_tree: {e}")))?;
    match tree {
        Some(t) => Ok(Json(serde_json::to_value(t).unwrap_or_default())),
        None => Err(ApiError::NotFound(format!("pipeline case {case_id}"))),
    }
}
/// `POST /api/companies/:company_id/review-cases/bulk` — bulk review.
/// Mirrors Node `/companies/:companyId/review-cases/bulk`.  For each item,
/// translates `decision` to status and updates the case.
// R603 v6.6: 业务下沉到 PipelineService.bulk_review_cases。
async fn bulk_review_cases_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<BulkReviewBody>,
) -> ApiResult<Json<Value>> {
    let svc = pipeline_service_with_activity(&state);
    let items: Vec<pc_pipelines::BulkReviewItem> = body
        .items
        .iter()
        .map(|i| pc_pipelines::BulkReviewItem {
            case_id: i.case_id,
            decision: i.decision.clone(),
            note: i.note.clone(),
            expected_version: i.expected_version,
        })
        .collect();
    let result = svc
        .bulk_review_cases(company_id, &items)
        .await
        .map_err(|e| map_pipeline_service_error(e, company_id))?;
    let results: Vec<Value> = result
        .results
        .into_iter()
        .map(|r| {
            let mut obj = json!({
                "caseId": r.case_id,
                "ok": r.ok,
            });
            if let Some(s) = r.new_status {
                obj["newStatus"] = json!(s);
            }
            if let Some(c) = r.case {
                obj["case"] = json!({
                    "id": c.id,
                    "caseNumber": c.case_number,
                    "identifier": c.identifier,
                    "status": c.status,
                });
            }
            if let Some(err) = r.error {
                let status = if err == "case not found" { 404 } else { 400 };
                obj["error"] = json!({"status": status, "message": err});
            }
            obj
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "results": results,
        "succeeded": result.succeeded,
        "failed": result.failed,
        "total": result.total,
    })))
}

// ============================================================================
// Round 47: cases automation retry endpoints
// ============================================================================

/// Mirrors Node `GET /cases/:case_id/automation/retry-plan`.  Returns a plan
/// object describing how to retry the case's stage automation.  Without a
/// full automation engine in this build, the plan reports a "manual" scope
/// with the current stage metadata so the UI can render the retry UI.
// R603 v6.6: 业务下沉到 PipelineService.get_case_automation_retry_plan。
async fn case_automation_retry_plan(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let svc = pipeline_service_with_activity(&state);
    let plan = svc
        .get_case_automation_retry_plan(case_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, case_id))?;
    Ok(Json(json!({
        "caseId": plan.case_id,
        "pipelineId": plan.pipeline_id,
        "companyId": plan.company_id,
        "scope": "manual",
        "version": plan.version,
        "targetStage": plan.target_stage,
        "automationRuns": [],
        "pendingSuggestion": plan.pending_suggestion,
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
// R603 v6.6: 业务下沉到 PipelineService.request_case_automation_retry。
async fn case_automation_retry(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(_body): Json<AutomationRetryBody>,
) -> ApiResult<Json<Value>> {
    let svc = pipeline_service_with_activity(&state);
    let r = svc
        .request_case_automation_retry(case_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, case_id))?;
    Ok(Json(json!({
        "caseId": r.case_id,
        "status": "retry_queued",
        "fromVersion": r.from_version,
        "toVersion": r.to_version,
        "queuedAt": chrono::Utc::now(),
    })))
}

/// Mirrors Node `POST /cases/:case_id/automations/:automation_id/retry`.
// R603 v6.6: 业务下沉到 PipelineService.request_case_automation_specific_retry。
async fn case_automation_specific_retry(
    State(state): State<AppState>,
    Path((case_id, automation_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let svc = pipeline_service_with_activity(&state);
    let (_cid, _company_id) = svc
        .request_case_automation_specific_retry(case_id, automation_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, case_id))?;
    Ok(Json(json!({
        "caseId": case_id,
        "automationId": automation_id,
        "status": "retry_queued",
        "queuedAt": chrono::Utc::now(),
    })))
}

/// Mirrors Node `POST /cases/:case_id/automation/current-stage/rerun`.
// R603 v6.6: 业务下沉到 PipelineService.request_case_automation_current_stage_rerun。
async fn case_automation_current_stage_rerun(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let svc = pipeline_service_with_activity(&state);
    let (_cid, stage_id, version) = svc
        .request_case_automation_current_stage_rerun(case_id)
        .await
        .map_err(|e| map_pipeline_service_error(e, case_id))?;
    Ok(Json(json!({
        "caseId": case_id,
        "stageId": stage_id,
        "status": "rerun_queued",
        "version": version,
        "queuedAt": chrono::Utc::now(),
    })))
}
