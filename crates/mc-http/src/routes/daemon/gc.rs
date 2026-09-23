//! GC 探针面（M3-7 / LUM-1438）—— daemon 回收宿主产物前问的「这条记录还活着吗」。
//!
//! 覆盖 5 条路由（`docs/16` §6.1 表 A 第 30–34 行）：
//!
//! | 路由 | upstream | 门 |
//! |---|---|---|
//! | `POST /api/daemon/workspaces/:workspaceId/issues/gc-check` | `BatchIssueGCCheck` `daemon.go:5874` | `G_ws` |
//! | `GET /api/daemon/issues/:issueId/gc-check` | `GetIssueGCCheck` `daemon.go:5954` | `G_ws`（按 issue 行解析） |
//! | `GET /api/daemon/chat-sessions/:sessionId/gc-check` | `GetChatSessionGCCheck` `daemon.go:5977` | `G_ws`（按 session 行解析） |
//! | `GET /api/daemon/autopilot-runs/:runId/gc-check` | `GetAutopilotRunGCCheck` `daemon.go:5998` | `G_ws`（按父 autopilot 行解析） |
//! | `GET /api/daemon/tasks/:taskId/gc-check` | `GetTaskGCCheck` `daemon.go:6028` | `G_task` |
//!
//! ## 三条不可动摇的性质
//!
//! 1. **反枚举**：单条探针一律「先载行 → 再按行里的 workspace 过门」，workspace 不匹配
//!    与行不存在返回**同一个 404** —— 否则一枚局限在 A 空间的 daemon token 就能靠
//!    404/403 的差别扫出 B 空间有哪些 issue / session / run 存在。
//! 2. **服务端归一化状态**：daemon 只消费一个事实（「是否终态，能否回收 workdir」），
//!    因此 response 里的 `category` 是 upstream 的四值生命周期词汇
//!    （`unstarted` / `started` / `done` / `closed`，由 [`mc_repos::daemon::issue_category`]
//!    从内置状态投影）；`status` 回传内置状态键本身。无类别的状态（本仓不存在，
//!    留给未来自定义状态）原样回 `status` 并**省略** `category` —— daemon 据此
//!    fail-closed，只回收产物。
//! 3. **批量在 SQL 层按 workspace 过滤**：不属于本空间的 id 与不存在的 id 一样是
//!    `found: false`，不给「这个 id 存在但属于别人」的信号。
//!
//! 批量探针是**已安装 daemon 定时跑**的端点（每个在跑任务的 workdir 都要过一遍），
//! 因此有 500 条 / 64 KiB 双重上限，[`MAX_ISSUE_GC_BATCH_SIZE`] 与
//! [`MAX_ISSUE_GC_BODY_BYTES`] 与上游 `maxIssueGCBatchSize` /
//! `maxIssueGCBatchBodyBytes` 同值。
//!
//! ## 已知偏离（`docs/32` 偏离表）
//!
//! 上游用 `http.MaxBytesReader` 让超限 body 在**解码阶段**失败，本地是先读 `Bytes`
//! 再比长度：两者都回 400 `invalid request body`，且都优先于 `too many issue_ids`
//! 判定，可观察行为一致。
//!
//! `completed_at` / `updated_at` 落 `Option<String>`（`None` → `null`）；上游把它们
//! 当非空 `time.Time` 序列化，未完成的 run 会输出零值时间 `0001-01-01T00:00:00Z`。
//! 本地回 `null` 更诚实，且这两个字段对 daemon 只是诊断信息（决策只看 `status`）。

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use mc_core::Id;
use mc_repos::daemon::DaemonRepo;

use super::dto::{
    decode_body, BatchIssueGcCheckRequest, MAX_ISSUE_GC_BATCH_SIZE, MAX_ISSUE_GC_BODY_BYTES,
};
use super::scope::{
    db_err, not_found, parse_path_id, require_task_access, require_workspace_access, timestamp,
    timestamp_opt, DaemonAuth,
};
use crate::error::ApiResult;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 1. 批量 issue 探针
// ---------------------------------------------------------------------------

/// upstream `batchIssueGCCheckItem`：`found: false` 时后三个字段**全部省略**。
#[derive(Debug, Clone, Serialize)]
struct BatchIssueGcItem {
    /// 请求里**原样**的 id 字符串（不是规范化后的 uuid）。
    id: String,
    /// 在本 workspace 内是否存在。
    found: bool,
    /// 内置状态键（仅 `found`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    /// 四值生命周期类别（仅 `found` 且有类别）。
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    /// `updated_at`（仅 `found`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

/// `POST /api/daemon/workspaces/:workspaceId/issues/gc-check`。
pub(crate) async fn batch_issue_gc_check(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(workspace_id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let workspace = parse_path_id("workspace_id", &workspace_id)?;
    require_workspace_access(&state, &auth, workspace, "workspace not found").await?;

    // 上游 `MaxBytesReader` 让超限 body 在解码阶段失败 ⇒ 也是 `invalid request body`，
    // 且**优先于** id 数量上限（先读后判）。
    if body.len() > MAX_ISSUE_GC_BODY_BYTES {
        return Err(super::scope::validation("invalid request body"));
    }
    let request: BatchIssueGcCheckRequest = decode_body(&body)?;
    if request.issue_ids.len() > MAX_ISSUE_GC_BATCH_SIZE {
        return Err(super::scope::validation("too many issue_ids"));
    }

    // 任何一个坏 id 都整批 400（不做「跳过坏的」—— daemon 侧拿到 400 会自己修）。
    let mut ids = Vec::with_capacity(request.issue_ids.len());
    for raw in &request.issue_ids {
        let id = Id::parse(raw.trim()).map_err(|_| super::scope::validation("invalid issue_id"))?;
        ids.push(id);
    }

    let rows = DaemonRepo::new(&state.db)
        .list_issue_gc(workspace, &ids)
        .await
        .map_err(|e| {
            tracing::warn!(
                workspace_id = %workspace_id,
                count = ids.len(),
                error = %e,
                "list issue GC statuses failed"
            );
            super::scope::internal("failed to check issues")
        })?;
    let by_id: HashMap<Id, _> = rows.into_iter().map(|row| (row.id, row)).collect();

    let items: Vec<BatchIssueGcItem> = request
        .issue_ids
        .iter()
        .zip(ids)
        .map(|(raw, id)| match by_id.get(&id) {
            Some(row) => BatchIssueGcItem {
                id: raw.clone(),
                found: true,
                status: Some(row.status.clone()),
                // 空串 = 无类别 ⇒ 省略该键（daemon fail-closed）。
                category: (!row.category.is_empty()).then(|| row.category.clone()),
                updated_at: timestamp_opt(row.updated_at),
            },
            None => BatchIssueGcItem {
                id: raw.clone(),
                found: false,
                status: None,
                category: None,
                updated_at: None,
            },
        })
        .collect();
    Ok(Json(json!({ "issues": items })))
}

// ---------------------------------------------------------------------------
// 2. 单 issue 探针
// ---------------------------------------------------------------------------

/// `GET /api/daemon/issues/:issueId/gc-check`。
///
/// 与批量版不同，这里的 `category` **无条件出现**（上游写的是 `map[string]any`，
/// 空串不会被省略）：daemon 靠 `category == ""` 识别「无类别的自定义状态」。
pub(crate) async fn get_issue_gc_check(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(issue_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_path_id("issue_id", &issue_id)?;
    let Some(row) = DaemonRepo::new(&state.db)
        .issue_gc_probe(id)
        .await
        .map_err(db_err)?
    else {
        return Err(not_found("issue not found"));
    };
    require_workspace_access(&state, &auth, row.workspace_id, "issue not found").await?;
    Ok(Json(json!({
        "status": row.status,
        "category": mc_repos::daemon::issue_category(&row.status),
        "updated_at": timestamp(row.updated_at),
    })))
}

// ---------------------------------------------------------------------------
// 3. chat session 探针
// ---------------------------------------------------------------------------

/// `GET /api/daemon/chat-sessions/:sessionId/gc-check`。
///
/// 404 在这里是**语义信号**而不是错误：用户显式删除 session 时是硬删
/// （`DeleteChatSession` 走真 DELETE），daemon 据此立即回收对应 workdir ——
/// 这是能拿到的最强回收授权。
pub(crate) async fn get_chat_session_gc_check(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(session_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_path_id("session_id", &session_id)?;
    let Some(row) = DaemonRepo::new(&state.db)
        .chat_session_gc(id)
        .await
        .map_err(db_err)?
    else {
        return Err(not_found("chat session not found"));
    };
    require_workspace_access(&state, &auth, row.workspace_id, "chat session not found").await?;
    Ok(Json(json!({
        "status": row.status,
        "updated_at": timestamp_opt(row.updated_at),
    })))
}

// ---------------------------------------------------------------------------
// 4. autopilot run 探针
// ---------------------------------------------------------------------------

/// `GET /api/daemon/autopilot-runs/:runId/gc-check`。
///
/// 归属从**父 autopilot 行**解析（run 自己不存 workspace）。父行不在 = 404 而不是 500：
/// daemon 会因此落到「按 mtime 判孤儿」的兜底路径，比让整轮 GC 卡在一个 5xx 上更好。
pub(crate) async fn get_autopilot_run_gc_check(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(run_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_path_id("run_id", &run_id)?;
    let Some(row) = DaemonRepo::new(&state.db)
        .autopilot_run_gc(id)
        .await
        .map_err(db_err)?
    else {
        return Err(not_found("autopilot run not found"));
    };
    // 父 autopilot 已消失 ⇒ 与「run 不存在」同一个 404（也是反枚举的一部分）。
    let Some(workspace_id) = row.workspace_id else {
        return Err(not_found("autopilot run not found"));
    };
    require_workspace_access(&state, &auth, workspace_id, "autopilot run not found").await?;
    Ok(Json(json!({
        "status": row.status,
        "completed_at": timestamp_opt(row.completed_at),
    })))
}

// ---------------------------------------------------------------------------
// 5. task 探针
// ---------------------------------------------------------------------------

/// `GET /api/daemon/tasks/:taskId/gc-check`。
///
/// quick-create 任务没有父记录（`WriteGCMeta` 时没有 issue、没有 chat session、
/// 没有 autopilot run），daemon 只能直接拿 task 行本身当 GC 的依据。
pub(crate) async fn get_task_gc_check(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let (task, _workspace) = require_task_access(&state, &auth, &task_id, "task not found").await?;
    Ok(Json(json!({
        "status": task.status,
        "completed_at": timestamp_opt(task.completed_at),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_item_omits_absent_fields() {
        let item = serde_json::to_value(BatchIssueGcItem {
            id: "abc".into(),
            found: false,
            status: None,
            category: None,
            updated_at: None,
        })
        .unwrap();
        assert_eq!(item, json!({ "id": "abc", "found": false }));
    }

    #[test]
    fn batch_item_omits_empty_category() {
        let item = serde_json::to_value(BatchIssueGcItem {
            id: "abc".into(),
            found: true,
            status: Some("weird".into()),
            category: None,
            updated_at: Some("2026-01-01T00:00:00Z".into()),
        })
        .unwrap();
        assert_eq!(
            item,
            json!({
                "id": "abc",
                "found": true,
                "status": "weird",
                "updated_at": "2026-01-01T00:00:00Z",
            })
        );
    }

    #[test]
    fn category_is_emitted_for_builtin_statuses() {
        assert_eq!(mc_repos::daemon::issue_category("done"), "done");
        assert_eq!(mc_repos::daemon::issue_category("cancelled"), "closed");
    }

    #[test]
    fn batch_caps_match_upstream() {
        assert_eq!(MAX_ISSUE_GC_BATCH_SIZE, 500);
        assert_eq!(MAX_ISSUE_GC_BODY_BYTES, 65536);
    }
}
