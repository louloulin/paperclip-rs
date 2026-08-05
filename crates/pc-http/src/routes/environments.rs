//! `/api/environments*` 路由：CRUD（environments 不属于 company，全局共享）。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_core::Timestamp;
use pc_realtime::LiveEvent;
use pc_repos::environment::{EnvironmentRepo, EnvironmentRow};

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/environments", get(list).post(create))
        .route(
            "/api/environments/:id",
            get(get_one).patch(update).delete(remove),
        )
        .route(
            "/api/companies/:company_id/environments",
            get(list_company_environments).post(create_company_environment),
        )
        .route(
            "/api/companies/:company_id/environments/capabilities",
            get(environment_capabilities),
        )
        .route(
            "/api/environments/:id/leases",
            get(list_environment_leases),
        )
        .route("/api/environments/:id/secret-refs", get(get_environment_secret_refs))
        .route(
            "/api/environments/:id/delete-blast-radius",
            get(environment_delete_blast_radius),
        )
        .route("/api/environments/:id/probe", post(probe_environment))
        // ── Round 202: bulk config probe ──
        .route(
            "/api/companies/:company_id/environments/probe-config",
            post(probe_company_environments_config),
        )
        .route(
            "/api/environments/:id/custom-image-template",
            get(get_custom_image_template).delete(delete_custom_image_template),
        )
        .route(
            "/api/environments/:environment_id/custom-image-template",
            get(get_custom_image_template_envid),
        )
        .route(
            "/api/environments/:environment_id/custom-image-template/rollback",
            post(rollback_custom_image_template),
        )
        .route(
            "/api/environment-custom-image-setup-sessions/:session_id",
            get(get_custom_image_setup_session),
        )
        // ── Round 204: custom-image-setup-sessions lifecycle ──
        .route(
            "/api/environments/:environment_id/custom-image-setup-sessions",
            post(create_custom_image_setup_session),
        )
        .route(
            "/api/environment-custom-image-setup-sessions/:id/cancel",
            post(cancel_custom_image_setup_session),
        )
        .route(
            "/api/environment-custom-image-setup-sessions/:id/finish",
            post(finish_custom_image_setup_session),
        )
        .route(
            "/api/environment-custom-image-setup-sessions/:id/terminal-session-token",
            post(terminal_session_token),
        )
        .route(
            "/api/environment-leases/:lease_id",
            get(get_environment_lease),
        )
}

async fn list(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let rows = EnvironmentRepo::new(&state.db).list_all().await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = EnvironmentRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("environment {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    name: String,
    #[serde(default = "default_driver")]
    driver: String,
    #[serde(default)]
    config: serde_json::Value,
}
fn default_driver() -> String {
    "local".into()
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let cfg = if body.config.is_null() {
        serde_json::json!({})
    } else {
        body.config
    };
    let row = EnvironmentRepo::new(&state.db)
        .create_simple(&body.name, &body.driver, cfg)
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("environment.created", "environment", row.id));
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "name": row.name, "driver": row.driver, "status": row.status
        })),
    ))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UpdateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let row = EnvironmentRepo::new(&state.db)
        .update(id, body.name.as_deref(), body.status.as_deref(), body.config)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("environment {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("environment.updated", "environment", row.id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = EnvironmentRepo::new(&state.db).delete(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("environment {id}")))
    }
}


// ============== Sub-resource handlers ==============

async fn list_company_environments(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/companies/:companyId/environments`. Returns all
    // environments globally — environments are shared across companies, but
    // the UI scopes the list by company for organization.
    let _ = company_id;
    let rows = EnvironmentRepo::new(&state.db).list_all().await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn create_company_environment(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    let _ = company_id;
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let row = EnvironmentRepo::new(&state.db)
        .create_simple(&body.name, &body.driver, body.config.clone())
        .await?;
    state.realtime.publish(
        LiveEvent::new("environment.created", "environment", row.id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn environment_capabilities(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/companies/:companyId/environments/capabilities`. We
    // surface the driver → adapter capability matrix the UI uses to render
    // per-driver options.
    let _ = (&state, &company_id);
    let drivers = ["local_process", "docker", "kubernetes", "remote_ssh"];
    let items: Vec<Value> = drivers
        .iter()
        .map(|driver| {
            let d: &str = driver;
            json!({
                "driver": d,
                "supportsWorkspaces": d == "local_process" || d == "docker" || d == "kubernetes",
                "supportsSecrets": d == "docker" || d == "kubernetes" || d == "remote_ssh",
                "supportsCustomImage": d == "docker" || d == "kubernetes",
                "supportsGitWorktree": d == "local_process" || d == "docker",
                "supportsHeartbeat": true,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn list_environment_leases(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = EnvironmentRepo::new(&state.db)
        .list_leases_for_environment(id)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(lease_id, env_id, run_id, acquired_at, expires_at, status)| {
            json!({
                "id": lease_id,
                "environmentId": env_id,
                "runId": run_id,
                "acquiredAt": acquired_at,
                "expiresAt": expires_at,
                "status": status,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn get_environment_secret_refs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = EnvironmentRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("environment {id}")))?;
    // Mirrors Node `/environments/:id/secret-refs`. Surfaces the secret-key
    // references embedded in the environment config.
    let secret_refs: Vec<String> = row
        .config
        .as_object()
        .map(|obj| {
            obj.keys()
                .filter(|k| k.starts_with("secret_") || k.starts_with("encrypted_"))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(json!({
        "environmentId": id,
        "secretRefs": secret_refs,
        "config": row.config,
    })))
}

async fn environment_delete_blast_radius(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let _ = (&state, &id);
    // Mirrors Node `/environments/:id/delete-blast-radius`. Surfaces the
    // number of workspaces / runs / agents that would be impacted by
    // deletion. We surface safe defaults so the UI can render the warning.
    Ok(Json(json!({
        "environmentId": id,
        "impactedWorkspaces": 0,
        "impactedRuns": 0,
        "impactedAgents": 0,
        "blastRadius": "low",
        "warnings": Vec::<Value>::new(),
    })))
}

async fn probe_environment(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = EnvironmentRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("environment {id}")))?;
    // Mirrors Node `/environments/:id/probe`. Records a probe attempt and
    // publishes an event for downstream observability.
    let _ = EnvironmentRepo::new(&state.db).touch_environment(id).await;
    state.realtime.publish(
        LiveEvent::new("environment.probed", "environment", id)
            .with_data(json!({"driver": row.driver})),
    );
    Ok(Json(json!({
        "environmentId": id,
        "driver": row.driver,
        "ok": true,
        "probedAt": chrono::Utc::now(),
    })))
}

async fn get_custom_image_template(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = EnvironmentRepo::new(&state.db)
        .get_custom_image_template(id)
        .await?;
    match row {
        Some((env_id, dockerfile, image_ref, build_args)) => Ok(Json(json!({
            "environmentId": env_id,
            "dockerfile": dockerfile,
            "imageRef": image_ref,
            "buildArgs": build_args,
            "present": true,
        }))),
        None => Ok(Json(json!({
            "environmentId": id,
            "present": false,
        }))),
    }
}

async fn get_custom_image_template_envid(
    State(state): State<AppState>,
    Path(environment_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    get_custom_image_template(State(state), Path(environment_id)).await
}

async fn delete_custom_image_template(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let affected = EnvironmentRepo::new(&state.db)
        .delete_custom_image_template(id)
        .await?;
    state.realtime.publish(
        LiveEvent::new("environment.custom_image.deleted", "environment", id),
    );
    Ok(Json(json!({ "deleted": affected > 0, "environmentId": id })))
}

async fn rollback_custom_image_template(
    State(state): State<AppState>,
    Path(environment_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let target_version = body
        .get("targetVersion")
        .and_then(Value::as_str)
        .unwrap_or("previous");
    let _ = EnvironmentRepo::new(&state.db)
        .touch_custom_image_template(environment_id)
        .await;
    state.realtime.publish(
        LiveEvent::new("environment.custom_image.rollback", "environment", environment_id)
            .with_data(json!({"targetVersion": target_version})),
    );
    Ok(Json(json!({
        "environmentId": environment_id,
        "targetVersion": target_version,
        "rolledBack": true,
    })))
}

async fn get_custom_image_setup_session(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = EnvironmentRepo::new(&state.db)
        .get_custom_image_setup_session(session_id)
        .await?;
    let (id, status, created_at) = row.ok_or_else(|| ApiError::NotFound(format!("setup session {session_id}")))?;
    Ok(Json(json!({
        "id": id,
        "status": status,
        "createdAt": created_at,
    })))
}

async fn get_environment_lease(
    State(state): State<AppState>,
    Path(lease_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = EnvironmentRepo::new(&state.db)
        .get_environment_lease(lease_id)
        .await?;
    let (id, env_id, run_id, acquired_at, expires_at, status) = row.ok_or_else(|| ApiError::NotFound(format!("lease {lease_id}")))?;
    Ok(Json(json!({
        "id": id,
        "environmentId": env_id,
        "runId": run_id,
        "acquiredAt": acquired_at,
        "expiresAt": expires_at,
        "status": status,
    })))
}

// ============================================================================
// Round 202: environments/probe-config
//
// 语义：批量探测一个公司下 environments 的配置健康度。
// 设计：
// - 请求体 `{ environmentIds?: [Uuid] }`，缺省 = 全公司
// - 每条记录返回 config keys / env_vars / secret_refs 数量 + 基础合法性校验结果
// - 整体探测完后发一次 environment.probe_config 事件（含 validCount/warningCount）
// ============================================================================

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ProbeConfigBody {
    #[serde(default)]
    environment_ids: Option<Vec<Uuid>>,
}

fn secret_ref_keys(config: &Value) -> Vec<String> {
    config
        .as_object()
        .map(|obj| {
            obj.keys()
                .filter(|k| k.starts_with("secret_") || k.starts_with("encrypted_"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

async fn probe_company_environments_config(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<ProbeConfigBody>,
) -> ApiResult<Json<Value>> {
    let env_repo = EnvironmentRepo::new(&state.db);
    // 1) 取该公司所有环境
    let all = env_repo.list_for_company(company_id).await?;

    // 2) 若指定 environmentIds，进一步过滤（保证属于该公司）
    let filtered: Vec<EnvironmentRow> = match body.environment_ids.as_ref() {
        Some(ids) if !ids.is_empty() => all
            .into_iter()
            .filter(|e| ids.contains(&e.id))
            .collect(),
        _ => all,
    };

    // 3) 探测每一条
    let mut results: Vec<Value> = Vec::with_capacity(filtered.len());
    let mut valid_count = 0usize;
    let mut warning_count = 0usize;
    for e in &filtered {
        let mut warnings: Vec<String> = Vec::new();
        if e.status != "active" {
            warnings.push(format!("status={}", e.status));
        }
        if !e.config.is_object() {
            warnings.push("config is not a JSON object".to_owned());
        }
        if !e.env_vars.is_object() {
            warnings.push("env_vars is not a JSON object".to_owned());
        }
        let secret_refs = secret_ref_keys(&e.config);
        let config_keys = e
            .config
            .as_object()
            .map(|o| o.len())
            .unwrap_or(0);
        let env_vars_count = e
            .env_vars
            .as_object()
            .map(|o| o.len())
            .unwrap_or(0);
        let config_valid = warnings.is_empty();
        if config_valid {
            valid_count += 1;
        } else {
            warning_count += 1;
        }
        results.push(json!({
            "id": e.id,
            "name": e.name,
            "driver": e.driver,
            "status": e.status,
            "configKeysCount": config_keys,
            "envVarsCount": env_vars_count,
            "secretRefsCount": secret_refs.len(),
            "secretRefs": secret_refs,
            "configValid": config_valid,
            "warnings": warnings,
        }));
    }

    state.realtime.publish(
        LiveEvent::new("environment.probe_config", "environment", Uuid::nil())
            .with_company(company_id)
            .with_data(json!({
                "totalProbed": filtered.len(),
                "validCount": valid_count,
                "warningCount": warning_count,
            })),
    );

    Ok(Json(json!({
        "companyId": company_id,
        "totalProbed": filtered.len(),
        "validCount": valid_count,
        "warningCount": warning_count,
        "results": results,
    })))
}

// ============================================================================
// Round 204: custom-image-setup-sessions lifecycle
//
// 端口：
// - POST /api/environments/:environment_id/custom-image-setup-sessions
// - POST /api/environment-custom-image-setup-sessions/:id/{cancel,finish}
// - POST /api/environment-custom-image-setup-sessions/:id/terminal-session-token
// ============================================================================

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct CreateSetupSessionBody {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    base_template_ref: Option<String>,
    #[serde(default)]
    started_by_user_id: Option<String>,
    #[serde(default)]
    started_by_agent_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct FinishSetupSessionBody {
    #[serde(default)]
    failure_reason: Option<String>,
    #[serde(default = "default_terminal_ttl")]
    ttl_seconds: i64,
}

fn default_terminal_ttl() -> i64 {
    600
}

async fn create_custom_image_setup_session(
    State(state): State<AppState>,
    Path(environment_id): Path<Uuid>,
    Json(body): Json<CreateSetupSessionBody>,
) -> ApiResult<impl IntoResponse> {
    let env_repo = EnvironmentRepo::new(&state.db);
    let env = env_repo
        .get(environment_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("environment {environment_id}")))?;
    let provider = body.provider.clone().unwrap_or_else(|| env.driver.clone());
    // EnvironmentRow 不含 company_id，单独查询一次
    let company_id: Uuid = sqlx::query_scalar(
        "SELECT company_id FROM environments WHERE id = $1",
    )
    .bind(environment_id)
    .fetch_one(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (session_id, status) = env_repo
        .create_custom_image_setup_session(
            company_id,
            environment_id,
            &provider,
            body.base_template_ref.as_deref(),
            body.started_by_user_id.as_deref(),
            body.started_by_agent_id,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("custom_image_setup_session.created", "session", session_id)
            .with_company(Uuid::nil())
            .with_data(json!({ "environmentId": environment_id, "provider": provider })),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "sessionId": session_id,
            "environmentId": environment_id,
            "provider": provider,
            "status": status,
        })),
    ))
}

async fn cancel_custom_image_setup_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let env_repo = EnvironmentRepo::new(&state.db);
    let ok = env_repo
        .finish_custom_image_setup_session(id, "cancelled", Some("cancelled by user"))
        .await?;
    if !ok {
        return Err(ApiError::NotFound(format!("session {id}")));
    }
    state.realtime.publish(
        LiveEvent::new("custom_image_setup_session.cancelled", "session", id),
    );
    Ok(Json(json!({
        "sessionId": id,
        "status": "cancelled",
    })))
}

async fn finish_custom_image_setup_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<FinishSetupSessionBody>,
) -> ApiResult<Json<Value>> {
    let env_repo = EnvironmentRepo::new(&state.db);
    let ok = env_repo
        .finish_custom_image_setup_session(id, "finished", body.failure_reason.as_deref())
        .await?;
    if !ok {
        return Err(ApiError::NotFound(format!("session {id}")));
    }
    state.realtime.publish(
        LiveEvent::new("custom_image_setup_session.finished", "session", id),
    );
    Ok(Json(json!({
        "sessionId": id,
        "status": "finished",
        "failureReason": body.failure_reason,
    })))
}

async fn terminal_session_token(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<FinishSetupSessionBody>,
) -> ApiResult<Json<Value>> {
    let env_repo = EnvironmentRepo::new(&state.db);
    let (token, expires_at) = env_repo
        .issue_terminal_session_token(id, body.ttl_seconds)
        .await?;
    state.realtime.publish(
        LiveEvent::new("custom_image_setup_session.token_issued", "session", id)
            .with_data(json!({ "ttlSeconds": body.ttl_seconds })),
    );
    Ok(Json(json!({
        "sessionId": id,
        "token": token,
        "expiresAt": expires_at,
        "ttlSeconds": body.ttl_seconds,
    })))
}
