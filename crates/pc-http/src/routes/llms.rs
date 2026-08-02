//! 面向 Agent 的纯文本配置文档端点。

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    routing::get,
    Router,
};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/llms/agent-configuration.txt", get(configuration_index))
        .route("/llms/agent-icons.txt", get(agent_icons))
        .route(
            "/llms/agent-configuration/:adapter_type.txt",
            get(configuration_for_adapter),
        )
        .route("/api/llms", get(configuration_index))
}

fn text_response(body: String) -> ([(header::HeaderName, &'static str); 1], String) {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body)
}

async fn configuration_index(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    let mut lines = vec![
        "# Paperclip Agent Configuration Index".to_owned(),
        String::new(),
        "Installed adapters:".to_owned(),
    ];
    for descriptor in state.adapters.descriptors() {
        lines.push(format!(
            "- {}: /llms/agent-configuration/{}.txt",
            descriptor.adapter_type, descriptor.adapter_type
        ));
    }
    lines.extend([
        String::new(),
        "Related API endpoints:".to_owned(),
        "- GET /api/companies/:companyId/agent-configurations".to_owned(),
        "- GET /api/agents/:id/configuration".to_owned(),
        "- POST /api/companies/:companyId/agent-hires".to_owned(),
        String::new(),
        "Sensitive values are redacted in configuration read APIs.".to_owned(),
    ]);
    text_response(lines.join("\n"))
}

async fn agent_icons() -> impl axum::response::IntoResponse {
    text_response(
        [
            "# Paperclip Agent Icon Names",
            "",
            "Use the `icon` field on agent create payloads.",
            "Common values: bot, code, search, shield, sparkles, terminal.",
        ]
        .join("\n"),
    )
}

async fn configuration_for_adapter(
    State(state): State<AppState>,
    Path(adapter_type): Path<String>,
) -> impl axum::response::IntoResponse {
    if let Some(descriptor) = state.adapters.descriptor(&adapter_type) {
        return (
            StatusCode::OK,
            text_response(format!(
                "# {} agent configuration\n\nAdapter label: {}\n\nConfigure adapter-specific values in adapterConfig and runtimeConfig.",
                descriptor.adapter_type, descriptor.label
            )),
        );
    }
    (
        StatusCode::NOT_FOUND,
        text_response(format!("Unknown adapter type: {adapter_type}")),
    )
}
