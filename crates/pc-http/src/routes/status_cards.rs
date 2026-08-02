//! 状态卡片 (dashboard widget)。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/status-cards",
            get(list_status_cards).post(create_status_card),
        )
        .route(
            "/api/status-cards/:id",
            get(get_status_card)
                .patch(patch_status_card)
                .delete(delete_status_card),
        )
        .route("/api/status-cards/:id/updates", get(card_updates))
        .route(
            "/api/status-cards/:id/summary-revisions",
            get(card_summary_revisions),
        )
        .route("/api/status-cards/:id/recompile", post(card_recompile))
        .route("/api/status-cards/:id/refresh", post(card_refresh))
        .route("/api/status-cards/:id/dry-run", get(card_dry_run))
        .route("/api/status-cards/:id/query", put(card_query))
        .route("/api/status-cards/:id/summary", put(card_summary))
}

#[derive(Debug, FromRow)]
struct CardRow {
    id: Uuid,
    company_id: Uuid,
    title: Option<String>,
    interest_prompt: String,
    state: String,
    queries: Value,
    refresh_policy: Value,
    last_generated_at: Option<pc_core::Timestamp>,
    next_eval_at: Option<pc_core::Timestamp>,
    archived_at: Option<pc_core::Timestamp>,
    document_id: Option<Uuid>,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn row_json(row: &CardRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "title": row.title,
        "interestPrompt": row.interest_prompt,
        "state": row.state,
        "queries": row.queries,
        "refreshPolicy": row.refresh_policy,
        "lastGeneratedAt": row.last_generated_at,
        "nextEvalAt": row.next_eval_at,
        "archivedAt": row.archived_at,
        "documentId": row.document_id,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct CreateBody {
    title: Option<String>,
    interest_prompt: Option<String>,
    refresh_policy: Option<Value>,
    queries: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct PatchBody {
    title: Option<String>,
    interest_prompt: Option<String>,
    refresh_policy: Option<Value>,
    archived: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct QueryBody {
    queries: Value,
    version: Option<i32>,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct SummaryBody {
    body: String,
    model: Option<String>,
}

async fn list_status_cards(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<CardRow> = sqlx::query_as(
        "SELECT id, company_id, title, interest_prompt, state, queries, refresh_policy, \
                last_generated_at, next_eval_at, archived_at, document_id, created_at, updated_at \
         FROM status_cards WHERE company_id = $1 AND archived_at IS NULL ORDER BY created_at DESC",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(row_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn get_status_card(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<CardRow> = sqlx::query_as(
        "SELECT id, company_id, title, interest_prompt, state, queries, refresh_policy, \
                last_generated_at, next_eval_at, archived_at, document_id, created_at, updated_at \
         FROM status_cards WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(row_json(&row))),
        None => Err(ApiError::NotFound(format!("status card {id}"))),
    }
}

async fn create_status_card(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    let prompt = body.interest_prompt.clone().unwrap_or_default();
    let title = body.title.clone();
    let refresh_policy = body.refresh_policy.clone().unwrap_or(json!({}));
    let queries = body.queries.clone().unwrap_or(json!([]));
    let row: CardRow = sqlx::query_as(
        "INSERT INTO status_cards (company_id, title, interest_prompt, queries, refresh_policy, state) \
         VALUES ($1, $2, $3, $4, $5, 'compiling') \
         RETURNING id, company_id, title, interest_prompt, state, queries, refresh_policy, \
                   last_generated_at, next_eval_at, archived_at, document_id, created_at, updated_at",
    )
    .bind(company_id)
    .bind(title)
    .bind(prompt)
    .bind(queries)
    .bind(refresh_policy)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::CREATED, Json(row_json(&row))))
}

async fn patch_status_card(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchBody>,
) -> ApiResult<Json<Value>> {
    let title = body.title.clone();
    let prompt = body.interest_prompt.clone();
    let refresh_policy = body.refresh_policy.clone();
    let archived = body.archived.unwrap_or(false);
    let row: CardRow = sqlx::query_as(
        "UPDATE status_cards SET \
            title = COALESCE($2, title), \
            interest_prompt = COALESCE($3, interest_prompt), \
            refresh_policy = COALESCE($4, refresh_policy), \
            archived_at = CASE WHEN $5 THEN now() ELSE archived_at END, \
            updated_at = now() \
         WHERE id = $1 \
         RETURNING id, company_id, title, interest_prompt, state, queries, refresh_policy, \
                   last_generated_at, next_eval_at, archived_at, document_id, created_at, updated_at",
    )
    .bind(id)
    .bind(title)
    .bind(prompt)
    .bind(refresh_policy)
    .bind(archived)
    .fetch_one(state.db.pool())
    .await?;
    Ok(Json(row_json(&row)))
}

async fn delete_status_card(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query("DELETE FROM status_cards WHERE id = $1")
        .bind(id)
        .execute(state.db.pool())
        .await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({}))))
}

async fn card_updates(State(_state): State<AppState>, Path(id): Path<Uuid>) -> Json<Value> {
    let _ = id;
    Json(json!({ "updates": [] }))
}

async fn card_summary_revisions(
    State(_state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Json<Value> {
    let _ = id;
    Json(json!({ "revisions": [] }))
}

async fn card_recompile(State(_state): State<AppState>, Path(id): Path<Uuid>) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({ "id": id, "status": "recompile-queued" })),
    )
}

async fn card_refresh(
    State(_state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({ "id": id, "status": "refresh-queued" })),
    )
}

async fn card_dry_run(State(_state): State<AppState>, Path(id): Path<Uuid>) -> Json<Value> {
    Json(json!({ "id": id, "preview": null, "warnings": [] }))
}

async fn card_query(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<QueryBody>,
) -> ApiResult<Json<Value>> {
    let version = body.version.unwrap_or(0) + 1;
    sqlx::query(
        "UPDATE status_cards SET queries = $2, query_version = $3, query_compiled_at = now(), updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&body.queries)
    .bind(version)
    .execute(state.db.pool())
    .await?;
    Ok(Json(
        json!({ "id": id, "version": version, "queries": body.queries }),
    ))
}

async fn card_summary(
    State(_state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SummaryBody>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "id": id,
            "model": body.model,
            "body": body.body,
            "savedAt": chrono::Utc::now()
        })),
    )
}
