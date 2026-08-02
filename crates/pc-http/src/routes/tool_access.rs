//! 工具访问：connections、tool gallery、connections OAuth。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/agents/me/connections/:connection_id/start-authorization",
            post(start_connection_authz),
        )
        .route(
            "/api/agents/me/connections/:connection_id/token",
            post(connection_token),
        )
        .route(
            "/api/companies/:company_id/tools/gallery",
            get(tool_gallery),
        )
        .route(
            "/api/companies/:company_id/tools/apps/connect",
            post(connect_tool_app),
        )
        .route(
            "/api/companies/:company_id/tools/connections/:connection_id/start-authorization",
            post(start_company_connection_authz),
        )
        .route("/api/tools/oauth/:connection_id/start", post(oauth_start))
        .route("/api/tools/oauth/callback", get(oauth_callback))
        .route(
            "/api/companies/:company_id/tools/apps/:connection_id/finish",
            post(finish_oauth),
        )
        .route(
            "/api/companies/:company_id/tools/connections",
            get(list_connections).post(create_connection),
        )
        .route(
            "/api/companies/:company_id/tools/connections/:connection_id",
            delete(delete_connection)
                .get(get_connection)
                .put(update_connection),
        )
        .route(
            "/api/companies/:company_id/tools/categories",
            get(tool_categories),
        )
        .route("/api/companies/:company_id/tools/lookup", post(tool_lookup))
        .route(
            "/api/companies/:company_id/tools/:tool_id",
            get(get_tool).delete(delete_tool),
        )
        .route(
            "/api/companies/:company_id/tools/:tool_id/invoke",
            post(invoke_tool),
        )
        .route(
            "/api/companies/:company_id/tools/invocations",
            get(list_invocations),
        )
}

async fn start_connection_authz(
    State(_s): State<AppState>,
    Path(connection_id): Path<String>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({
        "connectionId": connection_id,
        "authorizationUrl": null
    }))
}

async fn connection_token(
    State(_s): State<AppState>,
    Path(connection_id): Path<String>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({
        "connectionId": connection_id,
        "token": "stub-token"
    }))
}

async fn tool_gallery(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "items": [] }))
}

async fn connect_tool_app(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = company_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "status": "connect-queued" })),
    )
}

async fn start_company_connection_authz(
    State(_s): State<AppState>,
    Path((_company_id, connection_id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({
        "connectionId": connection_id,
        "authorizationUrl": null
    }))
}

async fn oauth_start(
    State(_s): State<AppState>,
    Path(connection_id): Path<String>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = connection_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "status": "oauth-started" })),
    )
}

async fn oauth_callback(State(_s): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html")],
        "<html><body>OAuth callback received</body></html>".to_string(),
    )
}

async fn finish_oauth(
    State(_s): State<AppState>,
    Path((_company_id, _connection_id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "finished": true })))
}

async fn list_connections(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "items": [] }))
}

async fn create_connection(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = company_id;
    (StatusCode::CREATED, Json(json!({ "id": "conn_new" })))
}

async fn get_connection(
    State(_s): State<AppState>,
    Path((_company_id, connection_id)): Path<(Uuid, String)>,
) -> Json<Value> {
    Json(json!({ "id": connection_id }))
}

async fn update_connection(
    State(_s): State<AppState>,
    Path((_company_id, connection_id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = connection_id;
    (StatusCode::OK, Json(json!({ "updated": true })))
}

async fn delete_connection(
    State(_s): State<AppState>,
    Path((_company_id, connection_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let _ = connection_id;
    (StatusCode::NO_CONTENT, Json(json!({ "deleted": true })))
}

async fn tool_categories(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "items": [] }))
}

async fn tool_lookup(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = company_id;
    (StatusCode::OK, Json(json!({ "tools": [] })))
}

async fn get_tool(
    State(_s): State<AppState>,
    Path((_company_id, tool_id)): Path<(Uuid, String)>,
) -> Json<Value> {
    Json(json!({ "id": tool_id }))
}

async fn delete_tool(
    State(_s): State<AppState>,
    Path((_company_id, tool_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let _ = tool_id;
    (StatusCode::NO_CONTENT, Json(json!({ "deleted": true })))
}

async fn invoke_tool(
    State(_s): State<AppState>,
    Path((_company_id, tool_id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = tool_id;
    (StatusCode::OK, Json(json!({ "result": null })))
}

async fn list_invocations(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "items": [] }))
}
