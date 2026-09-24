//! `/v1/storage*`（**4 个注册键**）+ handler 共享实现（bridge 面复用）。
//!
//! - **写者**：M6-7（`docs/57` §3.2）。
//! - **上游**：`router.go:108-111` 的挂载点，加上 `internal/handler/plugin_action.go` 的
//!   `ListPluginStorage` / `GetPluginStorage` / `PutPluginStorage` / `DeletePluginStorage`，
//!   以及 `internal/service/plugin_storage.go`。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/v1/storage/:scope` | GET | `router.go:108` |
//! | `/v1/storage/:scope/:key` | GET, PUT, DELETE | `router.go:109-111` |
//!
//! - **`:scope` 是 `workspace` / `user` 两态**（迁移 `344` 的 CHECK）：未知取值 ⇒ 400，不是 404。
//!   `scope_id` 由**凭据**决定（`user` ⇒ 调用者用户 id，`workspace` ⇒ 工作区 id）——
//!   **不要**从请求体/查询串里取 `scope_id`（那等于让调用者读写别人的键）。
//! - **配额**：1000 键 / 5 MiB（软配额、无淘汰）超限返回 507；键不存在时 `GET` ⇒ 404、
//!   `DELETE` ⇒ 404（上游代码如此，见 `mc-repos/src/plugin/storage.rs` 文件头的差异 2）。
//! - **共享实现**：本文件的 handler 被 `routes/plugin_bridge/storage.rs` 直接注册到另一个前缀上。
//! - **不做什么**：不做加密（storage 是明文不透明值；密文面是 `plugin_secret`）。
//!
//! 行预算（门 ⑩）：本文件 ≤400 行。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mc_openapi::v1::{
    PutStorageValueRequest, StorageKey, StorageKeyListResponse, StorageValueResponse,
};
use mc_plugin_host::scope::{SCOPE_STORAGE_USER, SCOPE_STORAGE_WORKSPACE};

use mc_repos::plugin::storage::{self as storage, StorageError, StorageRepo};

use super::issues::timestamp;
use super::policy::{self as policy, ActionCaller, ActionError, ActionResult};
use crate::state::AppState;

/// `/v1/storage*`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/storage/:scope", get(list_storage_keys))
        .route(
            "/v1/storage/:scope/:key",
            get(get_storage_value)
                .put(put_storage_value)
                .delete(delete_storage_value),
        )
}

// ---------------------------------------------------------------------------
// handler（两侧挂载点共用）
// ---------------------------------------------------------------------------

/// `GET /v1/storage/:scope`：列出键（**不含**值）。
pub(crate) async fn list_storage_keys(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(scope): Path<String>,
) -> Response {
    let request_id = policy::request_id(&headers);
    match list_storage_keys_inner(&state, &headers, &scope).await {
        Ok(response) => response,
        Err(error) => error.into_response_for(&request_id),
    }
}

async fn list_storage_keys_inner(
    state: &AppState,
    headers: &HeaderMap,
    scope: &str,
) -> ActionResult<Response> {
    let caller = policy::resolve_caller(state, headers, "").await?;
    let (scope_type, scope_id) = resolve_scope(&caller, scope)?;

    let rows = storage_repo(state)
        .list_keys(caller.installation.id(), scope_type, scope_id)
        .await
        .map_err(map_storage_error)?;
    let keys = rows
        .into_iter()
        .map(|row| StorageKey {
            key: row.key,
            size_bytes: row.size_bytes,
            updated_at: timestamp(row.updated_at),
        })
        .collect();
    Ok(Json(StorageKeyListResponse { keys }).into_response())
}

/// `GET /v1/storage/:scope/:key`。
pub(crate) async fn get_storage_value(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((scope, key)): Path<(String, String)>,
) -> Response {
    let request_id = policy::request_id(&headers);
    match get_storage_value_inner(&state, &headers, &scope, &key).await {
        Ok(response) => response,
        Err(error) => error.into_response_for(&request_id),
    }
}

async fn get_storage_value_inner(
    state: &AppState,
    headers: &HeaderMap,
    scope: &str,
    key: &str,
) -> ActionResult<Response> {
    let caller = policy::resolve_caller(state, headers, "").await?;
    let (scope_type, scope_id) = resolve_scope(&caller, scope)?;

    let value = storage_repo(state)
        .get_value(caller.installation.id(), scope_type, scope_id, key)
        .await
        .map_err(map_storage_error)?;
    Ok(Json(StorageValueResponse { value }).into_response())
}

/// `PUT /v1/storage/:scope/:key` ⇒ 204。
pub(crate) async fn put_storage_value(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((scope, key)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let request_id = policy::request_id(&headers);
    match put_storage_value_inner(&state, &headers, &scope, &key, &body).await {
        Ok(response) => response,
        Err(error) => error.into_response_for(&request_id),
    }
}

async fn put_storage_value_inner(
    state: &AppState,
    headers: &HeaderMap,
    scope: &str,
    key: &str,
    body: &Bytes,
) -> ActionResult<Response> {
    let caller = policy::resolve_caller(state, headers, "").await?;
    let (scope_type, scope_id) = resolve_scope(&caller, scope)?;

    let request: PutStorageValueRequest = decode(body)?;
    storage_repo(state)
        .set_value(
            caller.installation.id(),
            scope_type,
            scope_id,
            key,
            &request.value,
        )
        .await
        .map_err(map_storage_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `DELETE /v1/storage/:scope/:key` ⇒ 204。
pub(crate) async fn delete_storage_value(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((scope, key)): Path<(String, String)>,
) -> Response {
    let request_id = policy::request_id(&headers);
    match delete_storage_value_inner(&state, &headers, &scope, &key).await {
        Ok(response) => response,
        Err(error) => error.into_response_for(&request_id),
    }
}

async fn delete_storage_value_inner(
    state: &AppState,
    headers: &HeaderMap,
    scope: &str,
    key: &str,
) -> ActionResult<Response> {
    let caller = policy::resolve_caller(state, headers, "").await?;
    let (scope_type, scope_id) = resolve_scope(&caller, scope)?;

    storage_repo(state)
        .delete_value(caller.installation.id(), scope_type, scope_id, key)
        .await
        .map_err(map_storage_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// scope / 错误
// ---------------------------------------------------------------------------

/// 上游 `pluginStorageScope`：先判**该 scope 需要的授予**，再判 actor，最后解析 `scope_id`。
///
/// 顺序是上游的语义的一部分：未知 scope 名先撞上 `storage:workspace` 的授予判定，所以一个
/// 没有该 scope 的插件拿到的是 403（而不是「猜对了 scope 名才有 400」）。
fn resolve_scope<'a>(
    caller: &ActionCaller,
    scope_type: &'a str,
) -> ActionResult<(&'a str, mc_core::Id)> {
    let required = if scope_type == storage::SCOPE_USER {
        SCOPE_STORAGE_USER
    } else {
        SCOPE_STORAGE_WORKSPACE
    };
    if !caller.has_scope(required) {
        return Err(ActionError::new(
            StatusCode::FORBIDDEN,
            "missing_scope",
            format!("this Plugin was not granted the {required} scope"),
        ));
    }
    // `storage:user` 是**每成员**状态：没有成员身份的调用者根本解析不出这个 scope。
    // 若放行，每个 plugin-actor 都会写到零 UUID 那一个桶里 —— 一个伪装的「某人的私有桶」。
    let user_id = if scope_type == storage::SCOPE_USER {
        Some(caller.require_member()?)
    } else {
        None
    };
    let scope_id = storage::resolve_scope(scope_type, caller.workspace_id, user_id)
        .map_err(map_storage_error)?;
    Ok((scope_type, scope_id))
}

/// `mc-repos` 的存储错误 → 本契约的状态码（上游 `PluginError` 四态）。
fn map_storage_error(error: StorageError) -> ActionError {
    match error {
        StorageError::Invalid(message) => ActionError::invalid(message),
        StorageError::NotFound(message) => ActionError::not_found(message),
        StorageError::Quota(message) => ActionError::quota(message),
        StorageError::Unavailable(message) => ActionError::unavailable(message),
    }
}

fn storage_repo(state: &AppState) -> StorageRepo {
    StorageRepo::new(&state.db)
}

/// 请求体解码：形状不符 / 空 body → 400 `invalid request body`（上游 `json.Decoder` 同判）。
fn decode<T: serde::de::DeserializeOwned>(body: &Bytes) -> ActionResult<T> {
    if body.is_empty() {
        return Err(ActionError::invalid("invalid request body"));
    }
    serde_json::from_slice(body).map_err(|_| ActionError::invalid("invalid request body"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_plugin_host::scope::{SCOPE_STORAGE_USER, SCOPE_STORAGE_WORKSPACE};
    use mc_repos::plugin::installation::InstallationRow;

    fn caller_with(scopes: &[&str], actor: policy::ActionActor) -> ActionCaller {
        // 只填 resolve_scope 读得到的三处；其余字段用不到（本用例不碰 DB）。
        let workspace_id = mc_core::Id::new();
        ActionCaller {
            installation: InstallationRow {
                id: uuid::Uuid::nil(),
                workspace_id: workspace_id.0,
                plugin_key: "p".into(),
                version: "1.0.0".into(),
                manifest: sqlx::types::Json(serde_json::json!({})),
                granted_scopes: sqlx::types::Json(serde_json::json!([])),
                config: sqlx::types::Json(serde_json::json!({})),
                enabled: true,
                installed_by: None,
                token_hash: None,
                token_rotated_at: None,
                mcp_approvals: sqlx::types::Json(serde_json::json!({})),
                package_version_id: uuid::Uuid::nil(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
            workspace_id,
            scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
            issue_scope: None,
            actor,
        }
    }

    #[test]
    fn unknown_scope_is_missing_scope_before_it_is_invalid() {
        // 没有 storage:workspace ⇒ 未知 scope 名也先撞 403 `missing_scope`（上游顺序）。
        let caller = caller_with(&[], policy::ActionActor::Plugin);
        let error = resolve_scope(&caller, "global").unwrap_err();
        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.code, "missing_scope");

        // 有 storage:workspace ⇒ 未知名字才是 400 `invalid_request`。
        let caller = caller_with(&[SCOPE_STORAGE_WORKSPACE], policy::ActionActor::Plugin);
        let error = resolve_scope(&caller, "global").unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_request");
    }

    #[test]
    fn user_scope_needs_a_member_and_never_falls_back_to_the_workspace() {
        let caller = caller_with(&[SCOPE_STORAGE_USER], policy::ActionActor::Plugin);
        let error = resolve_scope(&caller, storage::SCOPE_USER).unwrap_err();
        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.code, "member_required");

        let user_id = mc_core::Id::new();
        let caller = caller_with(&[SCOPE_STORAGE_USER], policy::ActionActor::Member(user_id));
        let (scope_type, scope_id) = resolve_scope(&caller, storage::SCOPE_USER).unwrap();
        assert_eq!(scope_type, storage::SCOPE_USER);
        assert_eq!(scope_id, user_id, "user scope 的 scope_id 是**用户** id");
    }

    #[test]
    fn workspace_scope_resolves_to_the_workspace_id() {
        let caller = caller_with(&[SCOPE_STORAGE_WORKSPACE], policy::ActionActor::Plugin);
        let (_, scope_id) = resolve_scope(&caller, storage::SCOPE_WORKSPACE).unwrap();
        assert_eq!(scope_id, caller.workspace_id);
    }

    #[test]
    fn storage_errors_map_to_the_upstream_status_codes() {
        assert_eq!(
            map_storage_error(StorageError::Invalid("x".into())).status,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            map_storage_error(StorageError::NotFound("x".into())).status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            map_storage_error(StorageError::Quota("x".into())).status,
            StatusCode::INSUFFICIENT_STORAGE
        );
        assert_eq!(
            map_storage_error(StorageError::Unavailable("x".into())).status,
            StatusCode::BAD_GATEWAY
        );
    }
}
