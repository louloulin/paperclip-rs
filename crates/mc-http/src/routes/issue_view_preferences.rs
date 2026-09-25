//! `/api/issue-view-preferences`（2 条，M2-A 尾片 / LUM-1691）。
//!
//! 上游 `server/internal/handler/issue_view_preference.go` +
//! `server/cmd/server/router.go:2136-2137`：
//!
//! ```text
//! r.Get("/api/issue-view-preferences", h.GetIssueViewPreference)   // 2136
//! r.Put("/api/issue-view-preferences", h.PutIssueViewPreference)   // 2137
//! ```
//!
//! 两条都是 **plain 注册**（不是 `Route(...) + Get("/")`）⇒ fixture 记的是无斜杠路径
//! ⇒ **只注册无斜杠形态**（补一条 `/api/issue-view-preferences/` 会被
//! `slash_alias_audit.py` 判 `EXTRA_ALIAS`，与上游不符）。
//!
//! 为什么单独一个文件：上游这一面自成一份 handler 文件，且 R7 单文件 800 行上限
//! （门禁 ⑩）不允许把它和 `/api/issue-views/*` 挤在同一个 `issue_views.rs` 里。
//! 表 `issue_view_preference`（上游 `268_issue_view_preference.up.sql`）**没有外键**，
//! 复合主键 `(workspace_id, user_id, scope_type, scope_id)`。
//!
//! 语义要点（照上游，偏差表见 `docs/63-M2A-TAIL-ISSUE-VIEW-PIN.md`）：
//! - **`scope_id` 永不为 NULL**：`workspace` → workspace id、`my` → user id、
//!   `project` → project id（`project` 必须先在本 workspace 存在，否则 404）。回填口径在
//!   `mc_repos::issue_view::preference_scope_id`，有纯单测。
//! - **`GET` 无记录 = 200**：`prefs` 是 `{}`、`updated_at` 是 **空串**（上游 Go 字段没有
//!   `omitempty` ⇒ 空串照样出现在 JSON 里），**不是 404**。
//! - **`PUT` 是整文档覆盖**：单用户数据，last-write-wins，没有 revision 闸门。
//! - **`prefs` 必须 JSON 对象**：缺失 ⇒ `{}`；显式 `null` 或数组 ⇒ 400
//!   （靠 [`double_option`] 区分「缺失」与「给了 null」，见 `issue_views.rs` 的说明）。
//! - `prefs` 文档形状（`{"hidden":[…],"order":[…]}`，item id 形如 `builtin:<key>` /
//!   `view:<uuid>`）**在服务端是不透明**的，只有客户端 `definition_version` 契约懂它。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use mc_errors::Error;
use mc_repos::issue_view::{is_json_object, preference_scope_id};
use mc_repos::project::ProjectRepo;
use mc_repos::RepoError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::{not_found, require_workspace_member};
use crate::routes::issue_views::{double_option, issue_view_repo, parse_view_body, view_err};
use crate::routes::issues::{parse_target_id, resolve_workspace, validation, WorkspaceQuery};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 请求 / 响应类型
// ---------------------------------------------------------------------------

/// `GET /api/issue-view-preferences` 的查询。
#[derive(Debug, Default, Deserialize)]
pub struct PreferenceQuery {
    #[serde(default)]
    pub scope_type: Option<String>,
    #[serde(default)]
    pub scope_id: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_slug: Option<String>,
}

impl PreferenceQuery {
    fn selector(&self) -> WorkspaceQuery {
        WorkspaceQuery {
            workspace_id: self.workspace_id.clone(),
            workspace_slug: self.workspace_slug.clone(),
        }
    }
}

/// `PUT /api/issue-view-preferences`（上游 `PutIssueViewPreferenceRequest`）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
#[allow(clippy::option_option)] // 见 `double_option`：「键缺失」与「显式 null」必须分得开
pub struct PutPreferenceRequest {
    pub scope_type: String,
    pub scope_id: Option<String>,
    /// 三层：缺失 ⇒ `{}`；显式 `null` ⇒ 400；对象 ⇒ 落盘（见 `double_option`）。
    #[serde(deserialize_with = "double_option")]
    pub prefs: Option<Option<JsonValue>>,
}

/// 视图栏偏好响应（上游 `IssueViewPreferenceResponse`）。
///
/// `updated_at` **不是** `Option`：上游 Go 字段没有 `omitempty`，无记录时它也渲染成 `""`。
#[derive(Debug, Clone, Serialize)]
pub struct IssueViewPreferenceResponse {
    pub scope_type: String,
    pub scope_id: Option<String>,
    pub prefs: JsonValue,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

/// 2 条上游键 / 2 个注册点（都是 plain 注册，**没有**尾斜杠别名）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/api/issue-view-preferences",
        get(get_issue_view_preference).put(put_issue_view_preference),
    )
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `GET /api/issue-view-preferences`（上游 `GetIssueViewPreference`）。
async fn get_issue_view_preference(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<PreferenceQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssueViewPreferenceResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query.selector()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let scope_type = query.scope_type.clone().unwrap_or_default();
    let scope_id = resolve_preference_scope(
        &state,
        workspace_id,
        user.id(),
        &scope_type,
        query.scope_id.as_deref(),
    )
    .await?;

    match issue_view_repo(&state)
        .get_preference(workspace_id, user.id(), &scope_type, scope_id)
        .await
        .map_err(view_err)?
    {
        Some(pref) => {
            let scope_id_str = pref.scope_id().to_string();
            Ok(Json(IssueViewPreferenceResponse {
                scope_type: pref.scope_type,
                scope_id: Some(scope_id_str),
                prefs: pref.prefs,
                updated_at: pref.updated_at.to_rfc3339(),
            }))
        }
        // 还没有行 ⇒ 空文档，不是错误（上游 `pgx.ErrNoRows` 分支）。
        None => Ok(Json(IssueViewPreferenceResponse {
            scope_type,
            scope_id: Some(scope_id.to_string()),
            prefs: json!({}),
            updated_at: String::new(),
        })),
    }
}

/// `PUT /api/issue-view-preferences`（上游 `PutIssueViewPreference`；整文档覆盖）。
async fn put_issue_view_preference(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<IssueViewPreferenceResponse>> {
    let req: PutPreferenceRequest = parse_view_body(&body)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let scope_id = resolve_preference_scope(
        &state,
        workspace_id,
        user.id(),
        &req.scope_type,
        req.scope_id.as_deref(),
    )
    .await?;
    let prefs = match req.prefs {
        // 缺失 ⇒ 空文档；显式 `null` ⇒ 400（上游把 `null` 当「给了一个非对象」）。
        None => json!({}),
        Some(Some(value)) if is_json_object(&value) => value,
        _ => return Err(validation("prefs must be a JSON object").into()),
    };

    let row = issue_view_repo(&state)
        .upsert_preference(workspace_id, user.id(), &req.scope_type, scope_id, &prefs)
        .await
        .map_err(view_err)?;
    let scope_id_str = row.scope_id().to_string();
    Ok(Json(IssueViewPreferenceResponse {
        scope_type: row.scope_type,
        scope_id: Some(scope_id_str),
        prefs: row.prefs,
        updated_at: row.updated_at.to_rfc3339(),
    }))
}

// ---------------------------------------------------------------------------
// 内部 helper
// ---------------------------------------------------------------------------

/// preference 的 `scope_type` 校验 + `scope_id` 回填（上游 `resolvePreferenceScope`）。
///
/// 只有 `project` 分支要解析 / 校验 project id（且必须在本 workspace 存在）；
/// `workspace` / `my` 的 `scope_id` 由 [`preference_scope_id`] 从 workspace id / user id 回填，
/// 保证复合主键不依赖 NULL。
async fn resolve_preference_scope(
    state: &AppState,
    workspace_id: mc_core::Id,
    user_id: mc_core::Id,
    scope_type: &str,
    raw_scope_id: Option<&str>,
) -> Result<mc_core::Id, Error> {
    let raw_scope_id = raw_scope_id.unwrap_or("").trim();
    let project_id = if scope_type == "project" {
        if raw_scope_id.is_empty() {
            return Err(validation("scope_id is required for project scope"));
        }
        let project_id = parse_target_id("scope_id", raw_scope_id)?;
        match ProjectRepo::new(state.db.clone())
            .get_in_workspace(project_id.0, workspace_id)
            .await
        {
            Ok(Some(_)) => Some(project_id),
            Err(RepoError::Db(message)) => return Err(Error::Database(message)),
            // `Ok(None)`（本 workspace 没有该项目）与其余仓储错误都按 404 处理。
            _ => return Err(not_found("project")),
        }
    } else {
        None
    };
    preference_scope_id(scope_type, workspace_id, user_id, project_id)
        .ok_or_else(|| validation("invalid scope_type"))
}

// ---------------------------------------------------------------------------
// 纯单测（无需 DB）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_builds_without_panicking() {
        // 2 条 plain 注册，无尾斜杠别名 ⇒ 不得撞键。
        let _ = router();
    }

    #[test]
    fn prefs_distinguishes_absence_from_explicit_null() {
        // 缺失 ⇒ 保留 None（路由层落 `{}`）；显式 null ⇒ Some(None)（路由层 400）。
        let absent: PutPreferenceRequest =
            serde_json::from_str(r#"{"scope_type":"workspace"}"#).unwrap();
        assert_eq!(absent.prefs, None);
        let explicit: PutPreferenceRequest =
            serde_json::from_str(r#"{"scope_type":"workspace","prefs":null}"#).unwrap();
        assert_eq!(explicit.prefs, Some(None));
        let object: PutPreferenceRequest =
            serde_json::from_str(r#"{"scope_type":"workspace","prefs":{"order":[]}}"#).unwrap();
        assert_eq!(object.prefs, Some(Some(json!({"order": []}))));
        // `scope_id` 是普通 `Option`（`null` 与缺失同义，照 Go 的 `*string`）。
        let null_scope_id: PutPreferenceRequest =
            serde_json::from_str(r#"{"scope_type":"project","scope_id":null,"prefs":{}}"#).unwrap();
        assert!(null_scope_id.scope_id.is_none());
    }

    #[test]
    fn preference_response_keeps_updated_at_empty_not_omitted() {
        // 上游字段没有 `omitempty` ⇒ 无记录时 JSON 里仍有 `"updated_at": ""`。
        let value = serde_json::to_value(IssueViewPreferenceResponse {
            scope_type: "workspace".into(),
            scope_id: Some("11111111-1111-1111-1111-111111111111".into()),
            prefs: json!({}),
            updated_at: String::new(),
        })
        .unwrap();
        assert_eq!(value["updated_at"], json!(""));
        assert_eq!(value["prefs"], json!({}));
        assert!(value.get("scope_id").is_some());
    }
}
