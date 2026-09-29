//! `POST /api/daemon/deregister`（upstream `DaemonDeregister`，`daemon.go:939`）。
//! 模块文档见父模块（`mod.rs`）。

use axum::body::Bytes;
use axum::extract::State;
use axum::Json;
use mc_core::Id;
use mc_repos::daemon::DaemonRepo;
use serde_json::{json, Value};
use std::sync::Arc;

use super::super::dto::{decode_body, DeregisterRequest};
use super::super::scope::{internal, validation, DaemonAuth};
use crate::error::ApiResult;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// POST /api/daemon/deregister
// ---------------------------------------------------------------------------

/// upstream `DaemonDeregister`（`daemon.go:939`）：把给定 runtime 置为 `offline`。
///
/// 契约（`docs/16` §6.1）：`200 {"status":"ok"}`；400 `invalid request body`/
/// `runtime_ids is required`/`invalid runtime_ids`；500 `failed to load runtimes`。
///
/// **单条失败不影响整批**：上游对「行不在」「不属于本 workspace」「置离线写失败」都是
/// `slog.Warn` + `continue`，只有批量读出错才 500。daemon 停机时不该因为一台机器的行
/// 被删掉就让整次下线请求失败——那些 runtime 会由 liveness sweep 兜底。
pub(crate) async fn deregister(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let req: DeregisterRequest = decode_body(&body)?;
    if req.runtime_ids.is_empty() {
        return Err(validation("runtime_ids is required"));
    }
    let repo = DaemonRepo::new(&state.db);

    // 先整体校验 uuid（上游 `parseUUIDSliceOrBadRequest`：坏 id = 整批 400，不跳过）。
    let mut targets: Vec<(String, Id)> = Vec::new();
    for raw in &req.runtime_ids {
        let id = Id::parse(raw.trim()).map_err(|_| validation("invalid runtime_ids"))?;
        // 上游按 canonical uuid 去重后再批量查；本地保持「原始 id 单独查」以避免
        // 一次多余的全表比价，同时用 canonical id 去重（重复 id 不重复通知）。
        if !targets.iter().any(|(_, existing)| *existing == id) {
            targets.push((raw.clone(), id));
        }
    }

    let mut offline = 0usize;
    for (raw, runtime_id) in targets {
        // 上游批量读一次再逐条判；本地逐条读（N+1，偏离 D-6），但语义一致：
        // 读出错 = 500 `failed to load runtimes`，行不在 = warn + 跳过。
        let runtime = match repo.runtime_by_id(runtime_id).await {
            Ok(Some(runtime)) => runtime,
            Ok(None) => {
                tracing::warn!(runtime_id = %raw, "deregister: runtime not found");
                continue;
            }
            Err(_) => return Err(internal("failed to load runtimes")),
        };
        if !super::super::scope::workspace_allowed(&state, &auth, runtime.workspace_id).await? {
            tracing::warn!(runtime_id = %raw, "deregister: workspace mismatch");
            continue;
        }
        // 上游用**请求原文 id**（不是 canonical uuid）去 `offline_reasons` 里取值。
        let reason = req
            .offline_reasons
            .get(&raw)
            .filter(|value| !value.is_null());
        let updated = match reason {
            Some(reason) => {
                repo.set_runtime_offline_with_reason(runtime.workspace_id, runtime_id, reason)
                    .await
            }
            None => {
                repo.set_runtimes_offline(runtime.workspace_id, &[runtime_id])
                    .await
            }
        };
        match updated {
            Ok(rows) if rows.is_empty() => {
                tracing::warn!(runtime_id = %raw, "deregister: runtime not found");
            }
            Ok(rows) => {
                for id in rows {
                    // `notify_runtime_gone` 内部同时摘掉每条连接心跳 scope 里的这个 runtime。
                    state.daemon_hub.notify_runtime_gone(&id.to_string());
                    offline += 1;
                }
            }
            Err(err) => {
                tracing::warn!(runtime_id = %raw, error = %err, "deregister: failed to set offline");
            }
        }
    }

    tracing::info!(runtime_ids = ?req.runtime_ids, offline, "daemon deregistered");
    Ok(Json(json!({ "status": "ok" })))
}
