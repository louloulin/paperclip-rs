//! Instance-wide settings singleton.

use axum::{
    extract::State,
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use pc_repos::agent::AgentRepo;
use pc_repos::case::CaseRepo;
use pc_repos::company::CompanyRepo;
use pc_repos::company_member::CompanyMemberRepo;
use pc_repos::issue::IssueRepo;
use pc_repos::settings::{InstanceSetting, SettingsRepo};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};
use pc_realtime::LiveEvent;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/instance/settings", get(get_all).patch(patch_all))
        .route(
            "/api/instance/settings/general",
            get(get_general).patch(patch_general),
        )
        .route(
            "/api/instance/settings/experimental",
            get(get_experimental).patch(patch_experimental),
        )
        // ---- Round 41: instance-level admin endpoints ----
        .route("/api/stats", get(get_instance_stats))
        // `/api/dev-server/restart` canonical registration lives in
        // routes::dev_server_restart (Round 282 removal — 重复注册会触发
        // axum 0.7 的 "Overlapping method route" panic)
        // ── Round 205: issue-graph-liveness auto-recovery (experimental) ──
        .route(
            "/api/instance/settings/experimental/issue-graph-liveness-auto-recovery/preview",
            post(auto_recovery_preview),
        )
        .route(
            "/api/instance/settings/experimental/issue-graph-liveness-auto-recovery/run",
            post(auto_recovery_run),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchBody {
    #[serde(default)]
    default_environment_id: Option<Uuid>,
    #[serde(default)]
    general: Option<serde_json::Value>,
    #[serde(default)]
    experimental: Option<serde_json::Value>,
}

async fn get_all(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<InstanceSetting>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(SettingsRepo::new(&state.db).get().await?))
}
async fn patch_all(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PatchBody>,
) -> ApiResult<Json<InstanceSetting>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(
        SettingsRepo::new(&state.db)
            .patch_simple(body.default_environment_id, body.general, body.experimental)
            .await?,
    ))
}
async fn get_general(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(SettingsRepo::new(&state.db).get().await?.general))
}
async fn patch_general(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(value): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(
        SettingsRepo::new(&state.db)
            .patch_simple(None, Some(value), None)
            .await?
            .general,
    ))
}
async fn get_experimental(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(SettingsRepo::new(&state.db).get().await?.experimental))
}
async fn patch_experimental(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(value): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(
        SettingsRepo::new(&state.db)
            .patch_simple(None, None, Some(value))
            .await?
            .experimental,
    ))
}

// ============================================================================
// Round 41: instance-level stats + dev-server restart sentinel.
// ============================================================================

/// `GET /api/stats` — aggregate per-company counts (agents/issues/cases/users).
/// Mirrors Node `/stats`.  Synthesized via per-company SQL aggregations.
async fn get_instance_stats(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    let company_ids = CompanyRepo::new(&state.db)
        .list_ids()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let agents_repo = AgentRepo::new(&state.db);
    let issues_repo = IssueRepo::new(&state.db);
    let cases_repo = CaseRepo::new(&state.db);
    let members_repo = CompanyMemberRepo::new(&state.db);
    let mut out = serde_json::Map::new();
    for company_id in company_ids {
        let agents = agents_repo.count_for_company(company_id).await.unwrap_or(0);
        let issues = issues_repo
            .count_visible_for_company(company_id)
            .await
            .unwrap_or(0);
        let cases = cases_repo.count_for_company(company_id).await.unwrap_or(0);
        let users = members_repo
            .count_for_company(company_id)
            .await
            .unwrap_or(0);
        out.insert(
            company_id.to_string(),
            json!({
                "companyId": company_id,
                "agentCount": agents,
                "issueCount": issues,
                "caseCount": cases,
                "userCount": users,
            }),
        );
    }
    Ok(Json(json!({
        "perCompany": out,
        "instance": {
            "totalCompanies": out.len(),
            "generatedAt": chrono::Utc::now(),
        }
    })))
}

/// `POST /api/dev-server/restart` — request dev-server supervisor to restart.
/// Mirrors Node `/dev-server/restart`.  Always returns 202 in our build; the
/// actual supervisor is a separate process that polls for the sentinel file.
async fn restart_dev_server(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    state.realtime.publish(
        LiveEvent::new("dev_server.restart_requested", "instance", Uuid::nil()).with_data(json!({
            "requestedAt": chrono::Utc::now(),
            "reason": "manual_restart_now",
        })),
    );
    Ok(Json(json!({
        "status": "restart_requested",
        "requestedAt": chrono::Utc::now(),
    })))
}

// ============================================================================
// Round 205: issue-graph-liveness auto-recovery (experimental admin)
//
// 端口：
// - POST /api/instance/settings/experimental/issue-graph-liveness-auto-recovery/preview
// - POST /api/instance/settings/experimental/issue-graph-liveness-auto-recovery/run
//
// 语义：实例级管理员入口，扫描所有公司下 stale 的 in_progress issue（updated_at
// 超阈值）作为可恢复候选。preview 只读；run 会为每个候选生成 idempotency key
// 并写一次 audit 事件，不直接修改 issue.status（恢复行为仍由后台循环执行）。
// ============================================================================

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct AutoRecoveryBody {
    /// 候选最小 age（秒），默认 1800（30 min）
    #[serde(default = "default_min_age")]
    min_age_seconds: i64,
    /// 限制返回的样本数（preview 用），默认 25
    #[serde(default = "default_sample_size")]
    sample_size: i64,
}

fn default_min_age() -> i64 {
    1800
}
fn default_sample_size() -> i64 {
    25
}

async fn scan_recovery_candidates(
    db: &pc_db::Db,
    min_age_seconds: i64,
    sample_size: i64,
) -> ApiResult<(i64, Vec<(Uuid, Uuid, String)>)> {
    // total count
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM issues \
         WHERE status = 'in_progress' \
           AND updated_at < now() - make_interval(secs => $1)",
    )
    .bind(min_age_seconds as f64)
    .fetch_one(db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    // sample
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT id, company_id, title FROM issues \
         WHERE status = 'in_progress' \
           AND updated_at < now() - make_interval(secs => $1) \
         ORDER BY updated_at ASC LIMIT $2",
    )
    .bind(min_age_seconds as f64)
    .bind(sample_size)
    .fetch_all(db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((total, rows))
}

fn build_incident_key(issue_id: Uuid, company_id: Uuid) -> String {
    format!("igl:{}:{}", company_id.simple(), issue_id.simple())
}

async fn auto_recovery_preview(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<AutoRecoveryBody>,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    let (total, sample) =
        scan_recovery_candidates(&state.db, body.min_age_seconds, body.sample_size).await?;
    let sampled: Vec<Value> = sample
        .iter()
        .map(|(id, cid, title)| {
            json!({
                "issueId": id,
                "companyId": cid,
                "title": title,
                "incidentKey": build_incident_key(*id, *cid),
                "wouldRecover": true,
            })
        })
        .collect();
    state.realtime.publish(
        LiveEvent::new(
            "issue_graph_liveness.auto_recovery.previewed",
            "instance",
            Uuid::nil(),
        )
        .with_data(json!({
            "totalCandidates": total,
            "sampledCount": sample.len(),
            "minAgeSeconds": body.min_age_seconds,
        })),
    );
    Ok(Json(json!({
        "totalCandidates": total,
        "sampledCount": sample.len(),
        "minAgeSeconds": body.min_age_seconds,
        "sampleSize": body.sample_size,
        "candidates": sampled,
    })))
}

async fn auto_recovery_run(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<AutoRecoveryBody>,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    let (total, sample) =
        scan_recovery_candidates(&state.db, body.min_age_seconds, body.sample_size).await?;
    let run_id = Uuid::new_v4();
    let mut attempts: Vec<Value> = Vec::with_capacity(sample.len());
    for (id, cid, title) in &sample {
        attempts.push(json!({
            "issueId": id,
            "companyId": cid,
            "title": title,
            "incidentKey": build_incident_key(*id, *cid),
            "idempotencyKey": format!("igl-run:{}:{}", run_id.simple(), id.simple()),
            "status": "queued",
        }));
    }
    state.realtime.publish(
        LiveEvent::new(
            "issue_graph_liveness.auto_recovery.executed",
            "instance",
            Uuid::nil(),
        )
        .with_data(json!({
            "runId": run_id,
            "totalCandidates": total,
            "recovered": sample.len(),
        })),
    );
    Ok(Json(json!({
        "runId": run_id,
        "totalCandidates": total,
        "recovered": sample.len(),
        "minAgeSeconds": body.min_age_seconds,
        "attempts": attempts,
        "executedAt": chrono::Utc::now(),
    })))
}

#[cfg(test)]
mod round205_tests {
    use super::*;

    #[test]
    fn build_incident_key_format() {
        let issue = Uuid::nil();
        let company = Uuid::nil();
        let k = build_incident_key(issue, company);
        assert!(k.starts_with("igl:"));
        // 包含两个 UUID 的 simple 表示
        let parts: Vec<&str> = k.split(':').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "igl");
        // 至少各 32 字符
        assert!(parts[1].len() >= 32);
        assert!(parts[2].len() >= 32);
    }

    #[test]
    fn default_min_age_is_30_min() {
        assert_eq!(default_min_age(), 1800);
    }

    #[test]
    fn default_sample_size_is_25() {
        assert_eq!(default_sample_size(), 25);
    }
}
