//! 董事会聊天（SSE 流）。

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use serde_json::{json, Value};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/board/chat/stream", post(board_chat_stream))
}

async fn board_chat_stream(
    State(_state): State<AppState>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    // SSE streaming is not implemented in this build; the UI may fall back
    // to one-shot /api/board/chat for a final answer.
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "accepted",
            "message": "board chat streaming not implemented in Rust build yet"
        })),
    )
}
