//! 认证占位中间件：`require_user` / `require_member` / `require_role`。
//!
//! ## 占位语义（sub-issue B `/auth/verify-code` 落地后回来收紧）
//!
//! - session id 取自 `X-Multica-Session` header 或 cookie `multica_session`；
//! - 解析顺序：
//!   1. 查 in-memory session store（`mc_auth::SessionStoreContainer`），命中 → `session.user_id`
//!      （sub-issue B 颁发的正式 session 走这条）；
//!   2. 未命中 → 若字符串本身是合法 UUID，直接当作 user id 接受（本地联调通道：
//!      测试 / 本地 curl 直接传 user id 即可）；
//!   3. 否则 → 401。
//! - 认证结果写入 request extensions：`AuthUser`（`user_id`）、
//!   `WorkspacePathId`（`workspace_id`，自 URL `/api/workspaces/{id}` 解析）、
//!   `WorkspaceContext`（membership 校验通过后的 `member_id` + `role`）。
//!
//! 状态码与上游 `server/internal/middleware/workspace.go` 对齐：
//! 未认证 401、非成员 404（workspace not found，隐藏资源存在性）、角色不足 403。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use mc_core::member::WorkspaceMember;
use mc_core::workspace::WorkspaceRole;
use mc_core::Id;
use mc_repos::member::MemberRepo;
use mc_repos::RepoError;

use crate::error::ApiError;
use crate::state::AppState;
use mc_errors::Error;

/// 已认证用户（request extension）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthUser(pub Id);

/// URL 中解析出的 workspace id（request extension）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspacePathId(pub Id);

/// membership 校验通过后的成员上下文（request extension）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceContext {
    pub workspace_id: Id,
    pub member_id: Id,
    pub role: WorkspaceRole,
}

/// 从请求头提取 session id：优先 `X-Multica-Session`，回退 cookie `multica_session`。
pub fn session_id_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers
        .get("x-multica-session")
        .and_then(|v| v.to_str().ok())
    {
        let t = v.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let cookie = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("multica_session=") {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// 占位 session 解析：store 命中 → `session.user_id`；否则 UUID 直通；否则 None。
pub async fn resolve_session_user(state: &AppState, headers: &HeaderMap) -> Option<Id> {
    let sid = session_id_from_headers(headers)?;
    if let Ok(sess) = state.auth.store().get(&sid).await {
        return Some(sess.user_id);
    }
    // 占位通道：任意字符串 session id 里若是 UUID，直接当 user id（本地联调）。
    Id::parse(&sid).ok()
}

/// 从 URL path 解析 `/api/workspaces/{id}[/...]` 中的 workspace id。
pub fn workspace_id_from_path(uri: &Uri) -> Option<Id> {
    let mut segs = uri.path().split('/').filter(|s| !s.is_empty());
    if segs.next()? != "api" {
        return None;
    }
    if segs.next()? != "workspaces" {
        return None;
    }
    Id::parse(segs.next()?).ok()
}

fn err_response(err: Error) -> Response {
    ApiError(err).into_response()
}

fn unauthorized() -> Response {
    err_response(Error::Unauthorized {
        message: "user not authenticated".into(),
    })
}

fn workspace_not_found() -> Response {
    err_response(Error::NotFound {
        resource: "workspace".into(),
    })
}

fn internal(msg: String) -> Response {
    err_response(Error::Internal(msg))
}

/// 认证 + 把 `user_id` / `workspace_id`（来自 URL）写入 extensions。
/// 失败时返回 401 response。
/// 中间件内部 Result 别名：错误即待返回的 HTTP 响应，装箱以避免 `Result` 过大。
type MiddlewareResult<T> = Result<T, Box<Response>>;

async fn authenticate(state: &AppState, req: &mut Request) -> MiddlewareResult<Id> {
    let user_id = resolve_session_user(state, req.headers())
        .await
        .ok_or_else(|| Box::new(unauthorized()))?;
    req.extensions_mut().insert(AuthUser(user_id));
    if let Some(ws) = workspace_id_from_path(req.uri()) {
        req.extensions_mut().insert(WorkspacePathId(ws));
    }
    Ok(user_id)
}

/// membership 查询（404 = workspace 不存在或非成员，与上游一致）。
///
/// 注意：只按值传递 `Id`，不跨 await 持有 `&Request`
///（`Request` 内部 body 非 `Sync`，共享引用跨 await 会破坏 future 的 `Send`）。
async fn check_membership(
    state: &AppState,
    user_id: Id,
    workspace_id: Id,
) -> MiddlewareResult<WorkspaceMember> {
    MemberRepo::new(state.db.clone())
        .get_for_user(workspace_id, user_id)
        .await
        .map_err(|e| {
            Box::new(match e {
                RepoError::NotFound => workspace_not_found(),
                other => internal(other.to_string()),
            })
        })
}

fn insert_context(req: &mut Request, workspace_id: Id, member: &WorkspaceMember) {
    req.extensions_mut().insert(WorkspaceContext {
        workspace_id,
        member_id: member.id,
        role: member.role,
    });
}

/// 仅认证：任何登录用户可通过。写入 `AuthUser`（及 URL 里的 `WorkspacePathId`）。
pub async fn require_user(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Err(resp) = authenticate(&state, &mut req).await {
        return *resp;
    }
    next.run(req).await
}

/// 认证 + workspace membership 校验。写入 `AuthUser` / `WorkspacePathId` / `WorkspaceContext`。
pub async fn require_member(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let user_id = match authenticate(&state, &mut req).await {
        Ok(u) => u,
        Err(resp) => return *resp,
    };
    let Some(ws_id) = req.extensions().get::<WorkspacePathId>().map(|w| w.0) else {
        return workspace_not_found();
    };
    let member = match check_membership(&state, user_id, ws_id).await {
        Ok(m) => m,
        Err(resp) => return *resp,
    };
    insert_context(&mut req, ws_id, &member);
    next.run(req).await
}

/// `require_role` 中间件的共享状态：应用状态 + 允许的角色集合。
#[derive(Clone)]
pub struct RoleMiddlewareState {
    pub app: Arc<AppState>,
    pub roles: Vec<WorkspaceRole>,
}

/// `require_role` 守卫的 future 类型（显式装箱，便于以 fn 指针参与类型推断）。
pub type RoleGuardFuture = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;
/// `require_role_guard` 的 fn 指针类型（`FromFnLayer` 的泛型实参，可具名）。
pub type RoleGuardFn = fn(State<RoleMiddlewareState>, Request, Next) -> RoleGuardFuture;
/// `require_role` 返回的具体 layer 类型（具名，`route_layer` 可推断关联类型）。
pub type RoleLayer = axum::middleware::FromFnLayer<
    RoleGuardFn,
    RoleMiddlewareState,
    (State<RoleMiddlewareState>, Request),
>;

/// `require_role` 守卫体：认证 + membership + `role ∈ roles`（否则 403）。
pub fn require_role_guard(
    State(rs): State<RoleMiddlewareState>,
    mut req: Request,
    next: Next,
) -> RoleGuardFuture {
    Box::pin(async move {
        let user_id = match authenticate(&rs.app, &mut req).await {
            Ok(u) => u,
            Err(resp) => return *resp,
        };
        let Some(ws_id) = req.extensions().get::<WorkspacePathId>().map(|w| w.0) else {
            return workspace_not_found();
        };
        let member = match check_membership(&rs.app, user_id, ws_id).await {
            Ok(m) => m,
            Err(resp) => return *resp,
        };
        if !rs.roles.contains(&member.role) {
            return err_response(Error::Forbidden {
                message: "insufficient permissions".into(),
            });
        }
        insert_context(&mut req, ws_id, &member);
        next.run(req).await
    })
}

/// `require_role(roles)`：验证当前用户在该 workspace 的 role 属于 `roles`。
///
/// 用法（配合 `Router::route_layer`，片段非独立可编译单元，故用 `text`）：
/// ```text
/// .route_layer(require_role(state.clone(), &[WorkspaceRole::Owner]))
/// ```
pub fn require_role(app: Arc<AppState>, roles: &[WorkspaceRole]) -> RoleLayer {
    let guard: RoleGuardFn = require_role_guard;
    axum::middleware::from_fn_with_state(
        RoleMiddlewareState {
            app,
            roles: roles.to_vec(),
        },
        guard,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn session_prefers_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-multica-session", HeaderValue::from_static("abc"));
        headers.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_static("multica_session=xyz"),
        );
        assert_eq!(session_id_from_headers(&headers).as_deref(), Some("abc"));
    }

    #[test]
    fn session_falls_back_to_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_static("other=1; multica_session=xyz; more=2"),
        );
        assert_eq!(session_id_from_headers(&headers).as_deref(), Some("xyz"));
    }

    #[test]
    fn session_absent_is_none() {
        assert_eq!(session_id_from_headers(&HeaderMap::new()), None);
    }

    #[test]
    fn workspace_id_parsed_from_url() {
        let id = Id::new();
        let uri: Uri = format!("/api/workspaces/{id}/members").parse().unwrap();
        assert_eq!(workspace_id_from_path(&uri), Some(id));
    }

    #[test]
    fn workspace_id_rejects_other_paths() {
        let uri: Uri = "/api/me".parse().unwrap();
        assert_eq!(workspace_id_from_path(&uri), None);
        let uri: Uri = "/api/workspaces/not-a-uuid".parse().unwrap();
        assert_eq!(workspace_id_from_path(&uri), None);
    }
}
