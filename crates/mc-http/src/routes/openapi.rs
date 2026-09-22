//! `/api/openapi.json` handler.

use axum::Json;
use serde_json::Value;

pub async fn openapi_json() -> Json<Value> {
    Json(
        mc_openapi::OpenApiSpec::minimal(
            "Multica-rs API",
            env!("CARGO_PKG_VERSION"),
            "http://127.0.0.1:3500",
        )
        .render(),
    )
}
