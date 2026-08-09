//! `/api/routines*` 路由：CRUD + trigger。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use std::collections::BTreeMap;
use pc_telemetry::global;
use axum::Extension as AxumExtension;
use pc_auth::AuthContext;
use pc_authz::{enforce_permission, PermissionKey};

use pc_realtime::LiveEvent;
use pc_repos::routine::{
    CreateRoutineRecord, CreateRoutineTriggerRecord, CreateWebhookSecretInput, FireTriggerInput,
    NewRoutineAnnotationComment, NewRoutineAnnotationThread, RoutineAnnotationCommentRow,
    RoutineAnnotationPatch, RoutineAnnotationThreadRow, RoutineRepo, RunRoutineRecord,
    UpdateRoutineRecord, UpdateRoutineTriggerRecord,
};
use pc_secrets::local_encrypted::LocalEncryptedProvider;
use pc_secrets::SecretProvider;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/routines",
            get(list_company).post(create_company),
        )
        .route("/api/routines", get(list).post(create))
        .route(
            "/api/routines/:id",
            get(get_one).patch(update).delete(remove),
        )
        .route("/api/routines/:id/trigger", post(trigger))
        // revisions
        .route(
            "/api/routines/:id/revisions",
            get(list_revisions).post(create_revision),
        )
        .route(
            "/api/routines/:id/revisions/:revision_id/restore",
            post(restore_revision),
        )
        // runs
        .route("/api/routines/:id/runs", get(list_runs).post(create_run))
        .route("/api/routines/:id/run", post(run_routine))
        // triggers
        .route(
            "/api/routines/:id/triggers",
            get(list_triggers).post(create_trigger),
        )
        .route(
            "/api/routine-triggers/:trigger_id",
            get(get_trigger)
                .patch(update_trigger)
                .delete(remove_trigger),
        )
        .route(
            "/api/routine-triggers/public/:public_id/fire",
            post(fire_public_trigger),
        )
        // ── Round 23: routine description annotations + trigger secret rotation ──
        .route(
            "/api/routines/:id/description/annotations",
            get(list_routine_description_annotations),
        )
        .route(
            "/api/routines/:id/description/annotations",
            post(create_routine_description_annotation),
        )
        .route(
            "/api/routines/:id/description/annotations/:thread_id",
            get(get_routine_description_annotation),
        )
        .route(
            "/api/routines/:id/description/annotations/:thread_id",
            patch(patch_routine_description_annotation),
        )
        .route(
            "/api/routines/:id/description/annotations/:thread_id/comments",
            post(add_routine_description_annotation_comment),
        )
        .route(
            "/api/routine-triggers/:trigger_id/rotate-secret",
            post(rotate_trigger_secret_route),
        )
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ListQuery {
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    project_id: Option<Uuid>,
}

async fn list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = match q.company_id {
        Some(cid) => {
            RoutineRepo::new(&state.db)
                .list_by_company_filtered(cid, q.project_id)
                .await?
        }
        None => RoutineRepo::new(&state.db).list_all(200).await?,
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let repo = RoutineRepo::new(&state.db);
    let row = repo
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {id}")))?;
    let triggers = repo.list_triggers(id).await?;
    let recent_runs = repo.list_run_summaries(id, 25).await?;
    let active_issue = repo.get_active_issue(id).await?;
    let description_document = repo.get_description_document(id).await?;
    let mut detail = serde_json::to_value(row).unwrap_or_default();
    if let Some(object) = detail.as_object_mut() {
        object.insert("project".into(), Value::Null);
        object.insert("assignee".into(), Value::Null);
        object.insert("parentIssue".into(), Value::Null);
        object.insert(
            "descriptionDocument".into(),
            serde_json::to_value(description_document).unwrap_or(Value::Null),
        );
        object.insert(
            "triggers".into(),
            serde_json::to_value(triggers).unwrap_or_else(|_| json!([])),
        );
        object.insert(
            "recentRuns".into(),
            serde_json::to_value(recent_runs).unwrap_or_else(|_| json!([])),
        );
        object.insert(
            "activeIssue".into(),
            serde_json::to_value(active_issue).unwrap_or(Value::Null),
        );
        object.insert("managedByPlugin".into(), Value::Null);
    }
    Ok(Json(detail))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateBody {
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    project_id: Option<Uuid>,
    #[serde(default)]
    folder_id: Option<Uuid>,
    #[serde(default)]
    goal_id: Option<Uuid>,
    #[serde(default)]
    parent_issue_id: Option<Uuid>,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    assignee_agent_id: Option<Uuid>,
    #[serde(default = "default_priority")]
    priority: String,
    #[serde(default = "default_routine_status")]
    status: String,
    #[serde(default = "default_concurrency_policy")]
    concurrency_policy: String,
    #[serde(default = "default_catch_up_policy")]
    catch_up_policy: String,
    #[serde(default = "default_activity_gate_policy")]
    activity_gate_policy: String,
    #[serde(default = "default_activity_gate_scope")]
    activity_gate_scope: String,
    #[serde(default = "default_variables")]
    variables: Value,
    #[serde(default)]
    env: Option<Value>,
}

fn default_priority() -> String {
    "medium".into()
}
fn default_routine_status() -> String {
    "active".into()
}
fn default_concurrency_policy() -> String {
    "coalesce_if_active".into()
}
fn default_catch_up_policy() -> String {
    "skip_missed".into()
}
fn default_activity_gate_policy() -> String {
    "always".into()
}
fn default_activity_gate_scope() -> String {
    "company".into()
}
fn default_variables() -> Value {
    json!([])
}

async fn create_company(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    headers: axum::http::HeaderMap,
    Json(mut body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    body.company_id = Some(company_id);
    create_routine(state, actor, headers, body).await
}

async fn create(
    State(state): State<AppState>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    create_routine(state, actor, headers, body).await
}

async fn create_routine(
    state: AppState,
    actor: AuthContext,
    headers: axum::http::HeaderMap,
    body: CreateBody,
) -> ApiResult<impl IntoResponse> {
    let company_id = body
        .company_id
        .ok_or_else(|| ApiError::BadRequest("companyId is required".into()))?;
    // pc-authz：创建 routine 需要 PipelinesWrite 权限（pipeline 跟 routine 是同一组管理）
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        company_id,
        PermissionKey::PipelinesWrite,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    let title = body.title.trim();
    if title.is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let created_by_user_id = headers
        .get("x-paperclip-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let row = RoutineRepo::new(&state.db)
        .create_with_initial_revision(&CreateRoutineRecord {
            company_id,
            project_id: body.project_id,
            folder_id: body.folder_id,
            goal_id: body.goal_id,
            parent_issue_id: body.parent_issue_id,
            title: title.to_owned(),
            description: body.description,
            assignee_agent_id: body.assignee_agent_id,
            priority: body.priority,
            status: body.status,
            concurrency_policy: body.concurrency_policy,
            catch_up_policy: body.catch_up_policy,
            activity_gate_policy: body.activity_gate_policy,
            activity_gate_scope: body.activity_gate_scope,
            variables: body.variables,
            env: body.env,
            created_by_agent_id: None,
            created_by_user_id: created_by_user_id.clone(),
            responsible_user_id: created_by_user_id,
            created_by_run_id: None,
        })
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("routine.created", "routine", row.id).with_company(row.company_id));
    global::track("routine.created", BTreeMap::from([
        ("company_id".into(), serde_json::json!(row.company_id.to_string())),
        ("routine_id".into(), serde_json::json!(row.id.to_string())),
    ]));
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CompanyListQuery {
    #[serde(default)]
    project_id: Option<Uuid>,
}

async fn list_company(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<CompanyListQuery>,
) -> ApiResult<Json<Value>> {
    let repo = RoutineRepo::new(&state.db);
    let rows = repo
        .list_by_company_filtered(company_id, query.project_id)
        .await?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let triggers = repo.list_triggers(row.id).await?;
        let last_run = repo.list_run_summaries(row.id, 1).await?.into_iter().next();
        let active_issue = repo.get_active_issue(row.id).await?;
        let mut item = serde_json::to_value(row).unwrap_or_default();
        if let Some(object) = item.as_object_mut() {
            object.insert(
                "triggers".into(),
                serde_json::to_value(triggers).unwrap_or_else(|_| json!([])),
            );
            object.insert(
                "lastRun".into(),
                serde_json::to_value(last_run).unwrap_or(Value::Null),
            );
            object.insert(
                "activeIssue".into(),
                serde_json::to_value(active_issue).unwrap_or(Value::Null),
            );
            object.insert("managedByPlugin".into(), Value::Null);
        }
        items.push(item);
    }
    Ok(Json(Value::Array(items)))
}

fn deserialize_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct UpdateBody {
    #[serde(default, deserialize_with = "deserialize_nullable")]
    project_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    folder_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    goal_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    parent_issue_id: Option<Option<Uuid>>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    description: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    assignee_agent_id: Option<Option<Uuid>>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    concurrency_policy: Option<String>,
    #[serde(default)]
    catch_up_policy: Option<String>,
    #[serde(default)]
    activity_gate_policy: Option<String>,
    #[serde(default)]
    activity_gate_scope: Option<String>,
    #[serde(default)]
    variables: Option<Value>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    env: Option<Option<Value>>,
    #[serde(default)]
    base_revision_id: Option<Uuid>,
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    if body
        .title
        .as_deref()
        .is_some_and(|title| title.trim().is_empty())
    {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let user_id = headers
        .get("x-paperclip-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let _base_revision_id = body.base_revision_id;
    let row = RoutineRepo::new(&state.db)
        .update_with_revision(
            id,
            &UpdateRoutineRecord {
                project_id: body.project_id,
                folder_id: body.folder_id,
                goal_id: body.goal_id,
                parent_issue_id: body.parent_issue_id,
                title: body.title.map(|title| title.trim().to_owned()),
                description: body.description,
                assignee_agent_id: body.assignee_agent_id,
                priority: body.priority,
                status: body.status,
                concurrency_policy: body.concurrency_policy,
                catch_up_policy: body.catch_up_policy,
                activity_gate_policy: body.activity_gate_policy,
                activity_gate_scope: body.activity_gate_scope,
                variables: body.variables,
                env: body.env,
                updated_by_agent_id: None,
                updated_by_user_id: user_id,
                created_by_run_id: None,
            },
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("routine.updated", "routine", row.id).with_company(row.company_id));
    global::track("routine.updated", BTreeMap::from([
        ("company_id".into(), serde_json::json!(row.company_id.to_string())),
        ("routine_id".into(), serde_json::json!(row.id.to_string())),
    ]));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn trigger(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = RoutineRepo::new(&state.db)
        .trigger(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {id}")))?;
    state.realtime.publish(
        LiveEvent::new("routine.triggered", "routine", row.id).with_company(row.company_id),
    );
    Ok(Json(json!({
        "id": row.id, "last_triggered_at": row.last_triggered_at, "last_enqueued_at": row.last_enqueued_at
    })))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = RoutineRepo::new(&state.db).delete(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("routine {id}")))
    }
}

// ============================================================================
// Revisions
// ============================================================================

async fn list_revisions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = RoutineRepo::new(&state.db).list_revisions(id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateRevisionBody {
    title: String,
    #[serde(default)]
    description: Option<String>,
    /// 完整快照（json）
    snapshot: serde_json::Value,
    #[serde(default)]
    change_summary: Option<String>,
    #[serde(default)]
    created_by_user_id: Option<String>,
}

// Round 112: 仓储化。RoutineRepo::update_revision_pointer。
async fn create_revision(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateRevisionBody>,
) -> ApiResult<impl IntoResponse> {
    let routine = RoutineRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {id}")))?;
    let next = routine.latest_revision_number + 1;
    let row = RoutineRepo::new(&state.db)
        .create_revision(
            routine.company_id,
            id,
            next,
            &body.title,
            body.description.as_deref(),
            &body.snapshot,
            body.change_summary.as_deref(),
            body.created_by_user_id.as_deref(),
        )
        .await?;
    let n = RoutineRepo::new(&state.db)
        .update_revision_pointer(
            id,
            row.id,
            row.revision_number,
            &row.title,
            row.description.as_deref(),
        )
        .await?;
    if n == 0 {
        return Err(ApiError::NotFound(format!("routine {id}")));
    }
    state.realtime.publish(LiveEvent::new(
        "routine.revision.created",
        "routine_revision",
        row.id,
    ));
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

async fn restore_revision(
    State(state): State<AppState>,
    Path((id, revision_id)): Path<(Uuid, Uuid)>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user = headers
        .get("x-paperclip-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let result = RoutineRepo::new(&state.db)
        .restore_revision_by_id(id, revision_id, None, user.as_deref(), None)
        .await?;
    state.realtime.publish(
        LiveEvent::new(
            "routine.revision.restored",
            "routine_revision",
            result.revision.id,
        )
        .with_company(result.routine.company_id),
    );
    Ok(Json(serde_json::to_value(result).unwrap_or_default()))
}

// ============================================================================
// Runs
// ============================================================================

#[derive(Debug, Deserialize, Default)]
struct ListRunsQuery {
    #[serde(default = "default_runs_limit")]
    limit: i64,
}
fn default_runs_limit() -> i64 {
    50
}

async fn list_runs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<ListRunsQuery>,
) -> ApiResult<Json<Value>> {
    let rows = RoutineRepo::new(&state.db)
        .list_run_summaries(id, q.limit.clamp(1, 200))
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunRoutineBody {
    #[serde(default)]
    trigger_id: Option<Uuid>,
    #[serde(default = "default_run_source")]
    source: String,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    variables: Option<Value>,
    #[serde(default)]
    project_id: Option<Uuid>,
    #[serde(default)]
    project_workspace_id: Option<Uuid>,
    #[serde(default)]
    assignee_agent_id: Option<Uuid>,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default)]
    execution_workspace_id: Option<Uuid>,
    #[serde(default)]
    execution_workspace_preference: Option<String>,
    #[serde(default)]
    execution_workspace_settings: Option<Value>,
}

async fn run_routine(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RunRoutineBody>,
) -> ApiResult<impl IntoResponse> {
    if !matches!(body.source.as_str(), "manual" | "api") {
        return Err(ApiError::BadRequest("source must be manual or api".into()));
    }
    let actor_user_id = headers
        .get("x-paperclip-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let dispatched = RoutineRepo::new(&state.db)
        .dispatch_run(
            id,
            &RunRoutineRecord {
                trigger_id: body.trigger_id,
                source: body.source,
                payload: body.payload,
                variables: body.variables,
                project_id: body.project_id,
                project_workspace_id: body.project_workspace_id,
                assignee_agent_id: body.assignee_agent_id,
                idempotency_key: body.idempotency_key,
                execution_workspace_id: body.execution_workspace_id,
                execution_workspace_preference: body.execution_workspace_preference,
                execution_workspace_settings: body.execution_workspace_settings,
                actor_agent_id: None,
                actor_user_id,
            },
        )
        .await?;
    if !dispatched.heartbeat_run_id.is_nil() {
        let _ = state
            .heartbeat
            .ask(pc_heartbeat::StartHeartbeat {
                run_id: dispatched.heartbeat_run_id,
            })
            .await;
    }
    state.realtime.publish(
        LiveEvent::new("routine.run.triggered", "routine_run", dispatched.run.id)
            .with_company(dispatched.run.company_id),
    );
    global::track("routine.run.triggered", BTreeMap::from([
        ("company_id".into(), serde_json::json!(dispatched.run.company_id.to_string())),
        ("run_id".into(), serde_json::json!(dispatched.run.id.to_string())),
    ]));
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(dispatched.run).unwrap_or_default()),
    ))
}

#[derive(Debug, Deserialize)]
struct CreateRunBody {
    #[serde(default)]
    trigger_id: Option<Uuid>,
    #[serde(default = "default_run_source")]
    source: String,
    #[serde(default)]
    trigger_payload: Option<serde_json::Value>,
}
fn default_run_source() -> String {
    "manual".into()
}

async fn create_run(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateRunBody>,
) -> ApiResult<impl IntoResponse> {
    let routine = RoutineRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {id}")))?;
    let row = RoutineRepo::new(&state.db)
        .create_run(
            routine.company_id,
            id,
            body.trigger_id,
            &body.source,
            body.trigger_payload.as_ref(),
        )
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("routine.run.created", "routine_run", row.id));
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

// ============================================================================
// Triggers
// ============================================================================

async fn list_triggers(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = RoutineRepo::new(&state.db).list_triggers(id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_trigger(
    State(state): State<AppState>,
    Path(trigger_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = RoutineRepo::new(&state.db)
        .get_trigger(trigger_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("trigger {trigger_id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTriggerBody {
    kind: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    cron_expression: Option<String>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    signing_mode: Option<String>,
    #[serde(default)]
    replay_window_sec: Option<i32>,
}

fn default_true() -> bool {
    true
}

async fn create_trigger(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateTriggerBody>,
) -> ApiResult<impl IntoResponse> {
    let actor_user_id = headers
        .get("x-paperclip-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    if body.kind == "webhook" {
        let provider = LocalEncryptedProvider::load()
            .map_err(|error| ApiError::Internal(format!("load secrets provider: {error}")))?;
        let api_base_url = std::env::var("PAPERCLIP_PUBLIC_URL")
            .unwrap_or_else(|_| format!("http://{}:{}", state.config.host, state.config.port));
        let input = CreateWebhookSecretInput {
            kind: body.kind.clone(),
            label: body.label.clone(),
            enabled: body.enabled,
            signing_mode: body.signing_mode.clone(),
            replay_window_sec: Some(body.replay_window_sec.unwrap_or(300)),
            api_base_url,
            agent_id: None,
            user_id: actor_user_id.clone(),
            run_id: None,
        };
        let result = RoutineRepo::new(&state.db)
            .create_webhook_trigger(id, &input, &provider, &provider)
            .await?;
        state.realtime.publish(
            LiveEvent::new(
                "routine.trigger.created",
                "routine_trigger",
                result.trigger.id,
            )
            .with_company(result.trigger.company_id),
        );
        return Ok((
            StatusCode::CREATED,
            Json(serde_json::to_value(result).unwrap_or_default()),
        ));
    }

    let (cron_expression, timezone, next_run_at, signing_mode, replay_window_sec, public_id) =
        match body.kind.as_str() {
            "schedule" => {
                let cron_expression = body.cron_expression.ok_or_else(|| {
                    ApiError::Unprocessable("Scheduled triggers require cronExpression".into())
                })?;
                let timezone = body.timezone.unwrap_or_else(|| "UTC".into());
                if timezone != "UTC" {
                    return Err(ApiError::Unprocessable(format!(
                        "unsupported timezone: {timezone}"
                    )));
                }
                let schedule = cron_expression
                    .parse::<pc_workflow::schedule::ScheduleSpec>()
                    .map_err(|error| ApiError::Unprocessable(error.to_string()))?;
                let next_run_at = schedule
                    .next_after(chrono::Utc::now())
                    .ok_or_else(|| ApiError::Unprocessable("cron has no next occurrence".into()))?;
                (
                    Some(cron_expression),
                    Some(timezone),
                    Some(pc_core::Timestamp::from_dt(next_run_at)),
                    None,
                    None,
                    None,
                )
            }
            "api" => (None, None, None, None, None, None),
            other => {
                return Err(ApiError::BadRequest(format!(
                    "unsupported routine trigger kind: {other}"
                )));
            }
        };
    let result = RoutineRepo::new(&state.db)
        .create_trigger_with_revision(
            id,
            &CreateRoutineTriggerRecord {
                kind: body.kind,
                label: body.label,
                enabled: body.enabled,
                cron_expression,
                timezone,
                next_run_at,
                public_id,
                secret_id: None,
                signing_mode,
                replay_window_sec,
                actor_agent_id: None,
                actor_user_id,
                actor_run_id: None,
            },
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new(
            "routine.trigger.created",
            "routine_trigger",
            result.trigger.id,
        )
        .with_company(result.trigger.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(result).unwrap_or_default()),
    ))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct UpdateTriggerBody {
    #[serde(default, deserialize_with = "deserialize_nullable")]
    label: Option<Option<String>>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    cron_expression: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    timezone: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    signing_mode: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    replay_window_sec: Option<Option<i32>>,
}

async fn update_trigger(
    State(state): State<AppState>,
    Path(trigger_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<UpdateTriggerBody>,
) -> ApiResult<Json<Value>> {
    let repo = RoutineRepo::new(&state.db);
    let existing = repo
        .get_trigger(trigger_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("trigger {trigger_id}")))?;
    let next_run_at = if existing.kind == "schedule" {
        let cron_expression = body
            .cron_expression
            .clone()
            .unwrap_or_else(|| Some(existing.cron_expression.clone().unwrap_or_default()))
            .ok_or_else(|| {
                ApiError::Unprocessable("Scheduled triggers require cronExpression".into())
            })?;
        let timezone = body
            .timezone
            .clone()
            .unwrap_or_else(|| Some(existing.timezone.clone().unwrap_or_else(|| "UTC".into())))
            .ok_or_else(|| ApiError::Unprocessable("Scheduled triggers require timezone".into()))?;
        if timezone != "UTC" {
            return Err(ApiError::Unprocessable(format!(
                "unsupported timezone: {timezone}"
            )));
        }
        let schedule = cron_expression
            .parse::<pc_workflow::schedule::ScheduleSpec>()
            .map_err(|error| ApiError::Unprocessable(error.to_string()))?;
        Some(Some(pc_core::Timestamp::from_dt(
            schedule
                .next_after(chrono::Utc::now())
                .ok_or_else(|| ApiError::Unprocessable("cron has no next occurrence".into()))?,
        )))
    } else {
        None
    };
    if let Some(Some(value)) = body.replay_window_sec {
        if !(30..=86_400).contains(&value) {
            return Err(ApiError::BadRequest(
                "replayWindowSec must be between 30 and 86400".into(),
            ));
        }
    }
    let actor_user_id = headers
        .get("x-paperclip-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let result = repo
        .update_trigger_with_revision(
            trigger_id,
            &UpdateRoutineTriggerRecord {
                label: body.label,
                enabled: body.enabled,
                cron_expression: body.cron_expression,
                timezone: body.timezone,
                next_run_at,
                signing_mode: body.signing_mode,
                replay_window_sec: body.replay_window_sec,
                actor_agent_id: None,
                actor_user_id,
                actor_run_id: None,
            },
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("trigger {trigger_id}")))?;
    state.realtime.publish(
        LiveEvent::new(
            "routine.trigger.updated",
            "routine_trigger",
            result.trigger.id,
        )
        .with_company(result.trigger.company_id),
    );
    Ok(Json(
        serde_json::to_value(result.trigger).unwrap_or_default(),
    ))
}

async fn remove_trigger(
    State(state): State<AppState>,
    Path(trigger_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<StatusCode> {
    let actor_user_id = headers
        .get("x-paperclip-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let revision = RoutineRepo::new(&state.db)
        .delete_trigger_with_revision(trigger_id, None, actor_user_id.as_deref(), None)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("trigger {trigger_id}")))?;
    state.realtime.publish(LiveEvent::new(
        "routine.trigger.deleted",
        "routine_revision",
        revision.id,
    ));
    Ok(StatusCode::NO_CONTENT)
}

async fn fire_public_trigger(
    State(state): State<AppState>,
    Path(public_id): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<impl IntoResponse> {
    let authorization_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let signature_header = headers
        .get("x-signature")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let hub_signature_header = headers
        .get("x-hub-signature-256")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let timestamp_header = headers
        .get("x-timestamp")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let raw_body = body.to_vec();
    let payload = if raw_body.is_empty() {
        None
    } else {
        Some(
            serde_json::from_slice::<serde_json::Value>(&raw_body).unwrap_or(
                serde_json::Value::String(String::from_utf8_lossy(&raw_body).into_owned()),
            ),
        )
    };
    let provider = LocalEncryptedProvider::load()
        .map_err(|error| ApiError::Internal(format!("load secrets provider: {error}")))?;
    let input = FireTriggerInput {
        authorization_header,
        signature_header,
        hub_signature_header,
        timestamp_header,
        idempotency_key,
        raw_body: Some(raw_body),
        payload,
        agent_id: None,
        user_id: None,
        run_id: None,
    };
    let fired = RoutineRepo::new(&state.db)
        .fire_public_trigger(&public_id, &input, &provider)
        .await?;
    state.realtime.publish(LiveEvent::new(
        "routine.trigger.fired",
        "routine_trigger",
        fired.run.trigger_id.unwrap_or(Uuid::nil()),
    ));
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(fired.run).unwrap_or_default()),
    ))
}

// ============== Round 111: routine description annotations 仓储化 ==============
// types `RoutineAnnotationThreadRow` / `RoutineAnnotationCommentRow` 已迁移到 pc-repos::routine。
// route 改用 `RoutineRepo::list_annotation_threads` / `get_annotation_thread` / etc.

/// routine description uses literal "description" as the document_key in
/// document_annotation_threads/comments. The routine_id column is set on the
/// thread and comment rows.

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRoutineAnnotationsQuery {
    status: Option<String>,
    include_comments: Option<bool>,
}

// Round 111: 仓储化。RoutineRepo::get_company_id + list_annotation_threads +
//             list_thread_comments_bulk。
async fn list_routine_description_annotations(
    State(state): State<AppState>,
    Path(routine_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<ListRoutineAnnotationsQuery>,
) -> ApiResult<Json<Value>> {
    let repo = RoutineRepo::new(&state.db);
    let _company_id = repo
        .get_company_id(routine_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {routine_id}")))?;
    let status_filter = q.status.as_deref().and_then(|s| {
        if s == "open" || s == "resolved" {
            Some(s)
        } else {
            None
        }
    });
    let rows = repo
        .list_annotation_threads(routine_id, status_filter, 200)
        .await?;
    let include_comments = q.include_comments.unwrap_or(false);
    let mut items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "documentId": r.document_id,
                "documentKey": "description",
                "status": r.status,
                "anchorState": r.anchor_state,
                "originalRevisionNumber": r.original_revision_number,
                "currentRevisionNumber": r.current_revision_number,
                "selectedText": r.selected_text,
                "prefixText": r.prefix_text,
                "suffixText": r.suffix_text,
                "normalizedStart": r.normalized_start,
                "normalizedEnd": r.normalized_end,
                "markdownStart": r.markdown_start,
                "markdownEnd": r.markdown_end,
                "anchorConfidence": r.anchor_confidence,
                "anchorSelector": r.anchor_selector,
                "resolvedAt": r.resolved_at,
                "resolvedByUserId": r.resolved_by_user_id,
                "resolvedByAgentId": r.resolved_by_agent_id,
                "createdByUserId": r.created_by_user_id,
                "createdByAgentId": r.created_by_agent_id,
                "createdAt": r.created_at,
                "updatedAt": r.updated_at,
            })
        })
        .collect();
    if include_comments {
        let thread_ids: Vec<Uuid> = items
            .iter()
            .filter_map(|v| {
                v.get("id")
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok())
            })
            .collect();
        if !thread_ids.is_empty() {
            let comments = repo.list_thread_comments_bulk(&thread_ids).await?;
            for t in items.iter_mut() {
                let tid = t
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok());
                let cs: Vec<Value> = comments
                    .iter()
                    .filter(|c| Some(c.thread_id) == tid)
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "body": c.body,
                            "authorType": c.author_type,
                            "authorAgentId": c.author_agent_id,
                            "authorUserId": c.author_user_id,
                            "createdAt": c.created_at,
                        })
                    })
                    .collect();
                t["comments"] = json!(cs);
            }
        }
    }
    Ok(Json(json!({
        "routineId": routine_id,
        "documentKey": "description",
        "threads": items,
        "items": items,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateRoutineAnnotationBody {
    selected_text: String,
    prefix_text: Option<String>,
    suffix_text: Option<String>,
    normalized_start: Option<i32>,
    normalized_end: Option<i32>,
    markdown_start: Option<i32>,
    markdown_end: Option<i32>,
    anchor_confidence: Option<String>,
    anchor_selector: Option<Value>,
    body: Option<String>,
    revision_number: Option<i32>,
    document_id: Option<Uuid>,
    status: Option<String>,
}

// Round 111: 仓储化。RoutineRepo::get_company_id + create_annotation_thread +
//             create_thread_comment。
async fn create_routine_description_annotation(
    State(state): State<AppState>,
    Path(routine_id): Path<Uuid>,
    Json(body): Json<CreateRoutineAnnotationBody>,
) -> ApiResult<impl IntoResponse> {
    if body.selected_text.is_empty() {
        return Err(ApiError::BadRequest("selectedText is required".into()));
    }
    let repo = RoutineRepo::new(&state.db);
    let company_id = repo
        .get_company_id(routine_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {routine_id}")))?;
    let document_id = body
        .document_id
        .ok_or_else(|| ApiError::BadRequest("documentId is required".into()))?;
    let norm_start = body.normalized_start.unwrap_or(0);
    let norm_end = body
        .normalized_end
        .unwrap_or(body.selected_text.len() as i32);
    let md_start = body.markdown_start.unwrap_or(0);
    let md_end = body.markdown_end.unwrap_or(body.selected_text.len() as i32);
    let confidence = body
        .anchor_confidence
        .clone()
        .unwrap_or_else(|| "exact".to_owned());
    let selector = body.anchor_selector.clone().unwrap_or_else(|| json!({}));
    let revision_number = body.revision_number.unwrap_or(1);
    let input = NewRoutineAnnotationThread {
        company_id,
        routine_id,
        document_id,
        document_key: "description".to_owned(),
        status: body.status.clone(),
        revision_number,
        selected_text: body.selected_text.clone(),
        prefix_text: body.prefix_text.clone(),
        suffix_text: body.suffix_text.clone(),
        normalized_start: norm_start,
        normalized_end: norm_end,
        markdown_start: md_start,
        markdown_end: md_end,
        anchor_confidence: Some(confidence.clone()),
        anchor_selector: Some(selector.clone()),
    };
    let thread_id = repo.create_annotation_thread(&input).await?;
    if let Some(initial_body) = body.body.as_deref() {
        if !initial_body.is_empty() {
            let comment = NewRoutineAnnotationComment {
                company_id,
                routine_id,
                thread_id,
                document_id,
                body: initial_body.to_owned(),
                author_type: "user".to_owned(),
                author_user_id: None,
                author_agent_id: None,
            };
            repo.create_thread_comment(&comment).await?;
        }
    }
    state.realtime.publish(
        LiveEvent::new(
            "routine.annotation.created",
            "routine_annotation",
            thread_id,
        )
        .with_company(company_id)
        .with_data(json!({"routineId": routine_id})),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": thread_id,
            "routineId": routine_id,
            "documentKey": "description",
            "status": body.status.unwrap_or_else(|| "open".to_owned()),
            "selectedText": body.selected_text,
            "anchorConfidence": confidence,
            "anchorSelector": selector,
        })),
    ))
}

// Round 111: 仓储化。RoutineRepo::get_annotation_thread + list_thread_comments。
async fn get_routine_description_annotation(
    State(state): State<AppState>,
    Path((routine_id, thread_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let repo = RoutineRepo::new(&state.db);
    let _company_id = repo
        .get_company_id(routine_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {routine_id}")))?;
    let row = repo
        .get_annotation_thread(routine_id, thread_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("annotation thread {thread_id}")))?;
    let comments = repo.list_thread_comments(thread_id).await?;
    let comment_items: Vec<Value> = comments
        .into_iter()
        .map(|c| {
            json!({
                "id": c.id,
                "body": c.body,
                "authorType": c.author_type,
                "authorAgentId": c.author_agent_id,
                "authorUserId": c.author_user_id,
                "createdAt": c.created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "id": row.id,
        "routineId": routine_id,
        "documentId": row.document_id,
        "documentKey": "description",
        "status": row.status,
        "anchorConfidence": row.anchor_confidence,
        "normalizedStart": row.normalized_start,
        "normalizedEnd": row.normalized_end,
        "selectedText": row.selected_text,
        "anchorSelector": row.anchor_selector,
        "resolvedAt": row.resolved_at,
        "resolvedByAgentId": row.resolved_by_agent_id,
        "resolvedByUserId": row.resolved_by_user_id,
        "createdAt": row.created_at,
        "comments": comment_items,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchRoutineAnnotationBody {
    status: Option<String>,
    anchor_selector: Option<Value>,
    anchor_state: Option<String>,
    current_revision_id: Option<Uuid>,
    current_revision_number: Option<i32>,
}

// Round 111: 仓储化。RoutineRepo::get_company_id + update_annotation_thread。
async fn patch_routine_description_annotation(
    State(state): State<AppState>,
    Path((routine_id, thread_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchRoutineAnnotationBody>,
) -> ApiResult<Json<Value>> {
    let repo = RoutineRepo::new(&state.db);
    let company_id = repo
        .get_company_id(routine_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {routine_id}")))?;
    if let Some(s) = body.status.as_deref() {
        if !matches!(s, "open" | "resolved" | "outdated") {
            return Err(ApiError::BadRequest(format!("invalid status '{s}'")));
        }
    }
    let patch = RoutineAnnotationPatch {
        status: body.status.clone(),
        anchor_selector: body.anchor_selector.clone(),
        anchor_state: body.anchor_state.clone(),
        current_revision_id: body.current_revision_id,
        current_revision_number: body.current_revision_number,
    };
    let affected = repo
        .update_annotation_thread(routine_id, thread_id, &patch)
        .await?;
    if affected == 0 {
        return Err(ApiError::NotFound(format!("annotation thread {thread_id}")));
    }
    state.realtime.publish(
        LiveEvent::new(
            "routine.annotation.updated",
            "routine_annotation",
            thread_id,
        )
        .with_company(company_id)
        .with_data(json!({"routineId": routine_id, "status": body.status})),
    );
    Ok(Json(json!({
        "id": thread_id,
        "routineId": routine_id,
        "updated": true,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddRoutineAnnotationCommentBody {
    body: String,
    author_type: Option<String>,
    author_user_id: Option<String>,
    author_agent_id: Option<Uuid>,
}

// Round 111: 仓储化。RoutineRepo::get_company_id + get_thread_document_id +
//             create_thread_comment。
async fn add_routine_description_annotation_comment(
    State(state): State<AppState>,
    Path((routine_id, thread_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<AddRoutineAnnotationCommentBody>,
) -> ApiResult<impl IntoResponse> {
    if body.body.trim().is_empty() {
        return Err(ApiError::BadRequest("body is required".into()));
    }
    let repo = RoutineRepo::new(&state.db);
    let company_id = repo
        .get_company_id(routine_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {routine_id}")))?;
    let document_id = repo
        .get_thread_document_id(routine_id, thread_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("annotation thread {thread_id}")))?;
    let author_type = body
        .author_type
        .clone()
        .unwrap_or_else(|| "user".to_owned());
    let input = NewRoutineAnnotationComment {
        company_id,
        routine_id,
        thread_id,
        document_id,
        body: body.body.clone(),
        author_type: author_type.clone(),
        author_user_id: body.author_user_id.clone(),
        author_agent_id: body.author_agent_id,
    };
    let id = repo.create_thread_comment(&input).await?;
    state.realtime.publish(
        LiveEvent::new(
            "routine.annotation.comment_added",
            "routine_annotation_comment",
            id,
        )
        .with_company(company_id)
        .with_data(json!({"threadId": thread_id, "routineId": routine_id})),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "threadId": thread_id,
            "routineId": routine_id,
            "body": body.body,
            "authorType": author_type,
            "authorUserId": body.author_user_id,
            "authorAgentId": body.author_agent_id,
            "createdAt": chrono::Utc::now(),
        })),
    ))
}

// ── Trigger secret rotation ─────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RotateTriggerSecretBody {
    reason: Option<String>,
}

async fn rotate_trigger_secret_route(
    State(state): State<AppState>,
    Path(trigger_id): Path<Uuid>,
    Json(body): Json<RotateTriggerSecretBody>,
) -> ApiResult<Json<Value>> {
    // Round 112: 仓储化。RoutineRepo::get_trigger_for_rotation。
    let repo = RoutineRepo::new(&state.db);
    let info = repo
        .get_trigger_for_rotation(trigger_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine trigger {trigger_id}")))?;
    let company_id = info.company_id;
    let _routine_id = info.routine_id;
    // Generate a new secret via the secrets provider if available; otherwise use a local UUID
    let provider = LocalEncryptedProvider::load()
        .map_err(|error| ApiError::Internal(format!("load secrets provider: {error}")))?;
    let new_secret_value = format!("sk_{}", Uuid::new_v4().simple());
    let write_ctx = pc_secrets::provider::SecretProviderWriteContext {
        company_id,
        secret_key: format!("routine_trigger:{}", trigger_id),
        secret_name: format!("rotated_{}", trigger_id.simple()),
        version: 1,
    };
    let secret_ref = match provider
        .create_secret(new_secret_value.clone(), &write_ctx)
        .await
    {
        Ok(prepared) => prepared
            .external_ref
            .unwrap_or_else(|| format!("local://rotated/{}", Uuid::new_v4().simple())),
        Err(_) => format!("local://rotated/{}", Uuid::new_v4().simple()),
    };
    // Round 112: 仓储化。RoutineRepo::set_trigger_secret_ref。
    let n = repo
        .set_trigger_secret_ref(trigger_id, &secret_ref, body.reason.as_deref())
        .await?;
    if n == 0 {
        return Err(ApiError::NotFound(format!("routine trigger {trigger_id}")));
    }
    state.realtime.publish(
        LiveEvent::new(
            "routine_trigger.secret_rotated",
            "routine_trigger",
            trigger_id,
        )
        .with_company(company_id)
        .with_data(json!({"reason": body.reason})),
    );
    Ok(Json(json!({
        "id": trigger_id,
        "secretRef": secret_ref,
        "rotatedAt": chrono::Utc::now(),
    })))
}
