//! `/api/squads/:id/members*`（上游 `squad.go` L545–L975）。
//!
//! | method | path | 上游 |
//! |---|---|---|
//! | GET | `/api/squads/:id/members` | `ListSquadMembers` L545 |
//! | GET | `/api/squads/:id/members/status` | `ListSquadMemberStatus` L654 |
//! | POST | `/api/squads/:id/members` | `AddSquadMember` L767（201） |
//! | DELETE | `/api/squads/:id/members` | `RemoveSquadMember` L856（204） |
//! | PATCH | `/api/squads/:id/members/role` | `UpdateSquadMemberRole` L912 |
//!
//! 成员状态（`working` / `idle` / `unstable` / `offline` / `archived`）的推导在
//! `mc_squad::status`，本文件只负责把 SQL 行按上游规则折叠成响应。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::{DateTime, Utc};
use mc_errors::Error;
use mc_repos::squad::{is_conflict, is_valid_member_type, SquadMemberStatusRow, MEMBER_TYPE_AGENT};
use mc_squad::status::{derive_member_status, is_working_task_status};
use uuid::Uuid;

use super::dto::{
    decode_body_or_bad_request, AddSquadMemberRequest, RemoveSquadMemberRequest,
    SquadActiveIssueDto, SquadMemberDto, SquadMemberStatusDto, SquadMemberStatusListDto,
    UpdateSquadMemberRoleRequest,
};
use super::{bad_request, parse_uuid, repo_err, SquadScope};
use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// GET /api/squads/:id/members —— 上游 `ListSquadMembers`。
pub(super) async fn list_members(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<SquadMemberDto>>> {
    let scope = SquadScope::resolve(&state, auth, &headers, &query).await?;
    let squad = scope.squad(&id).await?;
    let rows = scope
        .repo
        .list_members(squad.id())
        .await
        .map_err(|e| repo_err(e, "squad member"))?;
    Ok(Json(rows.iter().map(SquadMemberDto::from_row).collect()))
}

/// GET /api/squads/:id/members/status —— 上游 `ListSquadMemberStatus`。
///
/// 只读聚合：`squad_member` LEFT JOIN `agent`/`agent_runtime`/`agent_task_queue`/`issue`。
/// 每个成员只出现一次（多任务行折叠），顺序沿用 SQL 的 `sm.created_at, dispatched_at
/// DESC NULLS LAST`。
#[allow(clippy::too_many_lines)] // 线性折叠 + 5 个字段的推导，拆函数只会割裂与上游的对照
pub(super) async fn list_member_status(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> ApiResult<Json<SquadMemberStatusListDto>> {
    let scope = SquadScope::resolve(&state, auth, &headers, &query).await?;
    let squad = scope.squad(&id).await?;
    let rows = scope
        .repo
        .member_status_rows(squad.id())
        .await
        .map_err(|e| repo_err(e, "squad member"))?;
    let prefix = scope
        .repo
        .workspace_issue_prefix(scope.workspace_id)
        .await
        .map_err(|e| repo_err(e, "workspace"))?;
    let now = Utc::now();

    let mut order: Vec<Uuid> = Vec::with_capacity(rows.len());
    let mut acc: HashMap<Uuid, MemberAcc> = HashMap::with_capacity(rows.len());
    for row in &rows {
        let entry = acc.entry(row.member_id).or_insert_with(|| {
            order.push(row.member_id);
            MemberAcc::from_row(row)
        });
        entry.fold(row, &prefix);
    }

    let mut members: Vec<SquadMemberStatusDto> = Vec::with_capacity(order.len());
    for member_id in order {
        let Some(entry) = acc.remove(&member_id) else {
            continue;
        };
        members.push(entry.finish(now));
    }
    Ok(Json(SquadMemberStatusListDto { members }))
}

/// 一个成员的状态累加器（上游 `memberAcc`）。
struct MemberAcc {
    member_type: String,
    member_id: Uuid,
    archived: bool,
    runtime_status: Option<String>,
    runtime_seen_at: Option<DateTime<Utc>>,
    has_working_task: bool,
    active_issues: Vec<SquadActiveIssueDto>,
    latest_active_at: Option<DateTime<Utc>>,
}

impl MemberAcc {
    /// 上游：`archived` / `runtime_status` / `runtime_seen_at` **只取第一行**的值
    /// （后续行不覆盖），`active_issues` 与 `latest_active_at` 逐行累加。
    fn from_row(row: &SquadMemberStatusRow) -> Self {
        Self {
            member_type: row.member_type.clone(),
            member_id: row.member_id,
            archived: row.agent_archived_at.is_some(),
            runtime_status: row.runtime_status.clone(),
            runtime_seen_at: row.runtime_last_seen_at,
            has_working_task: false,
            active_issues: Vec::new(),
            latest_active_at: None,
        }
    }

    fn fold(&mut self, row: &SquadMemberStatusRow, prefix: &str) {
        if self.member_type != MEMBER_TYPE_AGENT {
            return;
        }
        if row.task_id.is_none() {
            return;
        }
        if row
            .task_status
            .as_deref()
            .is_some_and(is_working_task_status)
        {
            self.has_working_task = true;
        }
        if let Some(issue_id) = row.task_issue_id {
            self.active_issues.push(SquadActiveIssueDto {
                issue_id: issue_id.to_string(),
                identifier: format!("{prefix}-{}", row.issue_number.unwrap_or(0)),
                title: row.issue_title.clone().unwrap_or_default(),
                issue_status: row.issue_status.clone().unwrap_or_default(),
            });
        }
        if let Some(dispatched) = row.task_dispatched_at {
            let newer = match self.latest_active_at {
                Some(current) => dispatched > current,
                None => true,
            };
            if newer {
                self.latest_active_at = Some(dispatched);
            }
        }
    }

    /// 上游收尾：非 agent 成员的 `status` / `last_active_at` 恒 `null`。
    fn finish(self, now: DateTime<Utc>) -> SquadMemberStatusDto {
        let is_agent = self.member_type == MEMBER_TYPE_AGENT;
        let status = is_agent.then(|| {
            derive_member_status(
                self.archived,
                self.runtime_status.as_deref(),
                self.runtime_seen_at,
                self.has_working_task,
                now,
            )
        });
        let last_active_at = is_agent
            .then(|| self.latest_active_at.or(self.runtime_seen_at))
            .flatten()
            .map(|t| t.to_rfc3339());
        SquadMemberStatusDto {
            member_type: self.member_type,
            member_id: self.member_id.to_string(),
            status,
            active_issues: self.active_issues,
            last_active_at,
        }
    }
}

/// POST /api/squads/:id/members —— 上游 `AddSquadMember`（201）。
pub(super) async fn add_member(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<SquadMemberDto>)> {
    let scope = SquadScope::resolve(&state, auth, &headers, &query).await?;
    let squad = scope.squad(&id).await?;
    scope.require_can_manage(&squad)?;
    let req: AddSquadMemberRequest = decode_body_or_bad_request(&body)?;

    let member_type = req.member_type.unwrap_or_default();
    let member_raw = req.member_id.unwrap_or_default();
    let role = req.role.unwrap_or_default();
    if !is_valid_member_type(&member_type) {
        return Err(bad_request("member_type must be 'agent' or 'member'").into());
    }
    if member_raw.is_empty() {
        return Err(bad_request("member_id is required").into());
    }
    let member_uuid = parse_uuid(&member_raw, "member_id")?;

    if member_type == MEMBER_TYPE_AGENT {
        let Some(agent) = scope.agent_in_workspace(&member_raw).await? else {
            return Err(bad_request("agent not found in this workspace").into());
        };
        if !scope.member_can_wire_agent(&agent).await? {
            return Err(Error::Forbidden {
                message: "you can only add an agent you have access to".into(),
            }
            .into());
        }
    } else if !scope
        .repo
        .workspace_member_exists(scope.workspace_id, mc_core::Id(member_uuid))
        .await
        .map_err(|e| repo_err(e, "member"))?
    {
        return Err(bad_request("member not found in this workspace").into());
    }

    let row = scope
        .repo
        .add_member(
            squad.id(),
            &member_type,
            mc_core::Id(member_uuid),
            role.as_str(),
        )
        .await
        .map_err(|e| {
            if is_conflict(&e) {
                Error::Conflict {
                    message: "member already in squad".into(),
                }
            } else {
                repo_err(e, "squad member")
            }
        })?;
    Ok((StatusCode::CREATED, Json(SquadMemberDto::from_row(&row))))
}

/// DELETE /api/squads/:id/members —— 上游 `RemoveSquadMember`（204）。
///
/// 上游从 **JSON body** 取 `{member_type, member_id}`。本仓额外容忍「空 body +
/// query 参数」（`?member_type=&member_id=`）：路由路径里没有成员位，而这个 endpoint
/// 同时服务 REST 客户端；body 非空时仍严格按上游解码（形状不符 → 400）。
pub(super) async fn remove_member(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let scope = SquadScope::resolve(&state, auth, &headers, &query).await?;
    let squad = scope.squad(&id).await?;
    scope.require_can_manage(&squad)?;

    let req: RemoveSquadMemberRequest = if body.is_empty() {
        RemoveSquadMemberRequest {
            member_type: query.get("member_type").cloned(),
            member_id: query.get("member_id").cloned(),
        }
    } else {
        decode_body_or_bad_request(&body)?
    };
    let member_type = req.member_type.unwrap_or_default();
    let member_raw = req.member_id.unwrap_or_default();
    let member_uuid = parse_uuid(&member_raw, "member_id")?;

    if member_type == MEMBER_TYPE_AGENT && squad.leader_id() == mc_core::Id(member_uuid) {
        return Err(bad_request("cannot remove the squad leader; change leader first").into());
    }
    let rows = scope
        .repo
        .remove_member(squad.id(), &member_type, mc_core::Id(member_uuid))
        .await
        .map_err(|e| repo_err(e, "squad member"))?;
    if rows == 0 {
        return Err(Error::NotFound {
            resource: "squad member".into(),
        }
        .into());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// PATCH /api/squads/:id/members/role —— 上游 `UpdateSquadMemberRole`。
pub(super) async fn update_member_role(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<SquadMemberDto>> {
    let scope = SquadScope::resolve(&state, auth, &headers, &query).await?;
    let squad = scope.squad(&id).await?;
    scope.require_can_manage(&squad)?;
    let req: UpdateSquadMemberRoleRequest = decode_body_or_bad_request(&body)?;

    let member_type = req.member_type.unwrap_or_default();
    let member_raw = req.member_id.unwrap_or_default();
    let role = req.role.unwrap_or_default();
    let member_uuid = parse_uuid(&member_raw, "member_id")?;

    let row = scope
        .repo
        .update_member_role(
            squad.id(),
            &member_type,
            mc_core::Id(member_uuid),
            role.as_str(),
        )
        .await
        .map_err(|e| repo_err(e, "squad member"))?
        .ok_or_else(|| Error::NotFound {
            resource: "squad member".into(),
        })?;
    Ok(Json(SquadMemberDto::from_row(&row)))
}
