//! `/api/agents/:id/labels` 三条路由（上游 `label.go` L576-660）。
//!
//! | method | path | 上游 |
//! |---|---|---|
//! | GET | `/api/agents/:id/labels` | `ListLabelsForAgent` L576 |
//! | POST | `/api/agents/:id/labels` | `AttachLabelToAgent` L591 |
//! | DELETE | `/api/agents/:id/labels/:label_id` | `DetachLabelFromAgent` L620 |
//!
//! 与上游一致的三点契约：
//! - 鉴权：GET 只需能加载到该 agent；attach/detach 还需 `canManageAgent`。
//! - attach 的 label 必须 `resource_type = 'agent'` 且属于同一 workspace，否则
//!   404 `"agent label not found"`。
//! - 三个端点都返回**同一个** `{"labels": [...]}` 包（attach/detach 成功后回读
//!   全量列表）；`usage_count` 沿用上游 `labelToResponse` 的恒 0。
//!
//! 偏离：不广播 `label:updated`（M3-7）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;

use super::dto::{AttachLabelRequest, LabelDto, LabelsDto};
use super::{bad_request, parse_uuid, repo_err, AgentScope};
use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 读全量 label 列表并包装（attach/detach 的响应体）。
async fn labels_response(scope: &AgentScope, agent_id: mc_core::Id) -> ApiResult<Json<LabelsDto>> {
    let rows = scope
        .repo
        .list_labels(agent_id)
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    Ok(Json(LabelsDto {
        labels: rows.iter().map(LabelDto::from_row).collect(),
    }))
}

/// `GET /api/agents/:id/labels`（上游 `ListLabelsForAgent`）。
pub(super) async fn list_labels(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<LabelsDto>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    labels_response(&scope, agent.id()).await
}

/// `POST /api/agents/:id/labels`（上游 `AttachLabelToAgent`，幂等）。
pub(super) async fn attach_label(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<LabelsDto>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage(&agent)?;

    let req: AttachLabelRequest = serde_json::from_slice(&body).unwrap_or_default();
    let Some(raw_label_id) = req.label_id.as_deref().filter(|v| !v.is_empty()) else {
        return Err(bad_request("label_id is required").into());
    };
    let label_id = parse_uuid(raw_label_id, "label_id")?;
    let label_id = mc_core::Id(label_id);

    // `get_label` 查不到 或 不是 agent 资源 → 同一个 404（上游合并了两个分支）。
    let label = scope
        .repo
        .get_label(scope.workspace_id, label_id)
        .await
        .map_err(|_| super::not_found("agent label"))?;
    if label.resource_type != "agent" {
        return Err(super::not_found("agent label").into());
    }
    scope
        .repo
        .attach_label(agent.id(), label_id, scope.workspace_id)
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    labels_response(&scope, agent.id()).await
}

/// `DELETE /api/agents/:id/labels/:label_id`（上游 `DetachLabelFromAgent`，幂等）。
pub(super) async fn detach_label(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path((id, label_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<LabelsDto>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage(&agent)?;
    let label_id = parse_uuid(&label_id, "label id")?;
    let label_id = mc_core::Id(label_id);
    scope
        .repo
        .detach_label(agent.id(), label_id, scope.workspace_id)
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    labels_response(&scope, agent.id()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_body_without_label_id_is_rejected() {
        // handler 层的 400 分支：body 不是 JSON 或缺 label_id 都要走同一条路径。
        let req: AttachLabelRequest = serde_json::from_slice(b"{}").unwrap_or_default();
        assert!(req.label_id.is_none());
        let req: AttachLabelRequest = serde_json::from_slice(b"[1,2]").unwrap_or_default();
        assert!(req.label_id.is_none());
    }
}
