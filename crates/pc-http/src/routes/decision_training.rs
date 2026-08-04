//! 决策训练示例（用于 fine-tune / 评估）。

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    q: Option<String>,
}

const ALLOWED_SOURCE_KINDS: &[&str] = &["interaction", "approval", "execution_decision"];

fn validate_source_kind(k: &str) -> Result<(), ApiError> {
    if ALLOWED_SOURCE_KINDS.contains(&k) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "invalid source_kind '{k}'; must be one of {ALLOWED_SOURCE_KINDS:?}"
        )))
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/decision-training",
            get(list_training).post(create_training),
        )
        // Round 33: preview 改 POST + body（与 Node 对齐）
        .route(
            "/api/companies/:company_id/decision-training/preview",
            post(preview_training_v2),
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
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let mut sql = String::from(
        "SELECT id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, notes_history, \
         decision_outcome, snapshot, created_by_user_id, created_at, updated_at \
         FROM decision_training_examples WHERE company_id = $1",
    );
    let mut idx = 2;
    if q.kind.is_some() { sql.push_str(&format!(" AND source_kind = ${idx}")); idx += 1; }
    if q.author.is_some() { sql.push_str(&format!(" AND created_by_user_id = ${idx}")); idx += 1; }
    if q.q.is_some() { sql.push_str(&format!(" AND notes ILIKE ${idx}")); idx += 1; }
    sql.push_str(" ORDER BY created_at DESC LIMIT 500");
    let mut query = sqlx::query_as::<_, TrainingRow>(&sql).bind(company_id);
    if let Some(k) = q.kind.as_ref() { query = query.bind(k); }
    if let Some(a) = q.author.as_ref() { query = query.bind(a); }
    if let Some(s) = q.q.as_ref() { query = query.bind(format!("%{s}%")); }
    let rows = query.fetch_all(state.db.pool()).await?;
    let items: Vec<Value> = rows.iter().map(row_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items, "count": items.len() })))
}

async fn preview_training_v2(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Json<Value>> {
    let source_kind = body.get("sourceKind").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::BadRequest("sourceKind required".into()))?;
    validate_source_kind(source_kind)?;
    let source_id_str = body.get("sourceId").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::BadRequest("sourceId required".into()))?;
    let source_id = uuid::Uuid::parse_str(source_id_str)
        .map_err(|_| ApiError::BadRequest("sourceId must be uuid".into()))?;
    let issue_id = body.get("issueId").and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let mut snapshot = json!({});
    let mut decision_outcome: Option<String> = None;
    match source_kind {
        "execution_decision" => {
            let row: Option<(String, Option<String>, serde_json::Value)> = sqlx::query_as(
                "SELECT status, decision_outcome, options FROM decisions WHERE company_id=$1 AND id=$2",
            ).bind(company_id).bind(source_id).fetch_optional(state.db.pool()).await?;
            if let Some((st, outcome, opts)) = row {
                snapshot = json!({"kind": "decision", "status": st, "options": opts});
                decision_outcome = outcome;
            }
        }
        "approval" => {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT status FROM approvals WHERE company_id=$1 AND id=$2",
            ).bind(company_id).bind(source_id).fetch_optional(state.db.pool()).await?;
            if let Some((st,)) = row {
                snapshot = json!({"kind": "approval", "status": st});
                decision_outcome = Some(st);
            }
        }
        "interaction" => {
            snapshot = json!({"kind": "interaction", "sourceId": source_id});
        }
        _ => {}
    }
    Ok(Json(json!({
        "companyId": company_id,
        "cutoffAt": chrono::Utc::now(),
        "decisionOutcome": decision_outcome,
        "snapshot": snapshot,
        "issueId": issue_id,
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
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    let source_kind = body
        .source_kind
        .clone()
        .unwrap_or_else(|| "interaction".to_owned());
    validate_source_kind(&source_kind)?;
    let source_id = body.source_id.unwrap_or_else(Uuid::now_v7);
    let issue_id = body
        .issue_id
        .ok_or_else(|| ApiError::BadRequest("issue_id required".into()))?;
    let cutoff = body.cutoff_at.unwrap_or_else(pc_core::Timestamp::now);
    let notes = body.notes.clone().unwrap_or_default();
    let outcome = body.decision_outcome.clone();
    let snapshot = body.snapshot.clone().unwrap_or(json!({}));
    let user_id = require_user_id(&state, &headers).await
        .unwrap_or_else(|_| "system".to_string());
    let row: TrainingRow = sqlx::query_as(
        "INSERT INTO decision_training_examples \
            (company_id, source_kind, source_id, issue_id, cutoff_at, notes, decision_outcome, snapshot, created_by_user_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
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
    .bind(&user_id)
    .fetch_one(state.db.pool())
    .await?;
    Ok(Json(row_json(&row)))
}

async fn patch_training(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let owner: Option<(String,)> = sqlx::query_as(
        "SELECT created_by_user_id FROM decision_training_examples WHERE id = $1",
    ).bind(id).fetch_optional(state.db.pool()).await?;
    let owner = owner.ok_or_else(|| ApiError::NotFound(format!("training example {id}")))?.0;
    let user_id = require_user_id(&state, &headers).await
        .map_err(|_| ApiError::Unauthorized("auth required to modify training example".into()))?;
    if owner != user_id && owner != "system" {
        return Err(ApiError::Forbidden("only the example author can modify this training example".into()));
    }
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
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let owner: Option<(String,)> = sqlx::query_as(
        "SELECT created_by_user_id FROM decision_training_examples WHERE id = $1",
    ).bind(id).fetch_optional(state.db.pool()).await?;
    let owner = owner.ok_or_else(|| ApiError::NotFound(format!("training example {id}")))?.0;
    let user_id = require_user_id(&state, &headers).await
        .map_err(|_| ApiError::Unauthorized("auth required to delete training example".into()))?;
    if owner != user_id && owner != "system" {
        return Err(ApiError::Forbidden("only the example author can delete this training example".into()));
    }
    sqlx::query("DELETE FROM decision_training_examples WHERE id = $1")
        .bind(id)
        .execute(state.db.pool())
        .await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({}))))
}
