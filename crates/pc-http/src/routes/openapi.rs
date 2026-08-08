//! `OpenAPI 3` 文档端点。

use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        // Canonical mount points used by the Rust server itself.
        .route("/openapi.json", get(document))
        .route("/api/openapi", get(document))
        // Alias matching the Node upstream contract (`/api/openapi.json`) so
        // parity tests and shared OpenAPI consumers can use one URL.
        .route("/api/openapi.json", get(document))
}

async fn document(State(state): State<AppState>) -> Json<Value> {
    let mut paths = serde_json::Map::new();
    for (path, method, operation_id) in [
        ("/health", "get", "health"),
        ("/api/auth/sign-in", "post", "signIn"),
        ("/api/companies", "get", "listCompanies"),
        ("/api/agents", "get", "listAgents"),
        ("/api/issues", "get", "listIssues"),
        ("/api/projects", "get", "listProjects"),
        ("/api/companies/{company_id}/dashboard", "get", "dashboard"),
        (
            "/api/companies/{company_id}/costs/summary",
            "get",
            "costSummary",
        ),
        (
            "/api/companies/{company_id}/resource-memberships/me",
            "get",
            "resourceMemberships",
        ),
        (
            "/api/companies/{company_id}/users/{user_slug}/profile",
            "get",
            "userProfile",
        ),
    ] {
        paths.entry(path.to_owned()).or_insert_with(|| json!({}))[method]["operationId"] =
            json!(operation_id);
    }
    let adapters = state
        .adapters
        .descriptors()
        .into_iter()
        .map(|descriptor| descriptor.adapter_type)
        .collect::<Vec<_>>();
    Json(json!({
        "openapi": "3.0.0",
        "info": {
            "title": "Paperclip API",
            "version": "0.1.0",
            "description": "REST API for the Paperclip AI agent management platform"
        },
        "servers": [{ "url": "/" }],
        "paths": paths,
        "x-paperclip": { "adapters": adapters }
    }))
}
