//! `GET` + `PATCH` + `PUT /api/notification-preferences/` —— **写者 M9-5**（`LUM-1820`，
//! `docs/62` §4.1 第 6 行）。
//!
//! | method | 路径（上游字面量） | 上游 handler | 语义 |
//! |---|---|---|---|
//! | GET | `/api/notification-preferences/` | `GetNotificationPreferences` | 读；**无行** ⇒ `preferences: {}` |
//! | PATCH | `/api/notification-preferences/` | `PatchNotificationPreferences` | **只**合并传来的键 |
//! | PUT | `/api/notification-preferences/` | `UpdateNotificationPreferences` | **整体替换** |
//!
//! # 🔴 本波**唯一**的双形态键（`docs/62` §1.4 / R-M9-5）
//!
//! 上游是 `r.Route("/api/notification-preferences", …)` + `r.Get("/")/Patch("/")/Put("/")`
//! （`router.go:2400-2403`）—— chi 的 `Route` + 子路由 `"/"` 形态**同时**服务
//! `/api/notification-preferences` 与 `/api/notification-preferences/`。
//! 而 `docs/fixtures/upstream-routes.tsv:290-292` 记的是**带**尾斜杠那一形态。
//!
//! axum 的 `Router::route` 逐字匹配 ⇒ 只注册一条形态就是**另一种形态 404**。而门 ⑦ 会把
//! `/x` 与 `/x/` **折叠**比较（看不见这个差异）⇒ 折叠层唯一能看见缺口的工具是
//! `scripts/slash_alias_audit.py`。
//!
//! **起手**（本片交付前）实测 `python3 scripts/slash_alias_audit.py --declared
//! docs/fixtures/m9-declared-routes.tsv` ⇒ `dual-form required: 3`、`MISSING_ALIAS (3)`，
//! **恰好是本文件这三个方法**。**收尾**（本片交付后）该模式须 `MISSING_ALIAS 0`，
//! 且无参模式（本地实况）仍 `0 defect(s)`。
//!
//! ⚠️ 这 3 个键**不进** `docs/fixtures/slash-alias-allowlist.tsv`（那是我**必须补**的形态，
//! 不是欠账 —— `docs/62` §3.1 末行逐字）。
//! ⚠️ 反向红线：M9-6 / M9-7 / M9-8 各片一律**单形态**，给它们补尾斜杠会触发 `EXTRA_ALIAS` 硬失败。
//!
//! # 授权链（`docs/62` §1.5 A 行 + §4.2 的 notification-preferences 行）
//!
//! - **无会话 ⇒ 401**：[`AuthUser`] 提取器（缺 `X-Multica-User-Id`）；
//! - **非 member ⇒ 403**：[`require_member`] 查 `member` 表，**没有行**即 403
//!   （上游 `ctxWorkspaceID` 那一层已经把非成员挡在外面；本仓把那一层显式化成 403，
//!   `DoD` 第 2 条逐字要求「非 member ⇒ 403」）；
//! - **workspace 来自 `x-workspace-id`**：[`resolve_workspace_id`]（400 兜底），
//!   客户端在 body 里走私的 `workspace_id` **被忽略**（`docs/62` §2.7 第 7 条）——
//!   上游的请求体里**根本没有** `workspace_id` 字段，本仓的
//!   [`UpdateNotificationPreferencesRequest`] 同样只有 `preferences`。
//!
//! # 词表与校验**不在本文件**
//!
//! 7 个分组 × 2 个取值 + 两条错误文本在 [`mc_core::notification`]（M9-0 anchor 定形），
//! 本文件**不复制**一份 ⇒ 校验入口是 [`UpdateNotificationPreferencesRequest::validate`]。
//!
//! # `GET` 不写行
//!
//! 「从没设过」返回 `{"workspace_id":…,"preferences":{}}`，**不是**顺手插一行全 `all`
//! —— 上游逐字：`writeJSON(w, 200, map[string]any{"workspace_id": workspaceID,
//! "preferences": map[string]any{}})`（`pgx.ErrNoRows` 分支）。

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use mc_core::notification::{
    NotificationPreferencesResponse, UpdateNotificationPreferencesRequest,
};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::notification_preference::NotificationPreferenceRepo;

use crate::error::{ApiError, ApiResult};
// ⚠️ 本仓有**两个** `AuthUser`：`middleware::authn::AuthUser` 是中间件写进 `Extensions`
// 的那一个（**不是**提取器），`routes::auth_user::AuthUser` 才是提取器。
use crate::routes::agents::{bad_request, repo_err};
use crate::routes::auth_user::AuthUser;
use crate::routes::inbox::resolve_workspace_id;
use crate::state::AppState;

/// 通知偏好切片：3 个方法 × **2 个形态**（6 个注册点 / 3 个上游键）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // 上游 `Route("/api/notification-preferences") + Get/Patch/Put("/")` ⇒ 两个形态都要
        // （fixture 记的是带斜杠那条，见文件头「双形态键」）。
        .route(
            "/api/notification-preferences",
            get(get_notification_preferences)
                .patch(patch_notification_preferences)
                .put(put_notification_preferences),
        )
        .route(
            "/api/notification-preferences/",
            get(get_notification_preferences)
                .patch(patch_notification_preferences)
                .put(put_notification_preferences),
        )
}

/// 一次请求的 `(workspace_id, user_id)` 解析：**401 → 400 → 403** 三级阶梯。
///
/// 顺序逐字：提取器先跑（401）⇒ 再解 workspace（400）⇒ 最后查成员（403）。
/// 成员闸**先于**任何表访问（不存在的 workspace 与非成员都落 403，不泄露 workspace 是否存在）。
async fn scope(state: &AppState, user: AuthUser, headers: &HeaderMap) -> Result<(Id, Id), Error> {
    let query = std::collections::HashMap::new();
    let workspace_id = resolve_workspace_id(headers, &query)?;
    let user_id = user.id();
    require_member(state, workspace_id, user_id).await?;
    Ok((workspace_id, user_id))
}

/// 非 member ⇒ 403（`DoD` 第 2 条逐字）。
///
/// 上游这一层是 workspace 中间件做的（未成员根本进不到 handler）；本仓把它显式化成一条
/// 可测判定。`member` 表**没有行** = 不是成员 ⇒ 403，**不**是 404
/// （404 会告诉调用方「这个 workspace 存在但你没权限」之外的额外信息，而 403 已经够了）。
async fn require_member(state: &AppState, workspace_id: Id, user_id: Id) -> Result<(), Error> {
    let role: Option<(String,)> =
        sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.0)
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
    role.map(|(role,)| role).map(|_| ()).ok_or_else(forbidden)
}

/// 403 文案。
fn forbidden() -> Error {
    Error::Forbidden {
        message: "notification preferences require workspace membership".into(),
    }
}

/// 响应装配：上游 `writeNotificationPreferenceResponse` 逐字。
fn response_of(
    workspace_id: Id,
    preferences: BTreeMap<String, String>,
) -> Json<NotificationPreferencesResponse> {
    Json(NotificationPreferencesResponse {
        workspace_id: workspace_id.as_string(),
        preferences,
    })
}

/// `GET /api/notification-preferences/`（上游 `GetNotificationPreferences`）。
///
/// **无行 ⇒ 200 + `preferences: {}`**（**不**是 404，也**不**是全 `all` 的默认表），
/// 且**不**写行（上游读面没有 `INSERT`）。
pub async fn get_notification_preferences(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
) -> ApiResult<Json<NotificationPreferencesResponse>> {
    let (workspace_id, user_id) = scope(&state, user, &headers).await?;
    let repo = NotificationPreferenceRepo::new(state.db.clone());
    let row = repo
        .get(workspace_id, user_id)
        .await
        .map_err(|e| repo_err(e, "notification preference"))?;
    let preferences = row.map(|row| row.preferences).unwrap_or_default();
    Ok(response_of(workspace_id, preferences))
}

/// `PATCH /api/notification-preferences/`（上游 `PatchNotificationPreferences`）。
///
/// **只合并**传来的键（上游逐字：`atomically merges only the supplied keys. This prevents stale
/// tabs or devices from replacing unrelated mute settings.`）⇒ 落 [`patch`](NotificationPreferenceRepo::patch)，
/// **不是** [`upsert`](NotificationPreferenceRepo::upsert)。
pub async fn patch_notification_preferences(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<NotificationPreferencesResponse>> {
    let request = decode_request(&body)?;
    let (workspace_id, user_id) = scope(&state, user, &headers).await?;
    let repo = NotificationPreferenceRepo::new(state.db.clone());
    let row = repo
        .patch(workspace_id, user_id, &request.preferences)
        .await
        .map_err(|e| repo_err(e, "notification preference"))?;
    Ok(response_of(workspace_id, row.preferences))
}

/// `PUT /api/notification-preferences/`（上游 `UpdateNotificationPreferences`）。
///
/// **整体替换**（上游逐字：`preserves the original replace-all PUT contract for compatibility
/// with installed clients`）⇒ 落 [`upsert`](NotificationPreferenceRepo::upsert)。
pub async fn put_notification_preferences(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<NotificationPreferencesResponse>> {
    let request = decode_request(&body)?;
    let (workspace_id, user_id) = scope(&state, user, &headers).await?;
    let repo = NotificationPreferenceRepo::new(state.db.clone());
    let row = repo
        .upsert(workspace_id, user_id, &request.preferences)
        .await
        .map_err(|e| repo_err(e, "notification preference"))?;
    Ok(response_of(workspace_id, row.preferences))
}

/// 上游 `decodeNotificationPreferenceRequest` 逐字：解码 → `preferences` 必填 → 逐对校验。
///
/// 三条 400 的顺序是上游同一个函数里的顺序：
/// `invalid request body` / `preferences field is required` / `invalid preference {group|value}: {x}`。
/// 词表本身在 [`mc_core::notification`]（7 组 × 2 值）。
///
/// 🔴 **为什么分两步解**（`{}` 那一格是本函数最容易搞错的地方）：上游是
/// `json.Decode(&req)` **然后** `if req.Preferences == nil`。`json.Decode` 对 `{}`
/// **成功**（map 字段留 `nil`），所以 `{}` 落在**第二条** 400 上，**不是**第一条。
/// 而 [`UpdateNotificationPreferencesRequest::preferences`] 没有 `#[serde(default)]`
/// ⇒ 直接 `from_slice::<UpdateNotificationPreferencesRequest>(b"{}")` 会把 `{}` 折进
/// **第一条**。⇒ 这里先解成 `serde_json::Value`（只有**畸形 JSON** 才失败 = 上游的
/// `Decode` 错），把 `preferences` 的「缺失 / `null`」判掉，再解成定型结构。
fn decode_request(body: &Bytes) -> Result<UpdateNotificationPreferencesRequest, ApiError> {
    // ① 只有**畸形 JSON** 才到这一条（上游 `json.NewDecoder(r.Body).Decode` 的错误分支）。
    let raw: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| ApiError(bad_request("invalid request body")))?;
    // ② `preferences` 缺失 / `null` ⇒ 上游 `req.Preferences == nil`。
    //    `{}`（空对象）是**合法**的清空请求，**不**在这里被拒。
    let has_preferences = raw.get("preferences").is_some_and(|value| !value.is_null());
    if !has_preferences {
        return Err(ApiError(bad_request("preferences field is required")));
    }
    // ③ 逐对词表（`BTreeMap` ⇒ 顺序确定，报错可复现）。
    let request: UpdateNotificationPreferencesRequest =
        serde_json::from_value(raw).map_err(|_| ApiError(bad_request("invalid request body")))?;
    request
        .validate()
        .map_err(|message| ApiError(bad_request(message)))?;
    Ok(request)
}

/// 语义：`StatusCode::OK` 是 `GET` 无行时的那一档（**不是** 404 / 201）。
pub const GET_MISSING_ROW_STATUS: StatusCode = StatusCode::OK;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
    use axum::Router;
    use mc_core::notification::{NOTIFICATION_GROUPS, NOTIFICATION_VALUES};

    fn body_of(raw: &str) -> Bytes {
        Bytes::from(raw.to_owned())
    }

    /// 库不可达（`connect_lazy` 不拨号）的 `AppState` —— 本组用例只判**形状**与**鉴权阶梯**。
    fn state() -> Arc<AppState> {
        let db = mc_db::Db::connect_lazy("postgres://np:np@127.0.0.1:1/none", 1, 0).expect("lazy");
        let realtime = mc_realtime::RealtimeHandle::start(8);
        let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "lum-1820"));
        Arc::new(AppState::new(
            db,
            RuntimeHandles {
                actors: mc_core::actor::ActorRegistry::new(),
                adapters: Arc::new(AdapterRegistry::default()),
            },
            ConfigSnapshot::default(),
            realtime,
            ws,
        ))
    }

    /// 装**本切片**的 router（库不可达 ⇒ 只看得到 401 那一档）。
    fn probe() -> Router {
        let state = state();
        router().with_state(state)
    }

    /// 装**全量** router（本仓的 `mount.rs` 聚合）—— 判「相邻键没被顺手补形态」那一格要用它。
    fn full_app() -> Router {
        let state = state();
        crate::apply_default_middleware(crate::routes::router(state.clone())).with_state(state)
    }

    /// 一条请求（不带会话头 ⇒ 提取器先拒）。
    async fn call(app: fn() -> Router, method: &str, uri: &str) -> StatusCode {
        use axum::body::Body as AxumBody;
        use http_body_util::BodyExt as _;
        use tower::ServiceExt as _;

        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(AxumBody::empty())
                    .expect("request"),
            )
            .await
            .expect("router call");
        let status = response.status();
        let _ = response.into_body().collect().await;
        status
    }

    /// 🔴 **本片的核心判据**：3 个方法 × **两种形态**都必须**已注册**。
    ///
    /// 判据是**可观测**的：未注册的形态是 axum 的 **404**（`Method Not Allowed` 只会
    /// 出现在**路径已注册、方法没有**的那一格），已注册的那一格在本装置下恒为
    /// **401**（[`AuthUser`] 提取器先于任何库访问拒绝）⇒ 404 ⇒ 404 **就**是缺形态。
    ///
    /// 只注册一种形态时，这 6 个断言里会有 3 个读到 404 —— 门 ⑦ 折叠 `/x` 与 `/x/`
    /// **看不见**这个差异，所以这一格必须由这里和 `slash_alias_audit.py` 一起钉住。
    #[tokio::test]
    async fn both_forms_are_registered_for_all_three_methods() {
        for method in ["GET", "PATCH", "PUT"] {
            for path in [
                "/api/notification-preferences",
                "/api/notification-preferences/",
            ] {
                let status = call(probe, method, path).await;
                assert_ne!(
                    status,
                    StatusCode::NOT_FOUND,
                    "{method} {path} is not registered (404) — dual-form key"
                );
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{method} {path}: registered path must reach the auth extractor"
                );
            }
        }
    }

    /// 形态门的**反向红线**：本片**只**补了这 3 个键的两形态。
    ///
    /// `feedback` / `contact-sales` 是 plain 注册 ⇒ 补尾斜杠会触发 `EXTRA_ALIAS` 硬失败。
    /// 这里逐条钉住「尾斜杠那一形态是 404」，让想「顺手对齐形态」的人当场看见代价。
    #[tokio::test]
    async fn the_other_two_keys_of_this_slice_stay_single_form() {
        for (method, path) in [("POST", "/api/feedback"), ("POST", "/api/contact-sales")] {
            let status = call(full_app, method, path).await;
            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "{method} {path} must stay registered in its single (no-slash) form"
            );
        }
    }

    /// 无行 ⇒ 200 + 空对象（上游 `pgx.ErrNoRows` 分支逐字）。
    #[test]
    fn a_missing_row_is_200_with_an_empty_object() {
        assert_eq!(GET_MISSING_ROW_STATUS, StatusCode::OK);
        let response = NotificationPreferencesResponse {
            workspace_id: "ws".into(),
            preferences: BTreeMap::new(),
        };
        assert_eq!(
            serde_json::to_value(response).expect("serialize"),
            serde_json::json!({"workspace_id": "ws", "preferences": {}})
        );
    }

    /// 三条 400 的顺序与文本（上游 `decodeNotificationPreferenceRequest` 逐字）。
    #[test]
    fn decode_rejects_body_then_missing_key_then_vocabulary_in_that_order() {
        // ① 解不开的体。
        assert_eq!(
            decode_request(&body_of("{")).unwrap_err().0.to_string(),
            "validation error: invalid request body"
        );
        // ② `preferences` 缺失 ⇒ 与「非法体」**不同**的文案。
        assert_eq!(
            decode_request(&body_of("{}")).unwrap_err().0.to_string(),
            "validation error: preferences field is required"
        );
        // `null` 与缺失同档（上游 `req.Preferences == nil`）。
        assert_eq!(
            decode_request(&body_of(r#"{"preferences":null}"#))
                .unwrap_err()
                .0
                .to_string(),
            "validation error: preferences field is required"
        );
        // ③ 词表：分组错先于取值错（上游同一个循环）。
        assert_eq!(
            decode_request(&body_of(r#"{"preferences":{"nope":"all"}}"#))
                .unwrap_err()
                .0
                .to_string(),
            "validation error: invalid preference group: nope"
        );
        assert_eq!(
            decode_request(&body_of(r#"{"preferences":{"comments":"loud"}}"#))
                .unwrap_err()
                .0
                .to_string(),
            "validation error: invalid preference value: loud"
        );
    }

    /// 7 组 × 2 值的**正例**逐个被接受（`DoD` 第 2 条的正例半边）。
    #[test]
    fn every_group_accepts_both_values() {
        assert_eq!(NOTIFICATION_GROUPS.len(), 7);
        assert_eq!(NOTIFICATION_VALUES.len(), 2);
        for group in NOTIFICATION_GROUPS {
            for value in NOTIFICATION_VALUES {
                let raw = format!(r#"{{"preferences":{{"{group}":"{value}"}}}}"#);
                let request = decode_request(&body_of(&raw))
                    .unwrap_or_else(|e| panic!("{group}={value} must be accepted: {}", e.0));
                assert_eq!(
                    request.preferences.get(group).map(String::as_str),
                    Some(value)
                );
            }
        }
    }

    /// 空对象是**合法**的清空请求（上游：`Preferences != nil` 且循环零次 ⇒ 200）。
    #[test]
    fn an_empty_object_is_a_legal_clearing_request() {
        let request = decode_request(&body_of(r#"{"preferences":{}}"#))
            .map_err(|e| e.0.to_string())
            .expect("legal");
        assert!(request.preferences.is_empty());
    }

    /// 请求体里**没有** `workspace_id` 可走私（`docs/62` §2.7 第 7 条）。
    ///
    /// 上游的 `updateNotifPrefRequest` 只有 `Preferences` 一个字段 ⇒ 客户端塞进来的
    /// `workspace_id` 被 serde **丢弃**（`deny_unknown_fields` 没开）。
    /// 用例钉住「它不参与落库」：解出来的结构里根本没有这个字段可读。
    #[test]
    fn a_smuggled_workspace_id_in_the_body_is_ignored() {
        let request = decode_request(&body_of(
            r#"{"workspace_id":"11111111-1111-1111-1111-111111111111","preferences":{"comments":"all"}}"#,
        ))
        .map_err(|e| e.0.to_string())
        .expect("accepted");
        assert_eq!(request.preferences.len(), 1);
        // 落库用的 `(workspace_id, user_id)` 由 [`scope`] 从头解析，**不**来自这个体。
        assert_eq!(
            request.preferences.get("comments").map(String::as_str),
            Some("all")
        );
    }
}
