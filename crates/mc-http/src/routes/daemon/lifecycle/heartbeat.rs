//! `POST /api/daemon/heartbeat`（upstream `DaemonHeartbeat`，`daemon.go:1089`）与心跳 ack
//! 的**唯一**生产者。模块文档见父模块（`mod.rs`）。

use axum::body::Bytes;
use axum::extract::State;
use axum::Json;
use mc_core::Id;
use mc_daemon_proto::capabilities::SERVER_HEARTBEAT_CAPABILITIES;
use mc_daemon_proto::messages::daemon::{
    DaemonHeartbeatAckPayload, DaemonHeartbeatPendingLocalSkillImport,
    DaemonHeartbeatPendingLocalSkills, DaemonHeartbeatPendingModelList,
    DaemonHeartbeatPendingUpdate, HEARTBEAT_STATUS_RUNTIME_GONE,
};
use mc_repos::daemon::DaemonRepo;
use serde_json::{json, Value};
use std::sync::Arc;

use super::super::dto::{decode_body, HeartbeatRequest};
use super::super::scope::{internal, not_found, validation, DaemonAuth};
use crate::daemon_requests::RequestKind;
use crate::error::ApiResult;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// POST /api/daemon/heartbeat
// ---------------------------------------------------------------------------

/// upstream `DaemonHeartbeat`（`daemon.go:1089`）。
///
/// **HTTP ack 与 WS ack 形状不同**（`docs/16` §6.4）：HTTP 版不带 `runtime_id`
/// 与 `server_capabilities`，因为调用方已经知道自己问的是哪台。
pub(crate) async fn heartbeat(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let req: HeartbeatRequest = decode_body(&body)?;
    if req.runtime_id.trim().is_empty() {
        return Err(validation("runtime_id is required"));
    }
    let runtime = super::super::scope::require_runtime_access(
        &state,
        &auth,
        req.runtime_id.trim(),
        "runtime not found",
    )
    .await?;

    let repo = DaemonRepo::new(&state.db);
    let touched = repo
        .touch_runtime_heartbeat(runtime.id)
        .await
        .map_err(|_| internal("heartbeat failed"))?;
    if touched.is_none() {
        return Err(not_found("runtime not found"));
    }

    let ack = heartbeat_ack(&state, runtime.id, req.supports_batch_import);
    Ok(Json(http_ack_value(&ack)))
}

/// 心跳 ack 的**唯一**生产者：HTTP 与 WS 两条腿共用，避免字段集在两处漂移
/// （upstream `processHeartbeat`，`daemon.go:1371`）。
///
/// 取走四类待处理请求并写进 ack（`omitempty` 语义：字段缺席 = 没有待办）。本地 store
/// 是单节点内存实现，没有上游 `HasPending` 探测 / `PopPending` 取走的两段式 —— 也就
/// 没有"探测超时"那一支（偏离见 `docs/32`）。
pub(crate) fn heartbeat_ack(
    state: &AppState,
    runtime_id: Id,
    supports_batch_import: bool,
) -> DaemonHeartbeatAckPayload {
    let store = &state.daemon_requests;
    let mut ack = DaemonHeartbeatAckPayload {
        runtime_id: runtime_id.to_string(),
        status: "ok".into(),
        server_capabilities: SERVER_HEARTBEAT_CAPABILITIES
            .iter()
            .map(|c| (*c).to_string())
            .collect(),
        ..DaemonHeartbeatAckPayload::default()
    };

    if let Some(pending) = store.pop_pending(RequestKind::Update, runtime_id) {
        // `target_version` **无条件**序列化（`messages.go:423`），结果上报的 body 里也
        // 不回传它 ⇒ 只有 store 记得住它，必须在这里带出去。
        ack.pending_update = Some(DaemonHeartbeatPendingUpdate {
            id: pending.id.to_string(),
            target_version: pending.target_version.clone().unwrap_or_default(),
        });
    }
    if let Some(pending) = store.pop_pending(RequestKind::ModelList, runtime_id) {
        ack.pending_model_list = Some(DaemonHeartbeatPendingModelList {
            id: pending.id.to_string(),
        });
    }
    if let Some(pending) = store.pop_pending(RequestKind::LocalSkills, runtime_id) {
        ack.pending_local_skills = Some(DaemonHeartbeatPendingLocalSkills {
            id: pending.id.to_string(),
        });
    }
    if supports_batch_import {
        let batch =
            store.pop_pending_batch(RequestKind::LocalSkillImport, runtime_id, MAX_IMPORT_BATCH);
        // 单数键 = 第一条（老 daemon 不认复数键，必须仍然拿到一条）；复数键 = 全部。
        // 部分失败也要把已认领的条目发出去：它们已经在 store 里转成 `running` 了，
        // 扣住不发只会让请求悬挂到超时。
        ack.pending_local_skill_import = batch.first().map(skill_import_pending);
        ack.pending_local_skill_imports = batch.iter().map(skill_import_pending).collect();
    } else if let Some(pending) = store.pop_pending(RequestKind::LocalSkillImport, runtime_id) {
        ack.pending_local_skill_import = Some(skill_import_pending(&pending));
    }
    ack
}

fn skill_import_pending(
    pending: &crate::daemon_requests::PendingRequest,
) -> DaemonHeartbeatPendingLocalSkillImport {
    DaemonHeartbeatPendingLocalSkillImport {
        id: pending.id.to_string(),
        skill_key: pending.skill_key.clone().unwrap_or_default(),
    }
}

/// upstram `runtimeGoneHeartbeatAck`（`daemon.go:1240`）：runtime 行已消失。
///
/// 带 `runtime_gone: true`，且**不带** `server_capabilities`（连协议协商都免了）。
pub(crate) fn runtime_gone_ack(runtime_id: &str) -> DaemonHeartbeatAckPayload {
    DaemonHeartbeatAckPayload {
        runtime_id: runtime_id.to_string(),
        status: HEARTBEAT_STATUS_RUNTIME_GONE.into(),
        runtime_gone: true,
        ..DaemonHeartbeatAckPayload::default()
    }
}

/// WS ack → HTTP ack 的投影（upstream `DaemonHeartbeat` 结尾，`daemon.go:1185`）：
///
/// - 去掉 `runtime_id`：调用方已经知道自己问的是哪台，带上只是噪声；
/// - 去掉 `server_capabilities`：上游 HTTP 腿从不发它（协议协商只走 WS）；
/// - 其余 `pending_*` 靠 `skip_serializing_if` 自然缺席。
fn http_ack_value(ack: &DaemonHeartbeatAckPayload) -> Value {
    let Ok(mut value) = serde_json::to_value(ack) else {
        return json!({ "status": "ok" });
    };
    if let Value::Object(obj) = &mut value {
        obj.remove("runtime_id");
        obj.remove("server_capabilities");
    }
    value
}

/// upstream `maxLocalSkillImportBatch = 10`。
const MAX_IMPORT_BATCH: usize = 10;
