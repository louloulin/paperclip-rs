//! 董事会聊天（SSE 流）。
//!
//! 简化实现：通过 tokio::process 启动 `claude` CLI（配置中可换），
//! 用 SSE 流把 chunked 输出推给前端。完成后写入 issue comment 并
//! 通过 LiveEvent 总线广播。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};
use pc_repos::board_chat::{BoardChatRepo, ChatMessageStatus, ChatRole, NewMessage, NewThread};
use pc_repos::issue::IssueRepo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/board/chat/stream", post(board_chat_stream))
        .route("/api/board/chat", post(board_chat_one_shot))
        .route(
            "/api/companies/:company_id/board-chat/threads",
            get(list_threads),
        )
        .route(
            "/api/board/chat/threads/:thread_id/messages",
            get(list_messages),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BoardChatBody {
    company_id: Uuid,
    issue_id: Option<Uuid>,
    /// Board 发送的 user message.
    message: String,
    /// Optional override for the claude CLI command (defaults to env var or `claude`).
    #[serde(default)]
    claude_command: Option<String>,
}

#[derive(Debug, Serialize)]
struct SsePayload {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    issue_id: Option<Uuid>,
    exit_code: Option<i32>,
    message: Option<String>,
}

async fn board_chat_stream(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<BoardChatBody>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let _actor = require_user_id(&state, &headers).await?;

    let resolved_issue_id = match body.issue_id {
        Some(id) => id,
        None => ensure_board_issue(&state, body.company_id, "Board Operations").await?,
    };

    // Persist the user message into a board chat thread (idempotent).
    let _thread_id = persist_user_message(&state, body.company_id, Some(resolved_issue_id), &body.message).await?;

    let claude = body
        .claude_command
        .clone()
        .or_else(|| std::env::var("PAPERCLIP_CLAUDE_CMD").ok())
        .unwrap_or_else(|| "claude".into());

    let (tx, rx) = mpsc::channel::<String>(32);

    // Spawn the claude CLI subprocess.
    let message = body.message.clone();
    let company_id = body.company_id;
    let db = state.db.clone();
    tokio::spawn(async move {
        let mut cmd = Command::new(&claude);
        cmd.arg("--print")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--dangerously-skip-permissions")
            .env("PAPERCLIP_API_URL", "http://127.0.0.1:3100")
            .env("PAPERCLIP_COMPANY_ID", company_id.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(err) => {
                let _ = tx
                    .send(format!(
                        r#"{{"type":"error","message":"Failed to start {claude}: {err}"}}"#
                    ))
                    .await;
                return;
            }
        };

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(message.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }

        let stdout = child.stdout.take().unwrap();
        let mut reader = BufReader::new(stdout).lines();
        let mut full_response = String::new();
        while let Ok(Some(line)) = reader.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Try JSONL; if not, wrap.
            let event = if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                val
            } else {
                Value::String(trimmed.to_owned())
            };
            // Extract text fields for chunk streaming.
            let chunk = extract_text(&event).unwrap_or_default();
            if !chunk.is_empty() {
                full_response.push_str(&chunk);
                let payload = SsePayload {
                    kind: "chunk".into(),
                    text: Some(chunk),
                    issue_id: None,
                    exit_code: None,
                    message: None,
                };
                if let Ok(json) = serde_json::to_string(&payload) {
                    if tx.send(json).await.is_err() {
                        break;
                    }
                }
            } else if let Some(tool) = extract_tool_use(&event) {
                let payload = SsePayload {
                    kind: "status".into(),
                    text: Some(format!("Using {tool}...")),
                    issue_id: None,
                    exit_code: None,
                    message: None,
                };
                if let Ok(json) = serde_json::to_string(&payload) {
                    let _ = tx.send(json).await;
                }
            }
        }

        let exit = child.wait().await.ok().and_then(|s| s.code());
        if !full_response.is_empty() {
            // Persist assistant turn as a comment on the standing issue AND as a
            // board_chat_messages entry. We rely on the parent DB pool rather than
            // a fresh PgPool (avoids extra connection slot).
            let _ = IssueRepo::new(&db)
                .insert_comment_idempotent(
                    Uuid::new_v4(),
                    resolved_issue_id,
                    "board-concierge",
                    &full_response,
                )
                .await;
            // Also persist into the board_chat thread tied to the issue (if any).
            let repo = BoardChatRepo::new(&db);
            if let Ok(thread) = repo
                .get_or_create_thread(&NewThread {
                    company_id,
                    issue_id: Some(resolved_issue_id),
                    title: "Board Operations".into(),
                    created_by_user_id: None,
                })
                .await
            {
                let _ = repo
                    .append_message(&NewMessage {
                        thread_id: thread.id,
                        company_id,
                        role: ChatRole::Assistant,
                        author_user_id: None,
                        author_agent_id: None,
                        body: full_response.clone(),
                        tool_uses: None,
                        status: Some(ChatMessageStatus::Complete),
                    })
                    .await;
            }
        }

        let payload = SsePayload {
            kind: "done".into(),
            text: None,
            issue_id: Some(resolved_issue_id),
            exit_code: exit,
            message: None,
        };
        if let Ok(json) = serde_json::to_string(&payload) {
            let _ = tx.send(json).await;
        }
    });

    let sse_stream = async_stream::stream! {
        let mut receiver = rx;
        // Emit a "start" frame first.
        let start = SsePayload {
            kind: "start".into(),
            text: None,
            issue_id: Some(resolved_issue_id),
            exit_code: None,
            message: None,
        };
        if let Ok(json) = serde_json::to_string(&start) {
            yield Ok::<Event, Infallible>(Event::default().data(json));
        }
        while let Some(msg) = receiver.recv().await {
            yield Ok(Event::default().data(msg));
        }
    };

    Ok(Sse::new(sse_stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

async fn board_chat_one_shot(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<BoardChatBody>,
) -> ApiResult<Json<Value>> {
    let _actor = require_user_id(&state, &headers).await?;
    let resolved_issue_id = match body.issue_id {
        Some(id) => id,
        None => ensure_board_issue(&state, body.company_id, "Board Operations").await?,
    };
    let _thread_id = persist_user_message(&state, body.company_id, Some(resolved_issue_id), &body.message).await?;
    let claude = body
        .claude_command
        .clone()
        .or_else(|| std::env::var("PAPERCLIP_CLAUDE_CMD").ok())
        .unwrap_or_else(|| "claude".into());
    let output = Command::new(&claude)
        .arg("--print")
        .arg("--dangerously-skip-permissions")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match output {
        Ok(c) => c,
        Err(err) => {
            return Err(ApiError::Internal(format!(
                "Failed to start {claude}: {err}"
            )));
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(body.message.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| ApiError::Internal(format!("claude wait: {e}")))?;
    let response = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(Json(json!({
        "issueId": resolved_issue_id,
        "response": response,
        "exitCode": output.status.code(),
    })))
}

async fn ensure_board_issue(
    state: &AppState,
    company_id: Uuid,
    title: &str,
) -> Result<Uuid, ApiError> {
    BoardChatRepo::new(&state.db)
        .ensure_board_issue(company_id, title)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))
}


async fn list_threads(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = BoardChatRepo::new(&state.db);
    let rows = repo
        .list_threads(company_id, 100)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "companyId": r.company_id,
                "issueId": r.issue_id,
                "title": r.title,
                "status": r.status,
                "createdByUserId": r.created_by_user_id,
                "lastMessageAt": r.last_message_at,
                "createdAt": r.created_at,
                "updatedAt": r.updated_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "items": items,
    })))
}

async fn list_messages(
    State(state): State<AppState>,
    Path(thread_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = BoardChatRepo::new(&state.db);
    let rows = repo
        .list_messages(thread_id, 500)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "threadId": r.thread_id,
                "companyId": r.company_id,
                "role": r.role,
                "authorUserId": r.author_user_id,
                "authorAgentId": r.author_agent_id,
                "body": r.body,
                "toolUses": r.tool_uses,
                "status": r.status,
                "createdAt": r.created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "threadId": thread_id,
        "items": items,
    })))
}



/// Persist the user message into a board_chat_thread BEFORE starting the LLM turn.
/// Returns the thread id for downstream callers (so they can tie the assistant reply
/// to the same thread).
async fn persist_user_message(
    state: &AppState,
    company_id: Uuid,
    issue_id: Option<Uuid>,
    message: &str,
) -> Result<Uuid, ApiError> {
    let repo = BoardChatRepo::new(&state.db);
    let thread = repo
        .get_or_create_thread(&NewThread {
            company_id,
            issue_id,
            title: "Board Operations".into(),
            created_by_user_id: None,
        })
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    repo.append_message(&NewMessage {
        thread_id: thread.id,
        company_id,
        role: ChatRole::User,
        author_user_id: None,
        author_agent_id: None,
        body: message.to_string(),
        tool_uses: None,
        status: Some(ChatMessageStatus::Complete),
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(thread.id)
}
fn extract_text(event: &Value) -> Option<String> {
    if let Some(text) = event.get("text").and_then(|v| v.as_str()) {
        return Some(text.to_owned());
    }
    if let Some(text) = event
        .get("delta")
        .and_then(|d| d.get("text"))
        .and_then(|v| v.as_str())
    {
        return Some(text.to_owned());
    }
    if let Some(text) = event
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("text"))
        })
        .and_then(|b| b.get("text"))
        .and_then(|v| v.as_str())
    {
        return Some(text.to_owned());
    }
    None
}

fn extract_tool_use(event: &Value) -> Option<String> {
    event
        .get("content_block")
        .and_then(|b| b.get("name"))
        .and_then(|v| v.as_str())
        .map(String::from)
}
