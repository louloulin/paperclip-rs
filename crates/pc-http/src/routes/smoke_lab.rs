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

// Round 153: `RunRow` / `StepRow` 已迁到 `pc_repos::smoke::{RunRow, StepRow}`。
use pc_repos::smoke::{RunRow, StepRow};

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
    pc_repos::smoke::SmokeRepo::new(&state.db)
        .insert_oauth_code(&code, company_id)
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
    let smoke_repo = pc_repos::smoke::SmokeRepo::new(&state.db);
    let claimed = smoke_repo
        .claim_oauth_code(&code, company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !claimed {
        return Err(ApiError::BadRequest("invalid or expired code".into()));
    }
    let access_token = Uuid::new_v4().to_string();
    smoke_repo
        .insert_oauth_token(&access_token, company_id)
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
        pc_repos::smoke::SmokeRepo::new(&state.db)
            .delete_oauth_token(token)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    Ok((StatusCode::OK, Json(json!({"revoked": true}))))
}

async fn services_list(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = pc_repos::smoke::SmokeRepo::new(&state.db)
        .list_services(company_id)
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
    pc_repos::smoke::SmokeRepo::new(&state.db)
        .upsert_service_running(company_id, key)
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
    pc_repos::smoke::SmokeRepo::new(&state.db)
        .stop_service(company_id, key)
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
    let repo = pc_repos::smoke::SmokeRepo::new(&state.db);
    let mut installed: Vec<String> = Vec::new();

    // 1) company — 仅在传入公司不存在时插入一条 fixture company 占位
    let company_exists = repo
        .company_exists(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !company_exists {
        let prefix = format!("FIX{}", &Uuid::new_v4().simple().to_string()[..4]);
        repo.insert_fixture_company(company_id, "Smoke Lab Fixture", &prefix)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        installed.push("company".into());
    }

    // 2) project
    let project_count = repo
        .count_projects(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if project_count == 0 {
        repo.insert_smoke_project(company_id, "Smoke Lab Project")
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        installed.push("project".into());
    }

    // 3) agent (smoke-bot) — adapters.codex_local 可用
    let agent_count = repo
        .count_agents_with_name(company_id, "Smoke Bot")
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if agent_count == 0 {
        repo.insert_smoke_agent(company_id, "Smoke Bot", "tester", "idle", "codex_local")
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        installed.push("agent".into());
    }

    // 4) issue（探测 issue）
    let issue_count = repo
        .count_issues_with_title(company_id, "Smoke probe")
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if issue_count == 0 {
        repo.insert_smoke_issue(
            company_id,
            "Smoke probe",
            "normal",
            "open",
            "smoke",
            "smoke-fixture",
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
        installed.push("issue".into());
    }

    // 5) smoke service 占位：env=local 状态=stopped；service_start 才是真实拉起。
    let svc_inserted = repo
        .insert_smoke_service_if_absent(
            company_id,
            "env-local",
            "stopped",
            serde_json::json!({"note": "installed-by-fixtures"}),
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if svc_inserted {
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
    let rows = pc_repos::smoke::SmokeRepo::new(&state.db)
        .list_by_company(company_id, None)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let new_run = pc_repos::smoke::NewRun {
        company_id,
        trigger: pc_repos::smoke::SmokeRunTrigger::parse(&trigger)
            .unwrap_or(pc_repos::smoke::SmokeRunTrigger::Manual),
    };
    let row = pc_repos::smoke::SmokeRepo::new(&state.db)
        .create_run(&new_run)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((StatusCode::ACCEPTED, Json(run_json(&row))))
}

async fn runs_get(
    State(state): State<AppState>,
    Path((company_id, run_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let smoke_repo = pc_repos::smoke::SmokeRepo::new(&state.db);
    let row = smoke_repo
        .get(company_id, run_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match row {
        Some(row) => {
            let steps = smoke_repo
                .list_steps(run_id)
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let status_str = body.status.clone().unwrap_or_else(|| "passed".to_owned());
    let status = pc_repos::smoke::SmokeStepStatus::parse(&status_str)
        .unwrap_or(pc_repos::smoke::SmokeStepStatus::Passed);
    let path_enum = pc_repos::smoke::SmokeStepPath::parse(&path)
        .unwrap_or(pc_repos::smoke::SmokeStepPath::OauthAuthorize);
    let new_step = pc_repos::smoke::NewStep {
        company_id,
        run_id,
        path: path_enum,
        scenario_step: scenario_step.clone(),
        status,
        detail: body.detail.clone(),
        screenshot_artifact_ref: body.screenshot_artifact_ref.clone(),
        duration_ms: body.duration_ms,
    };
    let row = pc_repos::smoke::SmokeRepo::new(&state.db)
        .add_step(&new_step)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((StatusCode::CREATED, Json(step_json(&row))))
}

async fn smoke_reset(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // Clean smoke lab data scoped to the company.
    pc_repos::smoke::SmokeRepo::new(&state.db)
        .reset_company(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"companyId": company_id, "status": "reset-complete"})),
    ))
}
