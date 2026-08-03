//! 决策训练示例（用于 fine-tune / 评估）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
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
            "/api/companies/:company_id/decision-training",
            get(list_training).post(create_training),
        )
        .route(
            "/api/companies/:company_id/decision-training/preview",
            get(preview_training),
        )
        .route(
            "/api/companies/:company_id/decision-training/export.jsonl",
            get(export_jsonl),
        )
        .route(
            "/api/decision-training/:id",
            get(get_training)
                .patch(patch_training)
                .delete(delete_training),
        )
}

#[derive(Debug, FromRow)]
struct TrainingRow {
    id: Uuid,
    company_id: Uuid,
    source_kind: String,
    source_id: Uuid,
    issue_id: Uuid,
    cutoff_at: pc_core::Timestamp,
    notes: String,
    notes_history: Value,
    decision_outcome: Option<String>,
    snapshot: Value,
    created_by_user_id: String,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn row_json(row: &TrainingRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "sourceKind": row.source_kind,
        "sourceId": row.source_id,
        "issueId": row.issue_id,
        "cutoffAt": row.cutoff_at,
        "notes": row.notes,
        "notesHistory": row.notes_history,
        "decisionOutcome": row.decision_outcome,
        "snapshot": row.snapshot,
        "createdByUserId": row.created_by_user_id,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct CreateBody {
    source_kind: Option<String>,
    source_id: Option<Uuid>,
    issue_id: Option<Uuid>,
    cutoff_at: Option<pc_core::Timestamp>,
    notes: Option<String>,
    decision_outcome: Option<String>,
    snapshot: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct UpdateBody {
    notes: Option<String>,
    decision_outcome: Option<String>,
}

async fn list_training(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<TrainingRow> = sqlx::query_as(
        "SELECT id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, notes_history, \
                decision_outcome, snapshot, created_by_user_id, created_at, updated_at \
         FROM decision_training_examples WHERE company_id = $1 ORDER BY created_at DESC",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(row_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn preview_training(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Count decisions that haven't been exported yet.
    let candidate_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM decisions WHERE company_id = $1          AND status = 'resolved' AND id NOT IN (SELECT source_id FROM decision_training_examples)",
    )
    .bind(company_id)
    .fetch_one(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "companyId": company_id,
        "candidateCount": candidate_count,
        "sources": [
            { "kind": "decision", "count": candidate_count }
        ]
    })))
}

async fn export_jsonl(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // Stream resolved decisions as JSONL training examples.
    let rows: Vec<(Uuid, String, serde_json::Value, Option<String>)> = sqlx::query_as(
        "SELECT id, title, payload, decision_outcome FROM decisions          WHERE company_id = $1 AND status = 'resolved'          ORDER BY created_at DESC LIMIT 1000",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut buf = String::new();
    for (id, title, payload, outcome) in rows {
        let example = json!({
            "id": id,
            "title": title,
            "input": payload,
            "output": outcome.unwrap_or_default(),
        });
        buf.push_str(&serde_json::to_string(&example).unwrap_or_default());
        buf.push_str("\n");
    }
    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/x-ndjson")],
        buf,
    ))
}

async fn get_training(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<TrainingRow> = sqlx::query_as(
        "SELECT id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, notes_history, \
                decision_outcome, snapshot, created_by_user_id, created_at, updated_at \
         FROM decision_training_examples WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(row_json(&row))),
        None => Err(ApiError::NotFound(format!("training example {id}"))),
    }
}

async fn create_training(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    let source_kind = body
        .source_kind
        .clone()
        .unwrap_or_else(|| "interaction".to_owned());
    let source_id = body.source_id.unwrap_or_else(Uuid::now_v7);
    let issue_id = body
        .issue_id
        .ok_or_else(|| ApiError::BadRequest("issue_id required".into()))?;
    let cutoff = body.cutoff_at.unwrap_or_else(pc_core::Timestamp::now);
    let notes = body.notes.clone().unwrap_or_default();
    let outcome = body.decision_outcome.clone();
    let snapshot = body.snapshot.clone().unwrap_or(json!({}));
    let row: TrainingRow = sqlx::query_as(
        "INSERT INTO decision_training_examples \
            (company_id, source_kind, source_id, issue_id, cutoff_at, notes, decision_outcome, snapshot, created_by_user_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'system') \
         RETURNING id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, notes_history, \
                   decision_outcome, snapshot, created_by_user_id, created_at, updated_at",
    )
    .bind(company_id)
    .bind(&source_kind)
    .bind(source_id)
    .bind(issue_id)
    .bind(cutoff)
    .bind(&notes)
    .bind(outcome)
    .bind(&snapshot)
    .fetch_one(state.db.pool())
    .await?;
    Ok(Json(row_json(&row)))
}

async fn patch_training(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let notes = body.notes.clone();
    let outcome = body.decision_outcome.clone();
    let row: TrainingRow = sqlx::query_as(
        "UPDATE decision_training_examples SET \
            notes = COALESCE($2, notes), \
            decision_outcome = COALESCE($3, decision_outcome), \
            notes_history = COALESCE(notes_history, '[]'::jsonb) || jsonb_build_array(jsonb_build_object('at', now(), 'notes', notes)), \
            updated_at = now() \
         WHERE id = $1 \
         RETURNING id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, notes_history, \
                   decision_outcome, snapshot, created_by_user_id, created_at, updated_at",
    )
    .bind(id)
    .bind(notes)
    .bind(outcome)
    .fetch_one(state.db.pool())
    .await?;
    Ok(Json(row_json(&row)))
}

async fn delete_training(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query("DELETE FROM decision_training_examples WHERE id = $1")
        .bind(id)
        .execute(state.db.pool())
        .await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({}))))
}
