//! `/api/decisions*` 路由：CRUD。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::decision::DecisionRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/decisions", get(list).post(create))
        .route("/api/decisions/:id", get(get_one).delete(remove))
        // ── Round 22: decision decide/dismiss/cancel/stats/bundles ──
        .route("/api/decisions/:id/decide", post(decide_decision))
        .route("/api/decisions/:id/dismiss", post(dismiss_decision))
        .route("/api/decisions/:id/cancel", post(cancel_decision))
        .route("/api/companies/:company_id/decisions/stats", get(decision_stats_route))
        .route("/api/companies/:company_id/decision-bundles", post(create_decision_bundle))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    company_id: Option<Uuid>,
}

async fn list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = match q.company_id {
        Some(cid) => DecisionRepo::new(&state.db).list_by_company(cid).await?,
        None => DecisionRepo::new(&state.db).list_all(200).await?,
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = DecisionRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("decision {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    company_id: Uuid,
    title: String,
    body: String,
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.title.trim().is_empty() || body.body.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "title and body must not be empty".into(),
        ));
    }
    let row = DecisionRepo::new(&state.db)
        .create(body.company_id, &body.title, &body.body)
        .await?;
    state.realtime.publish(
        LiveEvent::new("decision.created", "decision", row.id).with_company(row.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "company_id": row.company_id, "title": row.title, "body": row.body, "status": row.status, "expires_at": row.expires_at
        })),
    ))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = DecisionRepo::new(&state.db).delete(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("decision {id}")))
    }
}

// ============== Round 22: decision decide/dismiss/cancel/stats/bundles ==============

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DecideDecisionBody {
    chosen_option_id: String,
    decided_by_user_id: Option<String>,
    note: Option<String>,
    input_values: Option<Value>,
}

async fn decide_decision(
    State(state): State<AppState>,
    Path(decision_id): Path<Uuid>,
    Json(body): Json<DecideDecisionBody>,
) -> ApiResult<Json<Value>> {
    if body.chosen_option_id.trim().is_empty() {
        return Err(ApiError::BadRequest("chosenOptionId required".into()));
    }
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM decisions WHERE id = $1")
        .bind(decision_id)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten();
    let (company_id,) = row.ok_or_else(|| ApiError::NotFound(format!("decision {decision_id}")))?;
    sqlx::query(
        "UPDATE decisions SET status = 'decided', chosen_option_id = $1, decided_by_user_id = $2, \
            decided_at = now(), input_values = COALESCE($3, input_values), updated_at = now() \
         WHERE id = $4",
    )
    .bind(&body.chosen_option_id)
    .bind(body.decided_by_user_id.as_deref())
    .bind(body.input_values.clone())
    .bind(decision_id)
    .execute(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("decision.decided", "decision", decision_id)
            .with_company(company_id)
            .with_data(json!({
                "chosenOptionId": body.chosen_option_id,
                "decidedByUserId": body.decided_by_user_id,
                "note": body.note,
            })),
    );
    Ok(Json(json!({
        "id": decision_id,
        "companyId": company_id,
        "status": "decided",
        "chosenOptionId": body.chosen_option_id,
        "decidedAt": chrono::Utc::now(),
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DismissDecisionBody {
    reason: Option<String>,
    decided_by_user_id: Option<String>,
}

async fn dismiss_decision(
    State(state): State<AppState>,
    Path(decision_id): Path<Uuid>,
    Json(body): Json<DismissDecisionBody>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM decisions WHERE id = $1")
        .bind(decision_id)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten();
    let (company_id,) = row.ok_or_else(|| ApiError::NotFound(format!("decision {decision_id}")))?;
    sqlx::query(
        "UPDATE decisions SET status = 'dismissed', \
            metadata = COALESCE(metadata, '{}'::jsonb) || jsonb_build_object('dismissReason', to_jsonb($1::text), 'dismissedByUserId', to_jsonb($2::text)), \
            updated_at = now() \
         WHERE id = $3",
    )
    .bind(body.reason.clone().unwrap_or_default())
    .bind(body.decided_by_user_id.clone().unwrap_or_default())
    .bind(decision_id)
    .execute(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("decision.dismissed", "decision", decision_id)
            .with_company(company_id)
            .with_data(json!({"reason": body.reason, "decidedByUserId": body.decided_by_user_id})),
    );
    Ok(Json(json!({
        "id": decision_id,
        "companyId": company_id,
        "status": "dismissed",
        "reason": body.reason,
    })))
}

async fn cancel_decision(
    State(state): State<AppState>,
    Path(decision_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM decisions WHERE id = $1")
        .bind(decision_id)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten();
    let (company_id,) = row.ok_or_else(|| ApiError::NotFound(format!("decision {decision_id}")))?;
    sqlx::query(
        "UPDATE decisions SET status = 'cancelled', updated_at = now() WHERE id = $1",
    )
    .bind(decision_id)
    .execute(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("decision.cancelled", "decision", decision_id).with_company(company_id),
    );
    Ok(Json(json!({
        "id": decision_id,
        "companyId": company_id,
        "status": "cancelled",
    })))
}

async fn decision_stats_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*) FROM decisions WHERE company_id = $1 GROUP BY status",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let mut total = 0i64;
    let mut by_status: std::collections::BTreeMap<String, i64> = Default::default();
    for (s, c) in &rows {
        by_status.insert(s.clone(), *c);
        total += c;
    }
    let open_count = by_status.get("open").copied().unwrap_or(0);
    let decided = by_status.get("decided").copied().unwrap_or(0);
    let dismissed = by_status.get("dismissed").copied().unwrap_or(0);
    let cancelled = by_status.get("cancelled").copied().unwrap_or(0);
    Ok(Json(json!({
        "companyId": company_id,
        "total": total,
        "open": open_count,
        "decided": decided,
        "dismissed": dismissed,
        "cancelled": cancelled,
        "byStatus": by_status,
    })))
}


#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateDecisionBundleBody {
    title: String,
    summary: Option<String>,
    origin_agent_id: Uuid,
    origin_issue_id: Uuid,
    origin_run_id: Uuid,
}

async fn create_decision_bundle(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateDecisionBundleBody>,
) -> ApiResult<impl IntoResponse> {
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title required".into()));
    }
    let summary = body.summary.clone().unwrap_or_else(|| body.title.clone());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO decision_bundles (company_id, title, summary, origin_agent_id, origin_issue_id, origin_run_id) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(company_id)
    .bind(&body.title)
    .bind(&summary)
    .bind(body.origin_agent_id)
    .bind(body.origin_issue_id)
    .bind(body.origin_run_id)
    .fetch_one(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("decision_bundle.created", "decision_bundle", id).with_company(company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "companyId": company_id,
            "title": body.title,
            "summary": summary,
            "originAgentId": body.origin_agent_id,
            "originIssueId": body.origin_issue_id,
            "originRunId": body.origin_run_id,
        })),
    ))
}
