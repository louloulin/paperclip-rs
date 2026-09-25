//! `/api/pins*`（4 条，M2-A 尾 / LUM-1691）。
//!
//! 上游 `server/internal/handler/pin.go` + `server/cmd/server/router.go:2129-2132`：
//!
//! ```text
//! r.Route("/api/pins", func(r chi.Router) {
//!     r.Get("/",     h.ListPins)     // → /api/pins  与 /api/pins/   （chi Mount 两形态）
//!     r.Post("/",    h.CreatePin)    // → 同上两形态
//!     r.Put("/reorder",               h.ReorderPins)
//!     r.Delete("/{itemType}/{itemId}", h.DeletePin)
//! })
//! ```
//!
//! **尾斜杠形态**：fixture 里这两条记的是 `/api/pins/`（带斜杠）= 挂载式子路由根 ⇒
//! `/api/pins` 与 `/api/pins/` 必须**都注册**（`slash_alias_audit.py` 的 `MISSING_ALIAS`
//! 是硬失败，且本波 `slash-alias-allowlist.tsv` 是 0 数据行、**没有豁免退路**）。
//! `/reorder` 与 `/{itemType}/{itemId}` 是 plain 注册 ⇒ **只注册无尾斜杠形态**
//! （多注册一条 = `EXTRA_ALIAS` 警告 + 与上游不符）。
//!
//! 语义要点（逐条照上游，`docs/63-M2A-TAIL-ISSUE-VIEW-PIN.md` 有完整偏差表）：
//! - **先建后列**：`GET` 按 `position ASC, created_at ASC`；`POST` 的位置是
//!   `COALESCE(MAX(position),0)+1`（追加到末尾）。
//! - **`POST` 重复钉同一项 → 409** `item already pinned`（唯一约束 23505）。**这不是幂等接口**：
//!   上游确实报错；幂等只有 `DELETE`（未钉过再删仍 204）。
//! - **被钉对象必须在本 workspace 存在**，否则 404（`issue` / `project` / `view` 三条分支）；
//!   `view` 分支用的是**视图的读权限**（自己的或 workspace 共享的），别人的私有视图
//!   一并 404 —— 一次 pin 不能确认它的存在。
//! - **`item_type` 三值校验**：`issue` / `project` / `view`，其余 400。
//! - **老客户端的兼容闸门**：默认列表**不含** `item_type='view'` 的行，除非 `?include=view`
//!   （子串判定，照上游 `strings.Contains`）。不这么做的话，老客户端会把 view pin 当项目
//!   pin 拉详情 → 404 → 永久自动取消钉住。
//! - **`DELETE` 不校验 `itemType`**：只按四元组删，删不到也回 204（上游同款）。
//! - **`reorder` 的 `position` 缺省 = 0**（Go 零值），且只改 `position`，不动 `created_at`。
//!
//! **已知偏离（登记）**：上游 `CreatePin` / `DeletePin` / `ReorderPins` 会 `h.publish`
//! 一条 realtime 事件（`pin.created` / `pin.deleted` / `pin.reordered`）。M2 面（issue /
//! comment / inbox / label / property）**整波都没有 realtime 发布通道**，本片不单开一条边
//! ⇒ 多端同步靠轮询/重取；接线点与事件名见 `docs/63` §6。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, put};
use axum::{Json, Router};
use mc_errors::Error;
use mc_repos::pin::{
    is_valid_item_type, visible_without_view_capability, PinRepo, ITEM_TYPE_ERROR,
};
use mc_repos::project::ProjectRepo;
use mc_repos::RepoError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::{not_found, require_workspace_member};
use crate::routes::issue_views::issue_view_repo;
use crate::routes::issues::{
    issue_repo, parse_target_id, resolve_workspace, validation, WorkspaceQuery,
};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 请求 / 响应类型
// ---------------------------------------------------------------------------

/// `GET /api/pins` 的查询：workspace 选择器 + `?include=`（能力选择位）。
#[derive(Debug, Default, Deserialize)]
pub struct PinsQuery {
    /// 上游 `strings.Contains(r.URL.Query().Get("include"), "view")` ⇒ 子串判定。
    #[serde(default)]
    pub include: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_slug: Option<String>,
}

impl PinsQuery {
    fn selector(&self) -> WorkspaceQuery {
        WorkspaceQuery {
            workspace_id: self.workspace_id.clone(),
            workspace_slug: self.workspace_slug.clone(),
        }
    }
}

/// `POST /api/pins`（上游 `CreatePinRequest`）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CreatePinRequest {
    pub item_type: String,
    pub item_id: String,
}

/// `PUT /api/pins/reorder`（上游 `ReorderPinsRequest`）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ReorderPinsRequest {
    pub items: Vec<ReorderItem>,
}

/// 单个重排项（上游 `ReorderItem`：`position` 缺省 = Go 零值 `0`）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ReorderItem {
    pub id: String,
    pub position: f64,
}

/// 钉住项响应（上游 `PinnedItemResponse`）。
///
/// 体量刻意只带钉住元数据：`title` / `status` / `identifier` / `icon` **故意不在**响应里，
/// 客户端从自己的 issue/project 查询缓存里取 —— 这样 `issue:updated` 事件能自然流进侧栏，
/// 不需要跨实体失效 `pinKeys`（上游结构体上方那段注释）。
#[derive(Debug, Clone, Serialize)]
pub struct PinnedItemResponse {
    pub id: String,
    pub workspace_id: String,
    pub user_id: String,
    pub item_type: String,
    pub item_id: String,
    pub position: f64,
    pub created_at: String,
}

impl PinnedItemResponse {
    fn from_row(row: &mc_repos::pin::PinnedItemRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            user_id: row.user_id.to_string(),
            item_type: row.item_type.clone(),
            item_id: row.item_id.to_string(),
            position: row.position,
            created_at: row.created_at.to_rfc3339(),
        }
    }
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

/// `/api/pins*` 的注册表（4 条上游键 / 6 个注册点：`/api/pins` 的 GET+POST 各带尾斜杠别名）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // 上游 `Route("/api/pins") + Get/Post("/")` ⇒ 两个形态都要（fixture 记的是带斜杠那条）。
        .route("/api/pins", get(list_pins).post(create_pin))
        .route("/api/pins/", get(list_pins).post(create_pin))
        // 以下两条是 plain 注册 ⇒ 只此一形态。
        .route("/api/pins/reorder", put(reorder_pins))
        .route("/api/pins/:item_type/:item_id", delete(delete_pin))
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `GET /api/pins`（上游 `ListPins`）。
async fn list_pins(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<PinsQuery>,
    user: AuthUser,
) -> ApiResult<Json<JsonValue>> {
    let workspace_id = resolve_workspace(&state, &headers, &query.selector()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let include_views = query.include.as_deref().is_some_and(|v| v.contains("view"));

    let rows = pin_repo(&state)
        .list(workspace_id, user.id())
        .await
        .map_err(pin_err)?;
    let pins: Vec<PinnedItemResponse> = rows
        .iter()
        .filter(|row| visible_without_view_capability(&row.item_type, include_views))
        .map(PinnedItemResponse::from_row)
        .collect();
    // 上游 `make([]PinnedItemResponse, 0, …)` ⇒ 空列表编成 `[]` 而不是 `null`。
    Ok(Json(json!(pins)))
}

/// `POST /api/pins`（上游 `CreatePin`；成功 201）。
async fn create_pin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let req: CreatePinRequest = parse_body(&body)?;
    // 校验顺序照上游：item_type → item_id 非空 → item_id 是 uuid → workspace → 对象归属。
    if !is_valid_item_type(&req.item_type) {
        return Err(validation(ITEM_TYPE_ERROR).into());
    }
    if req.item_id.is_empty() {
        return Err(validation("item_id is required").into());
    }
    let item_id = parse_target_id("item_id", &req.item_id)?;

    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    ensure_item_is_pinnable(&state, workspace_id, &req.item_type, item_id, user.id()).await?;

    let repo = pin_repo(&state);
    let position = repo
        .max_position(workspace_id, user.id())
        .await
        .map_err(pin_err)?
        + 1.0;
    let row = repo
        .create(workspace_id, user.id(), &req.item_type, item_id, position)
        .await
        .map_err(pin_err)?;
    Ok((
        StatusCode::CREATED,
        Json(PinnedItemResponse::from_row(&row)),
    )
        .into_response())
}

/// `DELETE /api/pins/:itemType/:itemId`（上游 `DeletePin`；成功 204，且**幂等**）。
async fn delete_pin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_type, raw_item_id)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    // 上游只解析 `itemId`（400 = `item id must be a uuid`）；`itemType` 不校验。
    let item_id = parse_target_id("item id", &raw_item_id)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    pin_repo(&state)
        .delete(workspace_id, user.id(), &item_type, item_id)
        .await
        .map_err(pin_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/pins/reorder`（上游 `ReorderPins`；成功 204）。
async fn reorder_pins(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let req: ReorderPinsRequest = parse_body(&body)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    // 先把全部 id 解析完再落写：上游是同款「循环里先 parseUUIDOrBadRequest 再写」，但它的
    // 循环边解析边写 ⇒ 中途遇到非法 id 时前面几行已经落了库（半截排序）。本片把解析收在前，
    // 让 400 不产生任何写入 —— 这是本片唯一有意收紧上游的地方（docs/63 偏差表第 4 条）。
    let mut parsed = Vec::with_capacity(req.items.len());
    for item in &req.items {
        parsed.push((parse_target_id("items[].id", &item.id)?, item.position));
    }
    let repo = pin_repo(&state);
    // 逐项写：`UpdatePinnedItemPosition` 只认行数不认错误，0 行不是失败（照上游）。
    for (pin_id, position) in parsed {
        repo.set_position(workspace_id, user.id(), pin_id, position)
            .await
            .map_err(pin_err)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 内部 helper
// ---------------------------------------------------------------------------

fn pin_repo(state: &AppState) -> PinRepo {
    PinRepo::new(state.db.clone())
}

fn pin_err(err: RepoError) -> Error {
    match err {
        RepoError::NotFound => not_found("pin"),
        RepoError::Conflict => Error::Conflict {
            // 唯一索引 `pinned_item_workspace_id_user_id_item_type_item_id_key` 只在这
            // 一种情况下触发 ⇒ 409 文案可以直接写死（上游 `item already pinned`）。
            message: "item already pinned".into(),
        },
        RepoError::Db(message) => Error::Database(message),
    }
}

/// 被钉对象必须在本 workspace 里可得，否则 404（上游 `CreatePin` 的 `switch req.ItemType`）。
///
/// `view` 分支复用视图的**读权限**（`canReadIssueView` = 所有者或 workspace 共享）：
/// 别人的私有视图 404 —— 一次 pin 不能确认它的存在。
async fn ensure_item_is_pinnable(
    state: &AppState,
    workspace_id: mc_core::Id,
    item_type: &str,
    item_id: mc_core::Id,
    user_id: mc_core::Id,
) -> Result<(), Error> {
    match item_type {
        "issue" => match issue_repo(state).get(workspace_id, item_id).await {
            Ok(_) => Ok(()),
            Err(RepoError::Db(message)) => Err(Error::Database(message)),
            Err(_) => Err(not_found("issue")),
        },
        "project" => match ProjectRepo::new(state.db.clone())
            .get_in_workspace(item_id.0, workspace_id)
            .await
        {
            Ok(Some(_)) => Ok(()),
            Err(RepoError::Db(message)) => Err(Error::Database(message)),
            // `Ok(None)`（本 workspace 没有该项目）与其余仓储错误都按 404 处理。
            _ => Err(not_found("project")),
        },
        "view" => match issue_view_repo(state).get(workspace_id, item_id).await {
            Ok(view) if view.is_readable_by(user_id) => Ok(()),
            Err(RepoError::Db(message)) => Err(Error::Database(message)),
            // 不存在、别人的私有视图、其余仓储错误都按 404 处理（存在性不泄漏）。
            _ => Err(not_found("view")),
        },
        // 上层已用 `is_valid_item_type` 挡掉；这里是防御性分支（不 panic）。
        _ => Err(validation(ITEM_TYPE_ERROR)),
    }
}

/// body 解码（上游 `json.Decoder` 失败 ⇒ 400 `invalid request body`）。
fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Error> {
    serde_json::from_slice::<T>(body).map_err(|_| validation("invalid request body"))
}

// ---------------------------------------------------------------------------
// 纯单测（无需 DB）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_builds_without_panicking() {
        // axum 0.7 同 path+method 重复注册会在 build 期 panic；本断言保证 4 条上游键
        // + 2 个尾斜杠别名（`/api/pins` 的 GET/POST）互不冲突。
        let _ = router();
    }

    #[test]
    fn body_decode_failures_are_400s() {
        assert!(parse_body::<CreatePinRequest>(&Bytes::from_static(b"not json")).is_err());
        assert!(parse_body::<CreatePinRequest>(&Bytes::from_static(b"{}")).is_ok());
    }

    #[test]
    #[allow(clippy::float_cmp)] // 断言的就是「缺省恰好是 Go 的零值 0」
    fn reorder_item_defaults_position_to_zero_like_go() {
        // Go 的 `ReorderItem` 零值 `Position` 是 0 ⇒ 只给 id 的请求等价于 position=0。
        let req: ReorderPinsRequest = serde_json::from_str(r#"{"items":[{"id":"x"}]}"#).unwrap();
        assert_eq!(req.items.len(), 1);
        assert_eq!(req.items[0].position, 0.0);
        // 缺 `items` 也是合法请求（空重排，回 204）。
        let empty: ReorderPinsRequest = serde_json::from_str("{}").unwrap();
        assert!(empty.items.is_empty());
    }
}
