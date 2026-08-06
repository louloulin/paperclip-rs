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

use pc_repos::status_card::{
    StatusCardRepo, StatusCardRow, StatusCardUpdateRow, SummaryRevisionRow,
};

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

// Local type aliases — 一一对应 pc_repos::status_card DTOs
use StatusCardUpdateRow as UpdateRow;

fn row_json(row: &StatusCardRow) -> Value {
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
    let rows = StatusCardRepo::new(&state.db)
        .list_active(company_id)
        .await?;
    let items: Vec<Value> = rows.iter().map(row_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn get_status_card(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = StatusCardRepo::new(&state.db).get_by_id(id).await?;
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
    let repo = StatusCardRepo::new(&state.db);
    let row = repo
        .create(
            company_id,
            title.as_deref(),
            &prompt,
            &queries,
            &refresh_policy,
        )
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
    let archived = body.archived;
    let row = StatusCardRepo::new(&state.db)
        .patch(
            id,
            title.as_deref(),
            prompt.as_deref(),
            refresh_policy.as_ref(),
            archived,
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("status card {id}")))?;
    Ok(Json(row_json(&row)))
}

async fn delete_status_card(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    StatusCardRepo::new(&state.db).delete(id).await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({}))))
}

async fn card_updates(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<UpdateRow>>> {
    let rows = StatusCardRepo::new(&state.db).list_updates(id).await?;
    Ok(Json(rows))
}

async fn card_summary_revisions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<SummaryRevisionRow>>> {
    let link = StatusCardRepo::new(&state.db).get_doc_link(id).await?;
    let Some((company_id, Some(document_id))) = link else {
        return Ok(Json(Vec::new()));
    };
    // Use DocumentRepo for revisions (1:1 复用)
    let rows = pc_repos::document::DocumentRepo::new(&state.db)
        .list_revisions_in_company(company_id, document_id, 100)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    // Adapt DocRevision → SummaryRevision shape
    let summary_rows: Vec<SummaryRevisionRow> = rows
        .into_iter()
        .map(|r| SummaryRevisionRow {
            id: r.id,
            revision_number: r.revision_number,
            title: r.title,
            body: r.body,
            change_summary: r.change_summary,
            created_at: r.created_at,
        })
        .collect();
    Ok(Json(summary_rows))
}

async fn card_recompile(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // Mark card as "compiling" and bump query_version; the watcher will pick
    // it up and produce a fresh compiled query.
    let row = StatusCardRepo::new(&state.db).recompile(id).await?;
    let Some(row) = row else {
        return Err(ApiError::NotFound(format!("status card {id}")));
    };
    // Emit live event so the watcher can pick it up.
    state.realtime.publish(
        pc_realtime::LiveEvent::new("status_card.recompile.requested", "status_card", id)
            .with_company(row.company_id)
            .with_actor("manual")
            .with_data(json!({ "cardId": id })),
    );
    Ok((StatusCode::ACCEPTED, Json(row_json(&row))))
}

async fn card_refresh(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let repo = StatusCardRepo::new(&state.db);
    let row = repo
        .get_by_id(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("status card {id}")))?;
    let company_id = row.company_id;
    // Schedule a refresh by bumping next_eval_at to now; the watcher polls this.
    repo.refresh(id).await?;
    state.realtime.publish(
        pc_realtime::LiveEvent::new("status_card.refresh.requested", "status_card", id)
            .with_company(company_id)
            .with_actor("manual"),
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "cardId": id, "status": "refresh-queued" })),
    ))
}

/// Claim status cards whose `next_eval_at <= now()` and are in a state
/// that admits a refresh. Mirrors Node `claimDueStatusCardUpdates` in
/// `services/status-cards.ts`. Returns the claimed rows so the caller can
/// hand them to the refresh / recompile pipeline.
pub async fn claim_due_status_card_updates(state: &AppState, limit: i64) -> ApiResult<usize> {
    let count = StatusCardRepo::new(&state.db).claim_due(limit).await?;
    if count > 0 {
        state.realtime.publish(
            pc_realtime::LiveEvent::new("status_card.tick.claimed", "status_card", Uuid::nil())
                .with_data(json!({ "claimedCount": count })),
        );
    }
    Ok(count as usize)
}

async fn card_dry_run(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = StatusCardRepo::new(&state.db).dry_run_meta(id).await?;
    let Some((query_version, queries, mentioned_issues)) = row else {
        return Err(ApiError::NotFound(format!("status card {id}")));
    };
    Ok(Json(json!({
        "cardId": id,
        "queryVersion": query_version,
        "queries": queries,
        "mentionedIssues": mentioned_issues
    })))
}

async fn card_query(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<QueryBody>,
) -> ApiResult<Json<Value>> {
    let row = StatusCardRepo::new(&state.db)
        .update_queries(id, &body.queries)
        .await?;
    let Some(row) = row else {
        return Err(ApiError::NotFound(format!("status card {id}")));
    };
    Ok(Json(row_json(&row)))
}

async fn card_summary(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SummaryBody>,
) -> ApiResult<Json<Value>> {
    // 校验 card 存在并取 company_id
    let repo = StatusCardRepo::new(&state.db);
    let card_row = repo
        .get_by_id(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("status card {id}")))?;
    let company_id = card_row.company_id;
    let body_len = body.body.chars().count();
    let summary_id = repo
        .insert_summary_update(
            id,
            &json!([{ "field": "summary", "op": "set", "value": body.body }]),
            body.model.as_deref(),
            &format!("manual summary ({body_len} chars)"),
        )
        .await?;
    // 更新 status_cards.last_generated_at
    repo.touch_last_generated(id).await?;
    // 发布 live event
    state.realtime.publish(
        pc_realtime::LiveEvent::new("status_card.summary.created", "status_card", id)
            .with_company(company_id)
            .with_actor("manual")
            .with_data(
                json!({ "summaryId": summary_id, "model": body.model, "bodyLength": body_len }),
            ),
    );
    Ok(Json(json!({
        "cardId": id,
        "summaryId": summary_id,
        "companyId": company_id,
        "model": body.model,
        "bodyLength": body_len,
        "status": "completed",
    })))
}
