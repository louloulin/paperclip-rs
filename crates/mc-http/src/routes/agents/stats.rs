//! workspace 级聚合三条路由（上游 `agent.go` L2884 / L2925 / L2976）。
//!
//! | method | path | 上游 | 线上形状 |
//! |---|---|---|---|
//! | GET | `/api/agent-task-snapshot` | `ListWorkspaceAgentTaskSnapshot` L2976 | `[]AgentTaskResponse` |
//! | GET | `/api/agent-activity-30d` | `GetWorkspaceAgentActivity30d` L2925 | `[]AgentActivityBucket` |
//! | GET | `/api/agent-run-counts` | `GetWorkspaceAgentRunCounts` L2884 | `[]AgentRunCount` |
//!
//! 三个端点都是**顶层 JSON 数组**（不是 `{"items": …}`），且都按
//! `accessibleAgentIDs` 做白名单过滤：上游一次 `ListAllAgents` + 一次
//! `loadInvocationTargetsByAgent`，本片对应
//! [`AgentScope::filter_accessible`]（同一个 `list(ws, include_archived=true)`，
//! 因为 `ListAllAgents` 的 SQL 不过滤 `archived_at`）。
//!
//! 空结果序列化成 `[]` 而非 `null`（上游 `make([]T, 0, n)`）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use uuid::Uuid;

use super::dto::{ActivityBucketDto, AgentTaskDto, RunCountDto};
use super::{repo_err, AgentScope};
use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 一次取「本 workspace 全部 `kind='user'` agent + 白名单」——三个端点共用的前置。
async fn accessible_ids(scope: &AgentScope) -> ApiResult<HashSet<Uuid>> {
    let agents = scope
        .repo
        .list(scope.workspace_id, true)
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    let ids: Vec<Uuid> = agents.iter().map(|a| a.id).collect();
    let targets = scope.targets_by_agent(&ids).await?;
    Ok(scope
        .filter_accessible(agents, &targets)
        .into_iter()
        .map(|a| a.id)
        .collect())
}

/// `GET /api/agent-task-snapshot`（上游 `ListWorkspaceAgentTaskSnapshot`）。
pub(super) async fn task_snapshot(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<AgentTaskDto>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let allowed = accessible_ids(&scope).await?;
    let rows = scope
        .repo
        .task_snapshot(scope.workspace_id)
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    Ok(Json(
        rows.iter()
            .filter(|row| allowed.contains(&row.agent_id))
            .map(AgentTaskDto::from_row)
            .collect(),
    ))
}

/// `GET /api/agent-activity-30d`（上游 `GetWorkspaceAgentActivity30d`）。
pub(super) async fn activity_30d(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<ActivityBucketDto>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let allowed = accessible_ids(&scope).await?;
    let rows = scope
        .repo
        .activity_30d(scope.workspace_id)
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    Ok(Json(
        rows.iter()
            .filter(|row| allowed.contains(&row.agent_id))
            .map(ActivityBucketDto::from_row)
            .collect(),
    ))
}

/// `GET /api/agent-run-counts`（上游 `GetWorkspaceAgentRunCounts`）。
pub(super) async fn run_counts(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<RunCountDto>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let allowed = accessible_ids(&scope).await?;
    let rows = scope
        .repo
        .run_counts_30d(scope.workspace_id)
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    Ok(Json(
        rows.iter()
            .filter(|row| allowed.contains(&row.agent_id))
            .map(RunCountDto::from_row)
            .collect(),
    ))
}
