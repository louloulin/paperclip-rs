//! `/api/decisions*` 路由：CRUD。

#[allow(unused_imports)]
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::decision::{verify_decision_signature, DecisionRepo};

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/decisions", get(list).post(create))
        .route("/api/decisions/:id", get(get_one).delete(remove))
        // ── Round 22: decision decide/dismiss/cancel/stats/bundles ──
        .route("/api/decisions/:id/decide", post(decide_decision))
        .route("/api/decisions/:id/dismiss", post(dismiss_decision))
        .route("/api/decisions/:id/cancel", post(cancel_decision))
        .route(
            "/api/companies/:company_id/decisions/stats",
            get(decision_stats_route),
        )
        .route(
            "/api/companies/:company_id/decision-bundles",
            post(create_decision_bundle).get(list_decision_bundles),
        )
        .route("/api/decision-bundles/:id", get(get_decision_bundle))
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
        .create(
            body.company_id,
            &body.title,
            &body.body,
            &state.decision_signing,
        )
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

#[derive(Debug, sqlx::FromRow)]
struct SignedDecisionRow {
    company_id: Uuid,
    options: Value,
    target_snapshots: Value,
    signed_spec: String,
}

async fn load_verified_decision(
    state: &AppState,
    decision_id: Uuid,
) -> ApiResult<SignedDecisionRow> {
    let row = sqlx::query_as::<_, SignedDecisionRow>(
        "SELECT company_id, options, target_snapshots, signed_spec FROM decisions WHERE id = $1",
    )
    .bind(decision_id)
    .fetch_optional(state.db.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("decision {decision_id}")))?;
    let verified = verify_decision_signature(
        decision_id,
        &row.options,
        &row.target_snapshots,
        &row.signed_spec,
        &state.decision_signing,
    )
    .map_err(|error| ApiError::Internal(format!("resolve decision signing secret: {error}")))?;
    if !verified {
        return Err(ApiError::Forbidden(
            "Decision signature verification failed".into(),
        ));
    }
    Ok(row)
}

async fn decide_decision(
    State(state): State<AppState>,
    Path(decision_id): Path<Uuid>,
    Json(body): Json<DecideDecisionBody>,
) -> ApiResult<Json<Value>> {
    if body.chosen_option_id.trim().is_empty() {
        return Err(ApiError::BadRequest("chosenOptionId required".into()));
    }
    let company_id = load_verified_decision(&state, decision_id)
        .await?
        .company_id;
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
    let company_id = load_verified_decision(&state, decision_id)
        .await?
        .company_id;
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
    sqlx::query("UPDATE decisions SET status = 'cancelled', updated_at = now() WHERE id = $1")
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

// ============ Round 34: decision bundles list + detail ============

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ListDecisionBundlesQuery {
    #[serde(default)]
    agent_id: Option<Uuid>,
    #[serde(default)]
    issue_id: Option<Uuid>,
    #[serde(default)]
    run_id: Option<Uuid>,
    #[serde(default)]
    limit: Option<i64>,
}

async fn list_decision_bundles(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(q): Query<ListDecisionBundlesQuery>,
) -> ApiResult<Json<Value>> {
    let mut sql = String::from(
        "SELECT id, company_id, title, summary, origin_agent_id, origin_issue_id, origin_run_id, created_at          FROM decision_bundles WHERE company_id = $1",
    );
    let mut idx = 2;
    if q.agent_id.is_some() {
        sql.push_str(&format!(" AND origin_agent_id = ${idx}"));
        idx += 1;
    }
    if q.issue_id.is_some() {
        sql.push_str(&format!(" AND origin_issue_id = ${idx}"));
        idx += 1;
    }
    if q.run_id.is_some() {
        sql.push_str(&format!(" AND origin_run_id = ${idx}"));
        idx += 1;
    }
    sql.push_str(&format!(
        " ORDER BY created_at DESC LIMIT {}",
        q.limit.unwrap_or(100).clamp(1, 500)
    ));
    let mut query = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            String,
            String,
            Uuid,
            Uuid,
            Uuid,
            pc_core::Timestamp,
        ),
    >(&sql)
    .bind(company_id);
    if let Some(a) = q.agent_id {
        query = query.bind(a);
    }
    if let Some(i) = q.issue_id {
        query = query.bind(i);
    }
    if let Some(r) = q.run_id {
        query = query.bind(r);
    }
    let rows = query.fetch_all(state.db.pool()).await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, cid, title, summary, agent, issue, run, ts)| {
            json!({
                "id": id, "companyId": cid,
                "title": title, "summary": summary,
                "originAgentId": agent, "originIssueId": issue, "originRunId": run,
                "createdAt": ts,
            })
        })
        .collect();
    Ok(Json(
        json!({"items": items, "companyId": company_id, "count": items.len()}),
    ))
}

async fn get_decision_bundle(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, Uuid, String, String, Uuid, Uuid, Uuid, pc_core::Timestamp)> = sqlx::query_as(
        "SELECT id, company_id, title, summary, origin_agent_id, origin_issue_id, origin_run_id, created_at          FROM decision_bundles WHERE id = $1",
    ).bind(id).fetch_optional(state.db.pool()).await?;
    let (id, cid, title, summary, agent, issue, run, ts) =
        row.ok_or_else(|| ApiError::NotFound(format!("decision bundle {id}")))?;
    // 同时返回挂载的 decisions
    let decisions: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, title, status FROM decisions WHERE bundle_id = $1 ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(state.db.pool())
    .await?;
    let decisions_json: Vec<Value> = decisions
        .into_iter()
        .map(|(did, t, s)| {
            json!({
                "id": did, "title": t, "status": s,
            })
        })
        .collect();
    Ok(Json(json!({
        "id": id, "companyId": cid,
        "title": title, "summary": summary,
        "originAgentId": agent, "originIssueId": issue, "originRunId": run,
        "createdAt": ts,
        "decisions": decisions_json,
        "decisionCount": decisions_json.len(),
    })))
}
