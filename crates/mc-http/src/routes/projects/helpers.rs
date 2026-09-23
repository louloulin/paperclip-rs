//! `/api/projects*` 的公共小工具：错误映射 + 枚举校验 + 请求体读取 + 项目装载 + 计数。
//!
//! 上游对照：`handler/project.go` 的 `validateProjectEnum` / `writeProjectWriteError` /
//! `loadProjectIssueStats` / `loadProjectResourceCount` / `projectTerminalIssueStatusKeys`、
//! `handler/project_resource.go` 的 `loadProjectForResource`，以及 `handler.go` 的
//! `parseUUIDOrBadRequest`。

use std::collections::HashMap;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use chrono::NaiveDate;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::issue::IssueRepo;
use mc_repos::project::{
    ProjectIssueStats, ProjectRepo, ProjectRow, ProjectSearchError, WriteError,
};
use mc_repos::project_resource::ProjectResourceRepo;
use mc_repos::runtime::RuntimeListFilter;
use mc_repos::RepoError;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::ApiError;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::require_workspace_member;
use crate::routes::issues::{resolve_workspace, WorkspaceQuery};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 错误构造
// ---------------------------------------------------------------------------

pub(crate) fn validation(message: impl Into<String>) -> Error {
    Error::Validation {
        message: message.into(),
        details: vec![],
    }
}

pub(crate) fn conflict(message: impl Into<String>) -> Error {
    Error::Conflict {
        message: message.into(),
    }
}

pub(crate) fn not_found(resource: &str) -> Error {
    Error::NotFound {
        resource: resource.to_string(),
    }
}

pub(crate) fn repo_err(err: RepoError) -> Error {
    match err {
        RepoError::NotFound => not_found("project"),
        other => Error::Database(other.to_string()),
    }
}

/// `parseUUIDOrBadRequest`：形态非法 → 400 `invalid <field>`（`field` 由调用方给全，
/// 例如 `"project id"` / `"workspace id"` / `"lead_id"` / `"resource id"`）。
pub(crate) fn parse_uuid(field: &str, raw: &str) -> Result<Id, Error> {
    Id::parse(raw.trim()).map_err(|_| validation(format!("invalid {field}")))
}

/// `util.ParseCalendarDate`：只接受 `YYYY-MM-DD`，其它一律 400。
pub(crate) fn parse_calendar_date(field: &str, raw: &str) -> Result<NaiveDate, Error> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| validation(format!("invalid {field} format, expected YYYY-MM-DD")))
}

/// 上游 `validateProjectEnum`：命中返回 `Ok(())`，否则 400
/// `invalid <field> "<value>"; valid values: a, b, c`。
pub(crate) fn validate_enum(field: &str, value: &str, allowed: &[&str]) -> Result<(), Error> {
    if allowed.contains(&value) {
        return Ok(());
    }
    Err(validation(format!(
        "invalid {field} \"{value}\"; valid values: {}",
        allowed.join(", ")
    )))
}

/// 上游 `writeProjectWriteError`：CHECK 违反（SQLSTATE 23514）→ 400，其余 → 500
/// `failed to <action> project`（底层错误只进日志，不上线）。
pub(crate) fn project_write_err(err: WriteError, action: &str) -> Error {
    match err {
        WriteError::CheckViolation => validation(format!(
            "project {action} rejected: a field value failed a database constraint"
        )),
        WriteError::Repo(RepoError::NotFound) => not_found("project"),
        WriteError::Repo(other) => {
            tracing::error!(error = %other, action, "project write failed");
            Error::Internal(format!("failed to {action} project"))
        }
        // project 表除主键外没有唯一约束，走到这里说明是捆绑创建的资源行
        // （调用方应先匹配 `ResourceConflict` / `UniqueViolation` 再落到本分支）。
        WriteError::UniqueViolation | WriteError::ResourceConflict { .. } => {
            tracing::error!(action, "unexpected unique violation on project write");
            Error::Internal(format!("failed to {action} project"))
        }
    }
}

/// 资源子集合的写错误口径（上游只特判唯一违反，其余含 CHECK 都是 500）。
pub(crate) fn resource_write_err(err: WriteError, action: &str, unique: &str) -> Error {
    match err {
        WriteError::UniqueViolation => conflict(unique),
        WriteError::ResourceConflict { .. } => conflict(unique),
        WriteError::Repo(RepoError::NotFound) => not_found("project resource"),
        other => {
            tracing::error!(error = %other, action, "project resource write failed");
            Error::Internal(format!("failed to {action}"))
        }
    }
}

/// 搜索超时的 503（上游 `writeError(w, 503, "search timed out; please refine your query or
/// try again")`）。本仓没有映射到 503 的 `Error` 变体，故用 `ApiError::respond_with` ——
/// 错误体形状与其它端点完全一致。
pub(crate) fn search_timeout_response() -> Response {
    ApiError(Error::Internal(
        "search timed out; please refine your query or try again".into(),
    ))
    .respond_with(StatusCode::SERVICE_UNAVAILABLE)
}

/// 其余搜索失败 → 500 `failed to search projects`（调用方已先拦下 `Timeout`）。
pub(crate) fn search_err(err: ProjectSearchError) -> Error {
    match err {
        ProjectSearchError::Timeout => Error::Internal("search timed out".into()),
        ProjectSearchError::Repo(other) => {
            tracing::error!(error = %other, "project search failed");
            Error::Internal("failed to search projects".into())
        }
    }
}

// ---------------------------------------------------------------------------
// 仓储句柄
// ---------------------------------------------------------------------------

pub(crate) fn project_repo(state: &AppState) -> ProjectRepo {
    ProjectRepo::new(state.db.clone())
}

pub(crate) fn resource_repo(state: &AppState) -> ProjectResourceRepo {
    ProjectResourceRepo::new(state.db.clone())
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// 读 JSON 体：语法 / 形状非法 → 400 `invalid request body`（上游 `json.Decode` 口径）。
///
/// 用 `Bytes` 而不是 `Json<T>`，就是为了把 axum 自己的 rejection 体换成本仓统一的
/// `{"error":{"code","message"}}` 且文案与上游逐字一致。
pub(crate) fn parse_body<T: DeserializeOwned>(body: &Bytes) -> Result<T, Error> {
    serde_json::from_slice::<T>(body).map_err(|_| validation("invalid request body"))
}

// ---------------------------------------------------------------------------
// 项目装载
// ---------------------------------------------------------------------------

/// 解析 `:id` + workspace + 成员身份，装载项目行（`loadProjectForResource` 的等价物）。
///
/// 上游此路径不做成员校验（只有 `requireUserID`）；本仓沿用 M1 的 dev-mode workspace
/// 来源，因此补上成员门（非成员 → 404 `workspace`）。顺序与上游一致：
/// 先 400 `invalid project id` → 400 `invalid workspace id` → 404 `project not found`。
pub(crate) async fn load_project_scoped(
    state: &AppState,
    headers: &HeaderMap,
    query: &WorkspaceQuery,
    user: &AuthUser,
    raw_project_id: &str,
) -> Result<(Id, ProjectRow), Error> {
    let project_id = parse_uuid("project id", raw_project_id)?;
    let workspace_id = resolve_workspace(state, headers, query).await?;
    require_workspace_member(state, workspace_id, user.id()).await?;
    let row = project_repo(state)
        .get_in_workspace(project_id, workspace_id)
        .await
        .map_err(repo_err)?
        .ok_or_else(|| not_found("project"))?;
    Ok((workspace_id, row))
}

/// 上游 `DeleteProject` 的角色门 `requireWorkspaceRole(…, "project not found", "owner",
/// "admin")`：非成员 → 404 `project not found`（注意不是 `workspace`），成员但角色不足
/// → 403 `insufficient permissions`。
pub(crate) async fn require_project_admin(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> Result<(), Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.0)
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
    let role = row.ok_or_else(|| not_found("project"))?.0;
    if !matches!(role.as_str(), "owner" | "admin") {
        return Err(Error::Forbidden {
            message: "insufficient permissions".into(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 计数投影
// ---------------------------------------------------------------------------

/// 项目终态 status key：读不到自定义目录时退化成 canonical `done` / `cancelled`
/// （上游 `projectTerminalIssueStatusKeys`）。
async fn terminal_status_keys(state: &AppState, workspace_id: Id) -> Vec<String> {
    match IssueRepo::new(state.db.clone())
        .terminal_status_keys(workspace_id)
        .await
    {
        Ok(keys) => keys,
        Err(err) => {
            tracing::warn!(error = %err, "expand project terminal status categories failed");
            vec!["done".to_string(), "cancelled".to_string()]
        }
    }
}

/// 批量取 `(issue_count, done_count)`。上游对统计失败**不致命**（置 0 继续）。
pub(crate) async fn issue_stats_map(
    state: &AppState,
    workspace_id: Id,
    project_ids: &[Uuid],
) -> HashMap<Uuid, (i64, i64)> {
    let mut map = HashMap::new();
    if project_ids.is_empty() {
        return map;
    }
    let keys = terminal_status_keys(state, workspace_id).await;
    match project_repo(state)
        .issue_stats(workspace_id, project_ids, &keys)
        .await
    {
        Ok(stats) => {
            for ProjectIssueStats {
                project_id,
                total_count,
                done_count,
            } in stats
            {
                map.insert(project_id, (total_count, done_count));
            }
        }
        Err(err) => tracing::warn!(error = %err, "project issue stats failed"),
    }
    map
}

/// 批量取资源数。同样对失败宽容（缺键 ⇒ 0）。
pub(crate) async fn resource_count_map(
    state: &AppState,
    project_ids: &[Uuid],
) -> HashMap<Uuid, i64> {
    let mut map = HashMap::new();
    if project_ids.is_empty() {
        return map;
    }
    match resource_repo(state).resource_counts(project_ids).await {
        Ok(rows) => {
            for row in rows {
                map.insert(row.project_id, row.resource_count);
            }
        }
        Err(err) => tracing::warn!(error = %err, "project resource counts failed"),
    }
    map
}

// ---------------------------------------------------------------------------
// 运行时光标（`local_directory` worktree 门用）
// ---------------------------------------------------------------------------

/// 上游 `ListAgentRuntimes` 的等价读：一个 workspace 的运行时行（不按 owner 过滤）。
pub(crate) async fn workspace_runtimes(
    state: &AppState,
    workspace_id: Id,
) -> Result<Vec<mc_repos::runtime::AgentRuntimeRow>, Error> {
    mc_repos::runtime::AgentRuntimeRepo::new(state.db.clone())
        .list(workspace_id, RuntimeListFilter::All)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "failed to check runtime capabilities");
            Error::Internal("failed to check runtime capabilities".into())
        })
}

// ---------------------------------------------------------------------------
// serde 小工具
// ---------------------------------------------------------------------------

/// 三态字段：JSON 里**缺失** → `None`；`null` → `Some(None)`；有值 → `Some(Some(v))`。
///
/// 上游用 `map[string]json.RawMessage` 的 key 存在性区分「缺失 / 显式 null」；serde 里
/// 要 `Option<Option<T>>` 才有第三态（与 `issues::helpers::double_option` 同款，
/// 那边是 `pub(crate)` 未导出，故此处自带一份）。
pub(crate) fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}
