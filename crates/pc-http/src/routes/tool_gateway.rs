//! Tool gateway (MCP 风格的工具代理)。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/mcp/gateways/:gateway_public_id",
            get(get_gateway).post(post_gateway),
        )
        .route(
            "/api/companies/:company_id/tools/gateways",
            get(list_gateways).post(create_gateway),
        )
        .route(
            "/api/tool-gateway/gateways/:gateway_id",
            patch(patch_gateway),
        )
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct GatewayBody {
    name: Option<String>,
    description: Option<String>,
    public_id: Option<String>,
}

async fn get_gateway(State(_s): State<AppState>, Path(public_id): Path<String>) -> Json<Value> {
    Json(json!({ "publicId": public_id, "kind": "tool-gateway" }))
}

async fn post_gateway(
    State(_s): State<AppState>,
    Path(public_id): Path<String>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = public_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "publicId": public_id, "status": "received" })),
    )
}

async fn list_gateways(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "items": [] }))
}

async fn create_gateway(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<GatewayBody>,
) -> impl IntoResponse {
    let _ = company_id;
    (StatusCode::CREATED, Json(json!({ "id": "gw_new" })))
}

async fn patch_gateway(
    State(_s): State<AppState>,
    Path(gateway_id): Path<String>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = gateway_id;
    (
        StatusCode::OK,
        Json(json!({ "id": gateway_id, "updated": true })),
    )
}
