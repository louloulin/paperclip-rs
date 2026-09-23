//! daemon 面的任务**消息**路由（R7 拆分自 `tasks.rs`）。
//!
//! 上游落点：`daemon.go`（`ListTaskMessages` / `ReportTaskMessages`）与
//! `task_messages.go` 的时钟偏离规则。
//!
//! 两条路由是 daemon 与前端之间的「执行过程留痕」：`GET` 是**裸数组**（重连后的追赶读，
//! 没有分页信封），`POST` 是批量上报。上报回来的 `created_at` 只在**整批可信**时保留 ——
//! daemon 的机器时钟可以偏，偏得多的那批整批退回数据库时间，避免留痕顺序被一台没对表的
//! 机器打乱（upstream `maxTaskMessageClockSkew = 2 * time.Minute`）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use mc_repos::daemon::{DaemonRepo, NewTaskMessage};

use super::dto::{decode_body, opt, TaskMessageBatchRequest};
use super::scope::{internal, require_task_access, validation, DaemonAuth};
use crate::error::ApiResult;
use crate::state::AppState;

/// upstream `maxTaskMessageClockSkew = 2 * time.Minute`。
const MAX_TASK_MESSAGE_CLOCK_SKEW_SECS: i64 = 120;

// ---------------------------------------------------------------------------
// messages
// ---------------------------------------------------------------------------

/// `GET /api/daemon/tasks/:taskId/messages` —— 响应是**裸数组**（重连后的追赶读）。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct SinceQuery {
    /// `?since=<seq>`：只取 `seq >` 该值的消息。
    #[serde(default)]
    since: Option<String>,
}

pub(crate) async fn list_messages(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
    Query(query): Query<SinceQuery>,
) -> ApiResult<Json<Value>> {
    let repo = DaemonRepo::new(&state.db);
    let (task, _workspace) = require_task_access(&state, &auth, &task_id, "task not found").await?;

    let since_seq = match query.since.as_deref() {
        None | Some("") => None,
        Some(raw) => Some(
            raw.trim()
                .parse::<i32>()
                .map_err(|_| validation("invalid since parameter"))?,
        ),
    };

    let rows = repo
        .list_task_messages(task.id(), since_seq)
        .await
        .map_err(|e| internal(format!("failed to list task messages: {e}")))?;
    let issue_id = task.issue_id().map(|id| id.to_string()).unwrap_or_default();
    let payloads: Vec<Value> = rows
        .iter()
        .map(|m| task_message_payload(m, &task.id().to_string(), &issue_id))
        .collect();
    Ok(Json(Value::Array(payloads)))
}

/// upstream `taskMessageToPayload`。
fn task_message_payload(
    m: &mc_repos::task::TaskMessageRow,
    task_id: &str,
    issue_id: &str,
) -> Value {
    // `input` 列是可空 JSONB；`input` 为 NULL 时 payload 里就是 `null`（不是 `{}`）。
    let input = m.input.clone().unwrap_or(Value::Null);
    let mut out = json!({
        "task_id": task_id,
        "issue_id": issue_id,
        "seq": m.seq,
        "type": m.r#type,
        "tool": m.tool.clone().unwrap_or_default(),
        "call_id": m.call_id.clone().unwrap_or_default(),
        "content": m.content.clone().unwrap_or_default(),
        "input": input,
        "output": m.output.clone().unwrap_or_default(),
        "created_at": m.created_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
    });
    // 三态：没测量过就是 `null`，**永远不**塌成 `false`。
    if let Some(obj) = out.as_object_mut() {
        obj.insert(
            "output_truncated".into(),
            m.output_truncated.map_or(Value::Null, Value::Bool),
        );
    }
    out
}

/// `POST /api/daemon/tasks/:taskId/messages`（实时 agent 输出）。
///
/// 上游先解码再鉴权（空批直接 200、**不做**鉴权查询）—— 这是最热的写路径，
/// 每 500ms 每个在飞任务打一次；空批短路省掉的正是这次查询。
pub(crate) async fn report_messages(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    // 先解码后鉴权：坏 body / 空批对不存在的任务也走同一条短路。
    let req: TaskMessageBatchRequest = decode_body(&body)?;
    if req.messages.is_empty() {
        return Ok(Json(json!({ "status": "ok" })));
    }
    let repo = DaemonRepo::new(&state.db);
    let (task, _workspace) = require_task_access(&state, &auth, &task_id, "task not found").await?;

    let created_ats = batch_created_ats(&req.messages, Utc::now());
    let mut rows = Vec::with_capacity(req.messages.len());
    for (i, m) in req.messages.iter().enumerate() {
        rows.push(NewTaskMessage {
            task_id: task.id(),
            seq: i32::try_from(m.seq).unwrap_or(i32::MAX),
            kind: m.kind.clone(),
            tool: opt(m.tool.clone()),
            content: opt(m.content.clone()),
            // `input` 缺省是 `null`（Go 的 `map[string]any` 零值），不是 `{}`。
            input: (!m.input.is_null()).then(|| m.input.clone()),
            output: opt(m.output.clone()),
            output_truncated: m.output_truncated,
            call_id: opt(m.call_id.clone()),
            created_at: created_ats[i],
        });
    }
    repo.insert_task_messages(&rows)
        .await
        .map_err(|e| internal(format!("failed to insert task messages: {e}")))?;
    Ok(Json(json!({ "status": "ok" })))
}

/// upstream `taskMessageCreatedAt` + `taskMessageCreatedAts`：整批同一个时钟。
///
/// 只要有一条缺时间戳（老 daemon）或与服务器时钟偏离超过 2 分钟，**整批**都退回
/// 数据库时间。成对事件（`tool_call` / `tool_result`）跨分钟排序会让 UI 把它们
/// 显示反，这比丢掉一个可疑的客户端时间戳更糟。
fn batch_created_ats(
    messages: &[super::dto::TaskMessageRequest],
    now: DateTime<Utc>,
) -> Vec<Option<DateTime<Utc>>> {
    let skew = chrono::Duration::seconds(MAX_TASK_MESSAGE_CLOCK_SKEW_SECS);
    let mut out: Vec<Option<DateTime<Utc>>> = messages
        .iter()
        .map(|m| match m.created_at {
            None => None,
            Some(t) => {
                let delta = t - now;
                ((-skew <= delta) && (delta <= skew)).then_some(t)
            }
        })
        .collect();
    if out.iter().any(Option::is_none) {
        out.fill(None);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    #[test]
    fn batch_timestamps_are_all_or_nothing() {
        let now = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let msg = |secs: Option<i64>| super::super::dto::TaskMessageRequest {
            created_at: secs.map(|s| now + chrono::Duration::seconds(s)),
            ..Default::default()
        };

        // 全部可信 ⇒ 逐个保留。
        let ok = batch_created_ats(&[msg(Some(0)), msg(Some(-30))], now);
        assert!(ok.iter().all(Option::is_some));

        // 任一条缺时间戳 ⇒ 整批退回数据库时间。
        let mixed = batch_created_ats(&[msg(Some(0)), msg(None)], now);
        assert!(mixed.iter().all(Option::is_none));

        // 超过 2 分钟的时钟偏离 ⇒ 同样整批丢弃。
        let skewed = batch_created_ats(&[msg(Some(121))], now);
        assert_eq!(skewed, vec![None]);
        let edge = batch_created_ats(&[msg(Some(120))], now);
        assert!(edge[0].is_some());
    }
}
