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

use pc_repos::decision_training::{
    CreateInput, DecisionTrainingExampleRow, DecisionTrainingService,
};

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

fn row_json(row: &DecisionTrainingExampleRow) -> Value {
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
    let rows = DecisionTrainingService::new(&state.db)
        .list_filtered_simple(
            company_id,
            q.kind.as_deref(),
            q.author.as_deref(),
            q.q.as_deref(),
        )
        .await?;
    let items: Vec<Value> = rows.iter().map(row_json).collect();
    Ok(Json(
        json!({ "companyId": company_id, "items": items, "count": items.len() }),
    ))
}

async fn preview_training_v2(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Json<Value>> {
    let source_kind = body
        .get("sourceKind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::BadRequest("sourceKind required".into()))?;
    validate_source_kind(source_kind)?;
    let source_id_str = body
        .get("sourceId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::BadRequest("sourceId required".into()))?;
    let source_id = uuid::Uuid::parse_str(source_id_str)
        .map_err(|_| ApiError::BadRequest("sourceId must be uuid".into()))?;
    let issue_id = body
        .get("issueId")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let mut snapshot = json!({});
    let mut decision_outcome: Option<String> = None;
    match source_kind {
        "execution_decision" => {
            let row = DecisionTrainingService::new(&state.db)
                .preview_decision(company_id, source_id)
                .await?;
            if let Some((st, outcome, opts)) = row {
                snapshot = json!({"kind": "decision", "status": st, "options": opts});
                decision_outcome = outcome;
            }
        }
        "approval" => {
            let row = DecisionTrainingService::new(&state.db)
                .preview_approval(company_id, source_id)
                .await?;
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
    let rows = DecisionTrainingService::new(&state.db)
        .export_resolved_decisions(company_id)
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
    let row = DecisionTrainingService::new(&state.db)
        .get_by_id(id)
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
    let user_id = require_user_id(&state, &headers)
        .await
        .unwrap_or_else(|_| "system".to_string());
    let sk = pc_repos::decision_training::DecisionTrainingSourceKind::parse(&source_kind)
        .ok_or_else(|| ApiError::BadRequest(format!("invalid source_kind: {source_kind}")))?;
    let row = DecisionTrainingService::new(&state.db)
        .create(CreateInput {
            company_id,
            source_kind: sk,
            source_id,
            issue_id,
            notes,
            created_by_user_id: user_id,
        })
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(row_json(&row)))
}

async fn patch_training(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let owner = DecisionTrainingService::new(&state.db)
        .owner_for_id(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("training example {id}")))?;
    let user_id = require_user_id(&state, &headers)
        .await
        .map_err(|_| ApiError::Unauthorized("auth required to modify training example".into()))?;
    if owner != user_id && owner != "system" {
        return Err(ApiError::Forbidden(
            "only the example author can modify this training example".into(),
        ));
    }
    let notes = body.notes.clone();
    let outcome = body.decision_outcome.clone();
    let row = DecisionTrainingService::new(&state.db)
        .patch_with_history(id, notes, outcome)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("training example {id}")))?;
    Ok(Json(row_json(&row)))
}

async fn delete_training(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let owner = DecisionTrainingService::new(&state.db)
        .owner_for_id(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("training example {id}")))?;
    let user_id = require_user_id(&state, &headers)
        .await
        .map_err(|_| ApiError::Unauthorized("auth required to delete training example".into()))?;
    if owner != user_id && owner != "system" {
        return Err(ApiError::Forbidden(
            "only the example author can delete this training example".into(),
        ));
    }
    DecisionTrainingService::new(&state.db).delete(id).await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({}))))
}
