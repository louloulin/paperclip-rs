//! Teams catalog (团队/技能目录) — 浏览、安装、预览。
//!
//! 通过 `include_str!` 把 `packages/teams-catalog/generated/catalog.json` 编进
//! 二进制里，避免运行时去读文件系统。安装/卸载写回 `teams_installs` 表。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/teams/catalog", get(list_teams_catalog))
        .route("/api/teams/catalog/:catalog_id/files", get(catalog_files))
        .route("/api/teams/catalog/:catalog_id", get(catalog_detail))
        .route(
            "/api/companies/:company_id/teams/catalog/installed",
            get(installed_teams),
        )
        .route(
            "/api/companies/:company_id/teams/catalog/:catalog_id/preview",
            get(catalog_preview),
        )
        .route(
            "/api/companies/:company_id/teams/catalog/:catalog_id/install",
            post(install_team),
        )
        .route(
            "/api/companies/:company_id/teams/catalog/:catalog_id/uninstall",
            post(uninstall_team),
        )
}

const CATALOG_JSON: &str = include_str!("../../../../packages/teams-catalog/generated/catalog.json");

fn load_catalog() -> Result<Value, ApiError> {
    serde_json::from_str(CATALOG_JSON).map_err(|e| ApiError::Internal(format!("catalog: {e}")))
}

fn find_team<'a>(catalog: &'a Value, catalog_id: &str) -> Option<&'a Value> {
    catalog
        .get("teams")
        .and_then(|t| t.as_array())
        .and_then(|teams| {
            teams
                .iter()
                .find(|t| {
                    t.get("key").and_then(|v| v.as_str()) == Some(catalog_id)
                        || t.get("id").and_then(|v| v.as_str()) == Some(catalog_id)
                })
        })
}

async fn list_teams_catalog() -> ApiResult<Json<Value>> {
    let catalog = load_catalog()?;
    Ok(Json(json!({
        "schemaVersion": catalog.get("schemaVersion"),
        "packageVersion": catalog.get("packageVersion"),
        "items": catalog.get("teams").cloned().unwrap_or(json!([]))
    })))
}

async fn catalog_files(
    Path(catalog_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let catalog = load_catalog()?;
    let team = find_team(&catalog, &catalog_id)
        .ok_or_else(|| ApiError::NotFound(format!("catalog {catalog_id}")))?;
    let path = team.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let entry = team.get("entrypoint").and_then(|v| v.as_str()).unwrap_or("TEAM.md");
    Ok(Json(json!({
        "catalogId": catalog_id,
        "path": path,
        "entrypoint": entry,
        "files": []
    })))
}

async fn catalog_detail(
    Path(catalog_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let catalog = load_catalog()?;
    let team = find_team(&catalog, &catalog_id)
        .ok_or_else(|| ApiError::NotFound(format!("catalog {catalog_id}")))?;
    Ok(Json(team.clone()))
}

async fn installed_teams(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, Option<String>, serde_json::Value, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as(
            "SELECT catalog_id, status, snapshot, installed_at FROM team_installs \
             WHERE company_id = $1 ORDER BY installed_at DESC",
        )
        .bind(company_id)
        .fetch_all(state.db.pool())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, status, snap, ts)| {
            json!({
                "catalogId": id,
                "status": status,
                "snapshot": snap,
                "installedAt": ts,
            })
        })
        .collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn catalog_preview(
    Path((company_id, catalog_id)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let catalog = load_catalog()?;
    let team = find_team(&catalog, &catalog_id)
        .ok_or_else(|| ApiError::NotFound(format!("catalog {catalog_id}")))?;
    Ok(Json(json!({
        "companyId": company_id,
        "catalogId": catalog_id,
        "preview": {
            "name": team.get("name"),
            "description": team.get("description"),
            "agents": team.get("agentSlugs"),
            "projects": team.get("projectSlugs"),
            "routines": team.get("counts").and_then(|c| c.get("routines")),
            "skills": team.get("requiredSkills"),
        }
    })))
}

async fn install_team(
    State(state): State<AppState>,
    Path((company_id, catalog_id)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    let catalog = load_catalog()?;
    let team = find_team(&catalog, &catalog_id)
        .ok_or_else(|| ApiError::NotFound(format!("catalog {catalog_id}")))?
        .clone();
    let _ = sqlx::query(
        "INSERT INTO team_installs (company_id, catalog_id, status, snapshot, installed_at) \
         VALUES ($1, $2, 'queued', $3, now()) \
         ON CONFLICT (company_id, catalog_id) DO UPDATE SET status='queued', snapshot=EXCLUDED.snapshot, updated_at=now()",
    )
    .bind(company_id)
    .bind(&catalog_id)
    .bind(&team)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "catalogId": catalog_id,
            "status": "install-queued"
        })),
    ))
}

async fn uninstall_team(
    State(state): State<AppState>,
    Path((company_id, catalog_id)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    let _ = sqlx::query(
        "DELETE FROM team_installs WHERE company_id = $1 AND catalog_id = $2",
    )
    .bind(company_id)
    .bind(&catalog_id)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::NO_CONTENT,
        Json(json!({
            "companyId": company_id,
            "catalogId": catalog_id,
            "status": "uninstalled"
        })),
    ))
}
