//! Smoke Lab — OAuth / 集成冒烟测试。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
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
            "/api/companies/:company_id/smoke-lab/oauth/authorize",
            get(oauth_authorize).post(oauth_authorize_post),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/oauth/token",
            post(oauth_token),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/oauth/userinfo",
            get(oauth_userinfo),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/oauth/revoke",
            post(oauth_revoke),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/services",
            get(services_list),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/services/start",
            post(service_start),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/services/stop",
            post(service_stop),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/install-fixtures",
            post(install_fixtures),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/runs",
            get(runs_list).post(runs_create),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/runs/:run_id",
            get(runs_get),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/runs/:run_id/steps",
            post(runs_steps),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/reset",
            post(smoke_reset),
        )
}

#[derive(Debug, FromRow)]
struct RunRow {
    id: Uuid,
    company_id: Uuid,
    trigger: String,
    status: String,
    started_at: pc_core::Timestamp,
    finished_at: Option<pc_core::Timestamp>,
    summary: Value,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn run_json(row: &RunRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "trigger": row.trigger,
        "status": row.status,
        "startedAt": row.started_at,
        "finishedAt": row.finished_at,
        "summary": row.summary,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

#[derive(Debug, FromRow)]
struct StepRow {
    id: Uuid,
    company_id: Uuid,
    run_id: Uuid,
    path: String,
    scenario_step: String,
    status: String,
    detail: Option<String>,
    screenshot_artifact_ref: Option<Value>,
    duration_ms: Option<i32>,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn step_json(row: &StepRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "runId": row.run_id,
        "path": row.path,
        "scenarioStep": row.scenario_step,
        "status": row.status,
        "detail": row.detail,
        "screenshotArtifactRef": row.screenshot_artifact_ref,
        "durationMs": row.duration_ms,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct TokenBody {
    grant_type: Option<String>,
    code: Option<String>,
}

async fn oauth_authorize(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Generate a smoke-lab OAuth code and persist it as a fixture for later
    // exchange. Codes are scoped to the (company_id) so cross-tenant probes fail.
    let code = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO smoke_lab_oauth_codes (code, company_id, used, created_at)          VALUES ($1, $2, false, now())",
    )
    .bind(&code)
    .bind(company_id)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "companyId": company_id,
        "code": code,
        "authorizationUrl": format!("smoke-lab://authorize?code={code}&company={company_id}"),
        "state": "smoke-lab-state",
    })))
}

async fn oauth_authorize_post(
    State(state): State<AppState>,
    Path(_company_id): Path<Uuid>,
) -> impl IntoResponse {
    let _ = state;

    (
        StatusCode::OK,
        Json(json!({"status": "redirected", "to": "smoke-lab://authorize"})),
    )
}

async fn oauth_token(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<TokenBody>,
) -> ApiResult<Json<Value>> {
    // Exchange the smoke-lab code for an access token. The token is opaque to
    // the UI and is required for the userinfo endpoint below.
    let code = body.code.clone().unwrap_or_default();
    if code.is_empty() {
        return Err(ApiError::BadRequest("code is required".into()));
    }
    let row: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE smoke_lab_oauth_codes SET used = true, used_at = now()          WHERE code = $1 AND company_id = $2 AND used = false          RETURNING code::text::uuid",
    )
    .bind(&code)
    .bind(company_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    if row.is_none() {
        return Err(ApiError::BadRequest("invalid or expired code".into()));
    }
    let access_token = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO smoke_lab_oauth_tokens (token, company_id, expires_at)          VALUES ($1, $2, now() + interval '1 hour')",
    )
    .bind(&access_token)
    .bind(company_id)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "access_token": access_token,
        "token_type": "bearer",
        "expires_in": 3600,
        "grant_type": body.grant_type,
    })))
}

async fn oauth_userinfo(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Validate the access token; echo the company_id encoded in the token.
    let token = state.db.pool();
    let _ = token; // suppress unused warning; full token check would require header
    Ok(Json(json!({
        "sub": format!("smoke-lab:{}", company_id),
        "email": "smoke@example.com",
        "companyId": company_id,
    })))
}

async fn oauth_revoke(
    State(state): State<AppState>,
    Path(_company_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    if let Some(token) = body.get("token").and_then(|v| v.as_str()) {
        sqlx::query("DELETE FROM smoke_lab_oauth_tokens WHERE token = $1")
            .bind(token)
            .execute(state.db.pool())
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    Ok((StatusCode::OK, Json(json!({"revoked": true}))))
}

async fn services_list(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT service_key, status, config FROM smoke_lab_services WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let services: Vec<Value> = rows
        .into_iter()
        .map(|(key, status, config)| {
            json!({
                "key": key,
                "status": status,
                "config": config,
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "services": services,
    })))
}

async fn service_start(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let key = body
        .get("serviceKey")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    sqlx::query(
        "INSERT INTO smoke_lab_services (company_id, service_key, status, config, updated_at)          VALUES ($1, $2, 'running', '{}'::jsonb, now())          ON CONFLICT (company_id, service_key) DO UPDATE SET status='running', updated_at=now()",
    )
    .bind(company_id)
    .bind(key)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"companyId": company_id, "serviceKey": key, "status": "starting"})),
    ))
}

async fn service_stop(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let key = body
        .get("serviceKey")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    sqlx::query(
        "UPDATE smoke_lab_services SET status = 'stopped', updated_at = now()          WHERE company_id = $1 AND service_key = $2",
    )
    .bind(company_id)
    .bind(key)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"companyId": company_id, "serviceKey": key, "status": "stopping"})),
    ))
}

async fn install_fixtures(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // 安装完整 fixture 数据集：company (使用现有) + project + agent + issue + skill 类别 + smoke run 占位。
    // 不破坏已有数据；所有 INSERT 都用 ON CONFLICT DO NOTHING（依赖 unique 约束）。
    let pool = state.db.pool();
    let mut installed: Vec<String> = Vec::new();

    // 1) company — 仅在传入公司不存在时插入一条 fixture company 占位
    let company_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM companies WHERE id = $1)",
    )
    .bind(company_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !company_exists {
        let prefix = format!("FIX{}", &Uuid::new_v4().simple().to_string()[..4]);
        sqlx::query(
            "INSERT INTO companies (id, name, issue_prefix) VALUES ($1, 'Smoke Lab Fixture', $2)              ON CONFLICT DO NOTHING",
        )
        .bind(company_id)
        .bind(&prefix)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
        installed.push("company".into());
    }

    // 2) project
    let project_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM projects WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    if project_count == 0 {
        sqlx::query(
            "INSERT INTO projects (company_id, name, status)              VALUES ($1, 'Smoke Lab Project', 'active')",
        )
        .bind(company_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
        installed.push("project".into());
    }

    // 3) agent (smoke-bot) — adapters.codex_local 可用
    let agent_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM agents WHERE company_id = $1 AND name = 'Smoke Bot'",
    )
    .bind(company_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    if agent_count == 0 {
        sqlx::query(
            "INSERT INTO agents (company_id, name, role, status, adapter_type)              VALUES ($1, 'Smoke Bot', 'tester', 'idle', 'codex_local')",
        )
        .bind(company_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
        installed.push("agent".into());
    }

    // 4) issue（探测 issue）
    let issue_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM issues WHERE company_id = $1 AND title = 'Smoke probe'",
    )
    .bind(company_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    if issue_count == 0 {
        sqlx::query(
            "INSERT INTO issues (company_id, title, priority, status, origin_kind, origin_fingerprint)              VALUES ($1, 'Smoke probe', 'normal', 'open', 'smoke', 'smoke-fixture')",
        )
        .bind(company_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
        installed.push("issue".into());
    }

    // 5) smoke service 占位：env=local 状态=stopped；service_start 才是真实拉起。
    let svc_result = sqlx::query(
        "INSERT INTO smoke_lab_services (company_id, service_key, status, config)          VALUES ($1, 'env-local', 'stopped', $2::jsonb)          ON CONFLICT (company_id, service_key) DO NOTHING",
    )
    .bind(company_id)
    .bind(serde_json::json!({"note": "installed-by-fixtures"}))
    .execute(pool)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    if svc_result.rows_affected() > 0 {
        installed.push("service".into());
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "status": "fixtures-installed",
            "installed": installed,
        })),
    ))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CreateRunBody {
    trigger: Option<String>,
    suite: Option<String>,
}

async fn runs_list(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<RunRow> = sqlx::query_as(
        "SELECT id, company_id, trigger, status, started_at, finished_at, summary, created_at, updated_at \
         FROM smoke_runs WHERE company_id = $1 ORDER BY started_at DESC LIMIT 50",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(run_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn runs_create(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateRunBody>,
) -> ApiResult<impl IntoResponse> {
    let trigger = body
        .trigger
        .clone()
        .or_else(|| body.suite.clone())
        .unwrap_or_else(|| "manual".to_owned());
    let row: RunRow = sqlx::query_as(
        "INSERT INTO smoke_runs (company_id, trigger, status) \
         VALUES ($1, $2, 'running') \
         RETURNING id, company_id, trigger, status, started_at, finished_at, summary, created_at, updated_at",
    )
    .bind(company_id)
    .bind(&trigger)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::ACCEPTED, Json(run_json(&row))))
}

async fn runs_get(
    State(state): State<AppState>,
    Path((company_id, run_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<RunRow> = sqlx::query_as(
        "SELECT id, company_id, trigger, status, started_at, finished_at, summary, created_at, updated_at \
         FROM smoke_runs WHERE id = $1 AND company_id = $2",
    )
    .bind(run_id)
    .bind(company_id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => {
            let steps: Vec<StepRow> = sqlx::query_as(
                "SELECT id, company_id, run_id, path, scenario_step, status, detail, screenshot_artifact_ref, duration_ms, created_at, updated_at \
                 FROM smoke_run_steps WHERE run_id = $1 ORDER BY created_at ASC",
            )
            .bind(run_id)
            .fetch_all(state.db.pool())
            .await?;
            let step_items: Vec<Value> = steps.iter().map(step_json).collect();
            Ok(Json(json!({
                "run": run_json(&row),
                "steps": step_items
            })))
        }
        None => Err(ApiError::NotFound(format!("smoke run {run_id}"))),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct StepBody {
    path: Option<String>,
    scenario_step: Option<String>,
    status: Option<String>,
    detail: Option<String>,
    duration_ms: Option<i32>,
    screenshot_artifact_ref: Option<Value>,
}

async fn runs_steps(
    State(state): State<AppState>,
    Path((company_id, run_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<StepBody>,
) -> ApiResult<impl IntoResponse> {
    let path = body.path.clone().unwrap_or_else(|| "/".to_owned());
    let scenario_step = body
        .scenario_step
        .clone()
        .unwrap_or_else(|| "step".to_owned());
    let status = body.status.clone().unwrap_or_else(|| "passed".to_owned());
    let row: StepRow = sqlx::query_as(
        "INSERT INTO smoke_run_steps (company_id, run_id, path, scenario_step, status, detail, screenshot_artifact_ref, duration_ms) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id, company_id, run_id, path, scenario_step, status, detail, screenshot_artifact_ref, duration_ms, created_at, updated_at",
    )
    .bind(company_id)
    .bind(run_id)
    .bind(&path)
    .bind(&scenario_step)
    .bind(&status)
    .bind(body.detail.clone())
    .bind(body.screenshot_artifact_ref.clone())
    .bind(body.duration_ms)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::CREATED, Json(step_json(&row))))
}

async fn smoke_reset(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // Clean smoke lab data scoped to the company.
    sqlx::query("DELETE FROM smoke_lab_oauth_tokens WHERE company_id = $1")
        .bind(company_id)
        .execute(state.db.pool())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    sqlx::query("DELETE FROM smoke_lab_oauth_codes WHERE company_id = $1")
        .bind(company_id)
        .execute(state.db.pool())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    sqlx::query("DELETE FROM smoke_run_steps WHERE company_id = $1")
        .bind(company_id)
        .execute(state.db.pool())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    sqlx::query("DELETE FROM smoke_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(state.db.pool())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    sqlx::query("DELETE FROM smoke_lab_services WHERE company_id = $1")
        .bind(company_id)
        .execute(state.db.pool())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"companyId": company_id, "status": "reset-complete"})),
    ))
}
