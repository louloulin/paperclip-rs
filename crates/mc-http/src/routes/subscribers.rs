//! `/api/issues/{id}/subscribers` 等 4 条 issue 订阅路由。
//!
//! 对应上游 `server/internal/handler/subscriber.go` + `server/cmd/server/router.go`
//! L1982-1985（在 `/api/issues/{id}/` 子路由内）：
//!
//! | method | path | upstream handler |
//! |---|---|---|
//! | GET  | `/api/issues/:id/subscribers`          | `ListIssueSubscribers` |
//! | POST | `/api/issues/:id/subscribe`            | `SubscribeToIssue` |
//! | POST | `/api/issues/:id/unsubscribe`          | `UnsubscribeFromIssue` |
//! | POST | `/api/issues/:id/unsubscribe/subtree`  | `UnsubscribeFromIssueSubtree` |
//!
//! 独立文件（不并入 `inbox::router()`）：`mount.rs::mount_slice_subscriber()` 已经把
//! 本文件的 `router()` 与 issue 组接好，合并进 `inbox` 反而会在 axum `.merge` 时因
//! 路径重复 panic。
//!
//! 鉴权：与 inbox 一致 —— `X-Multica-User-Id`（`AuthUser`）+ workspace 上下文
//! （`X-Workspace-ID` / `?workspace_id`）+ 成员校验（非成员 404）；issue 不存在或
//! 不属于该 workspace 一律 404 `"issue not found"`（上游 `loadIssueForUser`）。
//!
//! 与上游的已知偏离（见 `docs/13-M2-INBOX.md`）：
//! - **退订是硬删除**：本仓 `issue_subscriber` 没有上游的 tombstone 列，故退订不留痕、
//!   重新订阅总是成功；子树退订也不会阻止未来新增子 issue 的自动订阅（M3+ 补列）；
//! - 不解析 `X-Agent-ID` / `X-Actor-Source`（上游 `resolveActor` 的 agent 分支），
//!   调用者默认 `user_type='user'`；body 里显式给的 `user_type` 仍然生效；
//! - `X-Workspace-Slug` / `?workspace_slug` 未实现（同 inbox）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use mc_core::Id;
use mc_errors::Error;
use mc_repos::subscriber::{
    IssueSubscriberRepo, IssueSubscriberRow, NewIssueSubscriber, REASON_MANUAL, USER_TYPE_USER,
};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::inbox::resolve_workspace_id;
use crate::routes::invitations::require_workspace_member;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    // axum 0.7 路径参数用 `:id`（matchit 0.7），`{id}` 是字面量段（恒 404）。
    Router::new()
        .route("/api/issues/:id/subscribers", get(list_subscribers))
        .route("/api/issues/:id/subscribe", post(subscribe))
        .route("/api/issues/:id/unsubscribe", post(unsubscribe))
        .route(
            "/api/issues/:id/unsubscribe/subtree",
            post(unsubscribe_subtree),
        )
}

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

/// 上游 `SubscriberResponse` 的逐字对应。
#[derive(Debug, Clone, Serialize)]
struct SubscriberDto {
    issue_id: String,
    user_type: String,
    user_id: String,
    reason: String,
    created_at: String,
}

impl From<&IssueSubscriberRow> for SubscriberDto {
    fn from(row: &IssueSubscriberRow) -> Self {
        Self {
            issue_id: row.issue_id().as_string(),
            user_type: row.user_type.clone(),
            user_id: row.user_id().as_string(),
            reason: row.reason.clone(),
            created_at: row.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
struct SubscribedDto {
    subscribed: bool,
}

/// `POST .../unsubscribe/subtree` 的响应：上游额外带被移除的 issue 列表。
#[derive(Debug, Serialize)]
struct SubtreeUnsubscribedDto {
    subscribed: bool,
    removed_issue_ids: Vec<String>,
}

/// `POST .../subscribe` 的可选 body：`{user_id?, user_type?}`。
///
/// 上游对 body 解码失败是**静默忽略**并落到调用者身份；我们用
/// `Option<Json<..>>`（axum 0.7 对 `Option<T>` 的 blanket impl：缺 Content-Type /
/// body 非法时给 `None`）复刻同样的宽容行为。
#[derive(Debug, Default, Deserialize)]
struct SubscribeBody {
    user_id: Option<String>,
    user_type: Option<String>,
}

// ---------------------------------------------------------------------------
// 请求上下文
// ---------------------------------------------------------------------------

/// 解析 workspace（400）→ 成员校验（404）→ 解析 issue（404）→ 装配仓储。
struct IssueScope {
    workspace_id: Id,
    repo: IssueSubscriberRepo,
}

impl IssueScope {
    async fn resolve(
        state: &AppState,
        user: AuthUser,
        headers: &HeaderMap,
        query: &HashMap<String, String>,
    ) -> Result<Self, Error> {
        let workspace_id = resolve_workspace_id(headers, query)?;
        require_workspace_member(state, workspace_id, user.id()).await?;
        Ok(Self {
            workspace_id,
            repo: IssueSubscriberRepo::new(state.db.clone()),
        })
    }

    /// 解析 issue id 并确认它属于当前 workspace。
    ///
    /// 上游 `loadIssueForUser`：非 UUID 会先尝试 `PREFIX-NUMBER` 标识符解析，失败后
    /// 与"查不到/不属于本 workspace"同样收敛成 404 `"issue not found"`。本切片只支持
    /// UUID（标识符解析属 M2-A 的 `issues.rs`）。
    async fn issue_id(&self, raw_issue_id: &str) -> Result<Id, Error> {
        let id = Id::parse(raw_issue_id).map_err(|_| not_found_issue())?;
        let workspace = self
            .repo
            .issue_workspace(id)
            .await
            .map_err(|e| repo_err(e, "issue"))?;
        if workspace != Some(self.workspace_id) {
            return Err(not_found_issue());
        }
        Ok(id)
    }
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `GET /api/issues/{id}/subscribers`（上游 `ListIssueSubscribers`）。
async fn list_subscribers(
    State(state): State<Arc<AppState>>,
    Path(issue_id): Path<String>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<SubscriberDto>>> {
    let scope = IssueScope::resolve(&state, user, &headers, &query).await?;
    let id = scope.issue_id(&issue_id).await?;
    let rows = scope
        .repo
        .list_for_issue(id)
        .await
        .map_err(|e| repo_err(e, "issue"))?;
    Ok(Json(rows.iter().map(SubscriberDto::from).collect()))
}

/// `POST /api/issues/{id}/subscribe`（上游 `SubscribeToIssue`，幂等）。
async fn subscribe(
    State(state): State<Arc<AppState>>,
    Path(issue_id): Path<String>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<SubscribeBody>>,
) -> ApiResult<Json<SubscribedDto>> {
    let scope = IssueScope::resolve(&state, user, &headers, &query).await?;
    let id = scope.issue_id(&issue_id).await?;
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let (target_id, target_type) = resolve_target(user, &body)?;

    // 目标主体必须是本 workspace 的成员 / agent（上游 `isWorkspaceEntity` → 403）。
    let is_entity = scope
        .repo
        .is_workspace_entity(scope.workspace_id, &target_type, target_id)
        .await
        .map_err(|e| repo_err(e, "issue"))?;
    if !is_entity {
        return Err(Error::Forbidden {
            message: "target user is not a member of this workspace".into(),
        }
        .into());
    }

    scope
        .repo
        .subscribe(NewIssueSubscriber {
            issue_id: id,
            user_type: target_type,
            user_id: target_id,
            // 显式订阅的 reason 是 `manual`（上游同）。
            reason: REASON_MANUAL.to_string(),
        })
        .await
        .map_err(|e| repo_err(e, "issue"))?;
    Ok(Json(SubscribedDto { subscribed: true }))
}

/// `POST /api/issues/{id}/unsubscribe`（上游 `UnsubscribeFromIssue`，幂等）。
async fn unsubscribe(
    State(state): State<Arc<AppState>>,
    Path(issue_id): Path<String>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<SubscribeBody>>,
) -> ApiResult<Json<SubscribedDto>> {
    let scope = IssueScope::resolve(&state, user, &headers, &query).await?;
    let id = scope.issue_id(&issue_id).await?;
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let (target_id, target_type) = resolve_target(user, &body)?;
    // 上游返回固定 `{"subscribed": false}`（即使本来没订阅）。
    scope
        .repo
        .unsubscribe(id, &target_type, target_id)
        .await
        .map_err(|e| repo_err(e, "issue"))?;
    Ok(Json(SubscribedDto { subscribed: false }))
}

/// `POST /api/issues/{id}/unsubscribe/subtree`（上游 `UnsubscribeFromIssueSubtree`）。
async fn unsubscribe_subtree(
    State(state): State<Arc<AppState>>,
    Path(issue_id): Path<String>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<SubscribeBody>>,
) -> ApiResult<Json<SubtreeUnsubscribedDto>> {
    let scope = IssueScope::resolve(&state, user, &headers, &query).await?;
    let id = scope.issue_id(&issue_id).await?;
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let (target_id, target_type) = resolve_target(user, &body)?;
    let removed = scope
        .repo
        .unsubscribe_subtree(id, &target_type, target_id)
        .await
        .map_err(|e| repo_err(e, "issue"))?;
    Ok(Json(SubtreeUnsubscribedDto {
        subscribed: false,
        removed_issue_ids: removed.iter().map(|id| id.as_string()).collect(),
    }))
}

// ---------------------------------------------------------------------------
// helper
// ---------------------------------------------------------------------------

/// 目标主体：body 显式指定优先，否则是调用者本人的 `user` 身份。
fn resolve_target(user: AuthUser, body: &SubscribeBody) -> Result<(Id, String), Error> {
    let raw_id = body.user_id.as_deref().filter(|v| !v.trim().is_empty());
    let target_id = match raw_id {
        Some(raw) => Id::parse(raw.trim()).map_err(|_| Error::Validation {
            message: "invalid user_id".into(),
            details: vec![],
        })?,
        None => user.id(),
    };
    let target_type = match body.user_type.as_deref().filter(|v| !v.trim().is_empty()) {
        Some(raw) => {
            let raw = raw.trim();
            if !IssueSubscriberRepo::is_valid_user_type(raw) {
                return Err(Error::Validation {
                    message: format!("invalid user_type: {raw}"),
                    details: vec![],
                });
            }
            raw.to_string()
        }
        None => USER_TYPE_USER.to_string(),
    };
    Ok((target_id, target_type))
}

fn not_found_issue() -> Error {
    Error::NotFound {
        resource: "issue".into(),
    }
}

fn repo_err(e: mc_repos::RepoError, resource: &str) -> Error {
    match e {
        mc_repos::RepoError::NotFound => Error::NotFound {
            resource: resource.to_string(),
        },
        mc_repos::RepoError::Conflict => Error::Conflict {
            message: format!("{resource} state conflict"),
        },
        mc_repos::RepoError::Db(msg) => Error::Database(msg),
    }
}

// ---------------------------------------------------------------------------
// 单元测试（不依赖 DB）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_builds_without_conflict() {
        let _app: Router<Arc<AppState>> = router();
    }

    #[test]
    fn target_defaults_to_the_caller_as_user() {
        let caller = Id::new();
        let (id, user_type) =
            resolve_target(AuthUser::from_id(caller), &SubscribeBody::default()).unwrap();
        assert_eq!(id, caller);
        assert_eq!(user_type, USER_TYPE_USER);
    }

    #[test]
    fn target_honours_explicit_body_and_rejects_bad_values() {
        let other = Id::new();
        let body = SubscribeBody {
            user_id: Some(format!("  {}  ", other.as_string())),
            user_type: Some(" agent ".into()),
        };
        let (id, user_type) = resolve_target(AuthUser::from_id(Id::new()), &body).unwrap();
        assert_eq!(id, other);
        assert_eq!(user_type, "agent");

        // 空串等于未传（与上游忽略解码失败一致）。
        let blank = SubscribeBody {
            user_id: Some(String::new()),
            user_type: Some("  ".into()),
        };
        let caller = Id::new();
        let (id, user_type) = resolve_target(AuthUser::from_id(caller), &blank).unwrap();
        assert_eq!(id, caller);
        assert_eq!(user_type, USER_TYPE_USER);

        let bad_id = SubscribeBody {
            user_id: Some("nope".into()),
            user_type: None,
        };
        assert_eq!(
            resolve_target(AuthUser::from_id(caller), &bad_id)
                .unwrap_err()
                .message(),
            "validation error: invalid user_id"
        );

        // `member` 是上游词汇；本仓表 CHECK 只有 `user` / `agent`。
        let bad_type = SubscribeBody {
            user_id: None,
            user_type: Some("member".into()),
        };
        assert_eq!(
            resolve_target(AuthUser::from_id(caller), &bad_type)
                .unwrap_err()
                .message(),
            "validation error: invalid user_type: member"
        );
    }
}
