//! `/api/cases*` 路由：CRUD。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_core::Timestamp;
use pc_realtime::LiveEvent;
use pc_repos::case::CaseRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/cases", get(list).post(create))
        .route("/api/cases/:case_id", get(get_one).patch(update).delete(remove))
        .route(
            "/api/companies/:company_id/cases",
            get(list_company_cases).post(create_company_case),
        )
        .route("/api/cases/:case_id/events", get(list_case_events))
        .route("/api/cases/:case_id/links", post(create_case_link))
        .route(
            "/api/cases/:case_id/documents",
            get(list_case_documents).put(upsert_case_document),
        )
        .route("/api/cases/:case_id/documents/:key", get(get_case_document))
        .route(
            "/api/cases/:case_id/documents/:key/lock",
            post(lock_case_document),
        )
        .route(
            "/api/cases/:case_id/documents/:key/unlock",
            post(unlock_case_document),
        )
        .route(
            "/api/cases/:case_id/documents/:key/annotations",
            get(list_case_annotations),
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
        Some(cid) => CaseRepo::new(&state.db).list_by_company(cid).await?,
        None => CaseRepo::new(&state.db).list_all(200).await?,
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(case_id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    company_id: Uuid,
    case_type: String,
    title: String,
    #[serde(default)]
    project_id: Option<Uuid>,
    #[serde(default)]
    summary: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let row = CaseRepo::new(&state.db)
        .create(
            body.company_id,
            &body.case_type,
            &body.title,
            body.project_id,
            body.summary.as_deref(),
        )
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("case.created", "case", row.id).with_company(row.company_id));
    let response = serde_json::json!({
            "id": row.id, "company_id": row.company_id, "title": row.title,
            "case_type": row.case_type, "status": row.status, "identifier": row.identifier
        });
    Ok((StatusCode::CREATED, Json(response)))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UpdateBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

async fn update(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let row = CaseRepo::new(&state.db)
        .update(
            case_id,
            body.title.as_deref(),
            body.summary.as_deref(),
            body.status.as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("case.updated", "case", row.id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(case_id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = CaseRepo::new(&state.db).delete(case_id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("case {case_id}")))
    }
}


// ============== Sub-resource handlers ==============

async fn list_company_cases(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = CaseRepo::new(&state.db)
        .list_by_company(company_id)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn create_company_case(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let row = CaseRepo::new(&state.db)
        .create(
            company_id,
            &body.case_type,
            &body.title,
            body.project_id,
            body.summary.as_deref(),
        )
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("case.created", "case", row.id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn list_case_events(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<EventsQuery>,
) -> ApiResult<Json<Value>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let rows: Vec<(Uuid, String, String, Option<String>, Option<Uuid>, Option<Uuid>, Value, Option<Timestamp>)> = sqlx::query_as(
        "SELECT id, kind, actor_type, actor_user_id, actor_agent_id, run_id, payload, created_at          FROM case_events WHERE case_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(case_id)
    .bind(limit)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, kind, actor_type, actor_user_id, actor_agent_id, run_id, payload, created_at)| {
            json!({
                "id": id,
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
    Ok(Json(json!({ "items": items })))
}

async fn create_case_link(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<CreateCaseLinkBody>,
) -> ApiResult<Json<Value>> {
    let case_row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let role = body.role.unwrap_or_else(|| "reference".to_string());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO case_issue_links (company_id, case_id, issue_id, role)          VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(case_row.company_id)
    .bind(case_id)
    .bind(body.issue_id)
    .bind(&role)
    .fetch_one(state.db.pool())
    .await?;
    sqlx::query(
        "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)          VALUES ($1, $2, 'issue_linked', 'user', jsonb_build_object('issueId',$3::text,'role',$4::text))",
    )
    .bind(case_row.company_id)
    .bind(case_id)
    .bind(body.issue_id.to_string())
    .bind(&role)
    .execute(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("case.issue_linked", "case", case_id)
            .with_company(case_row.company_id)
            .with_data(json!({"issueId": body.issue_id, "role": role})),
    );
    Ok(Json(json!({ "id": id, "caseId": case_id, "issueId": body.issue_id, "role": role })))
}

async fn list_case_documents(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, Uuid, String, Option<Timestamp>)> = sqlx::query_as(
        "SELECT id, document_id, key, created_at FROM case_documents          WHERE case_id = $1 ORDER BY created_at DESC",
    )
    .bind(case_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, document_id, key, created_at)| {
            json!({"id": id, "documentId": document_id, "key": key, "createdAt": created_at})
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn upsert_case_document(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<UpsertCaseDocumentBody>,
) -> ApiResult<Json<Value>> {
    let case_row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO case_documents (company_id, case_id, document_id, key)          VALUES ($1, $2, $3, $4)          ON CONFLICT (case_id, key) DO UPDATE SET document_id = EXCLUDED.document_id, updated_at = now()          RETURNING id",
    )
    .bind(case_row.company_id)
    .bind(case_id)
    .bind(body.document_id)
    .bind(&body.key)
    .fetch_one(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("case.document.upserted", "case", case_id)
            .with_company(case_row.company_id),
    );
    Ok(Json(json!({"id": id, "caseId": case_id, "key": body.key, "documentId": body.document_id})))
}

async fn get_case_document(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, document_id FROM case_documents WHERE case_id = $1 AND key = $2",
    )
    .bind(case_id)
    .bind(&key)
    .fetch_optional(state.db.pool())
    .await?;
    let (id, document_id) = row.ok_or_else(|| ApiError::NotFound(format!("case document {key}")))?;
    Ok(Json(json!({"id": id, "caseId": case_id, "key": key, "documentId": document_id})))
}

async fn lock_case_document(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let case_row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let _: Uuid = sqlx::query_scalar(
        "UPDATE case_documents SET updated_at = now() WHERE case_id = $1 AND key = $2 RETURNING id",
    )
    .bind(case_id)
    .bind(&key)
    .fetch_one(state.db.pool())
    .await?;
    sqlx::query(
        "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)          VALUES ($1, $2, 'document_locked', 'user', jsonb_build_object('key',$3::text))",
    )
    .bind(case_row.company_id)
    .bind(case_id)
    .bind(&key)
    .execute(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("case.document.locked", "case", case_id)
            .with_company(case_row.company_id)
            .with_data(json!({"key": key})),
    );
    Ok(Json(json!({"locked": true, "caseId": case_id, "key": key})))
}

async fn unlock_case_document(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let case_row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    sqlx::query(
        "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)          VALUES ($1, $2, 'document_unlocked', 'user', jsonb_build_object('key',$3::text))",
    )
    .bind(case_row.company_id)
    .bind(case_id)
    .bind(&key)
    .execute(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("case.document.unlocked", "case", case_id)
            .with_company(case_row.company_id)
            .with_data(json!({"key": key})),
    );
    Ok(Json(json!({"unlocked": true, "caseId": case_id, "key": key})))
}

async fn list_case_annotations(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/cases/:id/documents/:key/annotations`. Annotations live
    // in the `document_annotations` table; we filter by case-bound document
    // and key. Empty array when no rows exist (UI tolerates this).
    let rows: Vec<(Uuid, String, Option<String>, Value)> = sqlx::query_as(
        "SELECT id, kind, thread_id, payload FROM document_annotations          WHERE document_id IN (SELECT document_id FROM case_documents WHERE case_id = $1 AND key = $2)          ORDER BY created_at DESC LIMIT 200",
    )
    .bind(case_id)
    .bind(&key)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, kind, thread_id, payload)| {
            json!({"id": id, "kind": kind, "threadId": thread_id, "payload": payload})
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CreateCaseLinkBody {
    issue_id: Uuid,
    #[serde(default)]
    role: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpsertCaseDocumentBody {
    document_id: Uuid,
    key: String,
}
