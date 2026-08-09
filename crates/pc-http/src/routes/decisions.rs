//! `/api/decisions*` 路由：CRUD。

#[allow(unused_imports)]
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Extension as AxumExtension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_auth::AuthContext;
use pc_authz::{enforce_permission, PermissionKey};
use pc_realtime::LiveEvent;
use pc_repos::decision::{verify_decision_signature, DecisionRepo, SignedDecisionRow};
use pc_repos::decision_bundle::{
    DecisionBundleFilter, DecisionBundleRepo, DecisionBundleRow, NewDecisionBundle,
};

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
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        body.company_id,
        PermissionKey::JoinsApprove,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
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

async fn load_verified_decision(
    state: &AppState,
    decision_id: Uuid,
) -> ApiResult<SignedDecisionRow> {
    let row = DecisionRepo::new(&state.db)
        .get_signed_fields(decision_id)
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
    AxumExtension(actor): AxumExtension<AuthContext>,
    Path(decision_id): Path<Uuid>,
    Json(body): Json<DecideDecisionBody>,
) -> ApiResult<Json<Value>> {
    if body.chosen_option_id.trim().is_empty() {
        return Err(ApiError::BadRequest("chosenOptionId required".into()));
    }
    let company_id = load_verified_decision(&state, decision_id)
        .await?
        .company_id;
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        company_id,
        PermissionKey::JoinsApprove,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    DecisionRepo::new(&state.db)
        .mark_decided(
            decision_id,
            &body.chosen_option_id,
            body.decided_by_user_id.as_deref(),
            body.input_values.as_ref(),
        )
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
    AxumExtension(actor): AxumExtension<AuthContext>,
    Path(decision_id): Path<Uuid>,
    Json(body): Json<DismissDecisionBody>,
) -> ApiResult<Json<Value>> {
    let company_id = load_verified_decision(&state, decision_id)
        .await?
        .company_id;
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        company_id,
        PermissionKey::JoinsApprove,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    DecisionRepo::new(&state.db)
        .mark_dismissed(
            decision_id,
            &body.reason.clone().unwrap_or_default(),
            &body.decided_by_user_id.clone().unwrap_or_default(),
        )
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
    AxumExtension(actor): AxumExtension<AuthContext>,
    Path(decision_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = DecisionRepo::new(&state.db);
    let company_id = repo
        .get_company_id(decision_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("decision {decision_id}")))?;
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        company_id,
        PermissionKey::JoinsApprove,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    repo.mark_cancelled(decision_id).await?;
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
    let rows = DecisionRepo::new(&state.db)
        .status_counts(company_id)
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
    AxumExtension(actor): AxumExtension<AuthContext>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateDecisionBundleBody>,
) -> ApiResult<impl IntoResponse> {
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        company_id,
        PermissionKey::JoinsApprove,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    let row = DecisionBundleRepo::new(&state.db)
        .create(
            company_id,
            NewDecisionBundle {
                title: body.title.clone(),
                summary: body.summary.clone(),
                origin_agent_id: body.origin_agent_id,
                origin_issue_id: body.origin_issue_id,
                origin_run_id: body.origin_run_id,
            },
        )
        .await
        .map_err(map_decision_bundle_error)?;
    state.realtime.publish(
        LiveEvent::new("decision_bundle.created", "decision_bundle", row.id)
            .with_company(company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id,
            "companyId": row.company_id,
            "title": row.title,
            "summary": row.summary,
            "originAgentId": row.origin_agent_id,
            "originIssueId": row.origin_issue_id,
            "originRunId": row.origin_run_id,
            "createdAt": row.created_at,
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
    let filter = DecisionBundleFilter {
        agent_id: q.agent_id,
        issue_id: q.issue_id,
        run_id: q.run_id,
        limit: q.limit,
    };
    let rows = DecisionBundleRepo::new(&state.db)
        .list_by_company(company_id, &filter)
        .await?;
    let items: Vec<Value> = rows.into_iter().map(decision_bundle_to_json).collect();
    Ok(Json(
        json!({"items": items, "companyId": company_id, "count": items.len()}),
    ))
}

async fn get_decision_bundle(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let detail = DecisionBundleRepo::new(&state.db)
        .get_with_decisions(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("decision bundle {id}")))?;
    let decisions_json: Vec<Value> = detail
        .decisions
        .into_iter()
        .map(|d| {
            json!({
                "id": d.id, "title": d.title, "status": d.status,
            })
        })
        .collect();
    let b = detail.bundle;
    Ok(Json(json!({
        "id": b.id, "companyId": b.company_id,
        "title": b.title, "summary": b.summary,
        "originAgentId": b.origin_agent_id, "originIssueId": b.origin_issue_id,
        "originRunId": b.origin_run_id,
        "createdAt": b.created_at,
        "decisions": decisions_json,
        "decisionCount": decisions_json.len(),
    })))
}

/// 把 `DecisionBundleRow` 转成与原 Node 端一致的 JSON 形状。
fn decision_bundle_to_json(row: DecisionBundleRow) -> Value {
    json!({
        "id": row.id, "companyId": row.company_id,
        "title": row.title, "summary": row.summary,
        "originAgentId": row.origin_agent_id, "originIssueId": row.origin_issue_id,
        "originRunId": row.origin_run_id,
        "createdAt": row.created_at,
    })
}

/// 把仓储层错误转换成 HTTP 层错误；保留与原路由一致的状态码语义。
fn map_decision_bundle_error(error: pc_repos::decision_bundle::DecisionBundleError) -> ApiError {
    use pc_repos::decision_bundle::DecisionBundleError as E;
    match error {
        E::EmptyTitle => ApiError::BadRequest("title required".into()),
        other => ApiError::Internal(format!("decision bundle repo error: {other}")),
    }
}
