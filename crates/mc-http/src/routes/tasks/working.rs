//! `GET /api/working-agents`（上游 `agent.go:2766` `ListWorkspaceWorkingAgents`）。
//!
//! 参数组合比表面复杂，**顺序**是契约的一部分（上游逐个 `switch` 返回，先命中先回）：
//! `type` → `scope`/`relation` → `parent`。错误文案逐条沿用上游。
//!
//! 「私有 agent 不得凭名字 / 头像 / 计数暴露存在性」这条隐私约束在 SQL 里做不到
//! （可见性判定在 M3-5 的 [`crate::routes::agents::AgentScope`]），因此本 handler
//! 先取聚合行、再做访问过滤 —— 与上游 `accessibleAgentIDs` 后置过滤的顺序一致。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use mc_core::Id;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;

use super::dto::WorkingAgentDto;
use super::{bad_request, non_empty_query, parse_uuid, repo_err, TaskScope};

/// `type` 的白名单（空 = 不过滤）。
const WORK_TYPES: [&str; 3] = ["issue", "autopilot", "chat"];
/// `relation` 的白名单（`scope=mine` 下有效）。
const MINE_RELATIONS: [&str; 4] = ["assigned", "created", "involved", "any"];

/// `GET /api/working-agents`（上游 `ListWorkspaceWorkingAgents`）。
pub(crate) async fn working_agents(
    State(state): State<Arc<crate::state::AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<WorkingAgentDto>>> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;

    let work_type = non_empty_query(&query, "type").unwrap_or_default();
    if !work_type.is_empty() && !WORK_TYPES.contains(&work_type.as_str()) {
        return Err(bad_request(
            "invalid type: must be issue, autopilot, or chat",
        ));
    }

    let mine_scope = non_empty_query(&query, "scope");
    let mut mine_relation = non_empty_query(&query, "relation").unwrap_or_default();
    let mut member_id = None;
    match mine_scope.as_deref() {
        None => {
            if !mine_relation.is_empty() {
                return Err(bad_request("relation requires scope=mine"));
            }
            mine_relation.clear();
        }
        Some("mine") => {
            if work_type != "issue" {
                return Err(bad_request("scope=mine requires type=issue"));
            }
            if mine_relation.is_empty() {
                "any".clone_into(&mut mine_relation);
            }
            if !MINE_RELATIONS.contains(&mine_relation.as_str()) {
                return Err(bad_request(
                    "invalid relation: must be assigned, created, involved, or any",
                ));
            }
            member_id = Some(scope.user_id());
        }
        Some(_) => return Err(bad_request("invalid scope: must be mine")),
    }

    let parent_issue_id = match non_empty_query(&query, "parent") {
        None => None,
        Some(raw) => {
            if work_type != "issue" {
                return Err(bad_request("parent requires type=issue"));
            }
            if mine_scope.is_some() {
                return Err(bad_request("parent cannot be combined with scope"));
            }
            Some(Id::from(parse_uuid(&raw, "parent")?))
        }
    };

    let rows = scope
        .repo
        .list_working_agents(
            scope.workspace_id(),
            &mc_repos::task::WorkingAgentFilter {
                work_type,
                mine_relation,
                member_id,
                parent_issue_id,
            },
        )
        .await
        .map_err(|e| repo_err(e, "working agent"))?;

    // 访问过滤（上游 `accessibleAgentIDs`）：不可见的 agent 直接整行丢弃。
    let allowed = scope.accessible_agents().await?;
    Ok(Json(
        rows.into_iter()
            .filter(|row| allowed.contains_key(&row.id))
            .map(|row| WorkingAgentDto {
                id: row.id.to_string(),
                name: row.name,
                avatar_url: row.avatar_url,
                running_task_count: row.running_task_count,
                issue_ids: row.issue_ids.iter().map(ToString::to_string).collect(),
            })
            .collect(),
    ))
}
