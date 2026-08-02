/// Tool gateway (MCP 网关)。
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
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
            "/api/companies/:company_id/tools/gateways",
            get(list_gateways).post(create_gateway),
        )
        .route(
            "/api/tool-gateway/gateways/:gateway_id",
            get(get_gateway).post(post_gateway),
        )
}

#[derive(Debug, FromRow)]
struct McpGatewayRow {
    id: Uuid,
    company_id: Uuid,
    name: String,
    slug: String,
    description: Option<String>,
    status: String,
    profile_id: Uuid,
    agent_id: Option<Uuid>,
    project_id: Option<Uuid>,
    issue_id: Option<Uuid>,
    metadata: Value,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

fn gateway_json(row: &McpGatewayRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "name": row.name,
        "slug": row.slug,
        "description": row.description,
        "status": row.status,
        "profileId": row.profile_id,
        "agentId": row.agent_id,
        "projectId": row.project_id,
        "issueId": row.issue_id,
        "metadata": row.metadata,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

async fn list_gateways(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<McpGatewayRow> = sqlx::query_as(
        "SELECT id, company_id, name, slug, description, status, profile_id, \
         agent_id, project_id, issue_id, metadata, created_at, updated_at \
         FROM tool_mcp_gateways WHERE company_id = $1 ORDER BY created_at DESC",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(gateway_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateGatewayBody {
    name: String,
    slug: Option<String>,
    description: Option<String>,
    profile_id: Uuid,
    agent_id: Option<Uuid>,
    project_id: Option<Uuid>,
    issue_id: Option<Uuid>,
}

async fn create_gateway(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateGatewayBody>,
) -> ApiResult<impl IntoResponse> {
    let slug = body
        .slug
        .clone()
        .unwrap_or_else(|| body.name.to_lowercase().replace(' ', "-"));
    let row: McpGatewayRow = sqlx::query_as(
        "INSERT INTO tool_mcp_gateways \
         (company_id, name, slug, description, profile_id, agent_id, project_id, issue_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id, company_id, name, slug, description, status, profile_id, \
                   agent_id, project_id, issue_id, metadata, created_at, updated_at",
    )
    .bind(company_id)
    .bind(&body.name)
    .bind(&slug)
    .bind(&body.description)
    .bind(body.profile_id)
    .bind(body.agent_id)
    .bind(body.project_id)
    .bind(body.issue_id)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::CREATED, Json(gateway_json(&row))))
}

async fn get_gateway(
    State(state): State<AppState>,
    Path(gateway_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<McpGatewayRow> = sqlx::query_as(
        "SELECT id, company_id, name, slug, description, status, profile_id, \
         agent_id, project_id, issue_id, metadata, created_at, updated_at \
         FROM tool_mcp_gateways WHERE id = $1",
    )
    .bind(gateway_id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(gateway_json(&row))),
        None => Err(ApiError::NotFound(format!("gateway {gateway_id}"))),
    }
}

async fn post_gateway(
    State(state): State<AppState>,
    Path(gateway_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // MCP-style POST: forward/process request
    let row: Option<McpGatewayRow> = sqlx::query_as(
        "SELECT id, company_id, name, slug, description, status, profile_id, \
         agent_id, project_id, issue_id, metadata, created_at, updated_at \
         FROM tool_mcp_gateways WHERE id = $1 AND status = 'active'",
    )
    .bind(gateway_id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(_gateway) => {
            // In a full implementation this would forward the MCP request to the backend
            Ok(Json(json!({
                "gatewayId": gateway_id,
                "received": true,
                "method": body.get("method"),
                "status": "forwarded"
            })))
        }
        None => Err(ApiError::NotFound(format!("gateway {gateway_id}"))),
    }
}
