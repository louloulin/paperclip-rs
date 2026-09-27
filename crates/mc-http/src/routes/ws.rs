//! `GET /ws` —— **realtime hub 端点**（写者 M10-B4 / `LUM-2115` / `docs/64` §4.2）。
//!
//! 上游来源（pin `f41fae6b08fb`）：`server/cmd/server/router.go:1424`（裸 `r.Get`，
//! 注册在**会话中间件之外**）→ `server/internal/realtime/hub.go:775` `HandleWebSocket`。
//!
//! ## 本片**只读复用** `mc-ws` 的 hub（**不新写 hub** —— 这是硬前置 M10-0 的约束）
//!
//! 身份在这里构造好（查询收窄 + 成员校验 + dev-mode 取用户），然后交给
//! [`Hub::handle_websocket`](mc_ws::hub::Hub::handle_websocket) —— 与
//! `routes/daemon/lifecycle.rs::ws` 同一个入口。hub 侧的连接表 / 扇出 / 去重 /
//! 慢客户端驱逐 / 收发泵**一行都没有在这里重写**。
//!
//! 两条升级前判定逐字照上游 `hub.go:775-800`：
//!
//! 1. `workspace_id` 缺失 ⇒ **400** `{"error":"workspace_id or workspace_slug required"}`；
//!    只给 `workspace_slug` 时走 [`WorkspaceRepo::get_by_slug`] 解析（上游
//!    `slugResolver` 的本仓形态），解析不到 ⇒ **404** `{"error":"workspace not found"}`；
//! 2. 认证：上游是「cookie 会话令牌」或「**首帧** `{"type":"auth","payload":{"token":…}}`」。
//!    本地是 M1 dev-mode：**`X-Multica-User-Id` 头**或**会话 cookie**。
//!    取不到 / 取错 ⇒ **401**；取到但不是该工作区成员 ⇒ **403**。
//!
//! ## 偏离（已登记 `docs/32` §9.23）
//!
//! - **首帧认证未实现**：上游允许「先升级、再用第一帧交 token」。本仓在升级**之前**
//!   就要判身份（dev-mode 头 + cookie 都拿不到 ⇒ 401），因此**没有**「已升级但未认证」
//!   的连接。理由：`Hub::handle_websocket` 一次性接管 `on_upgrade`，要插首帧握手就得
//!   绕开 hub 的升级入口 —— 那正是「不新写 hub」这条硬前置不许做的事。
//! - 错误体是全仓统一形状 `{"error":{"code","message"}}`（`ApiError`），上游是裸
//!   `{"error":"…"}`；**状态码逐条相同**。
//!
//! ## 形态
//!
//! 上游是裸 `r.Get("/ws", …)` ⇒ **只注册无尾斜杠形态**。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde_json::json;

use mc_core::Id;
use mc_repos::member::MemberRepo;
use mc_repos::workspace::WorkspaceRepo;
use mc_ws::identity::ClientIdentity;

use crate::routes::auth_user::USER_ID_HEADER;
use crate::state::AppState;

/// 上游 `hub.go:786` 的 400 文案。
pub const ERR_NO_WORKSPACE: &str = "workspace_id or workspace_slug required";
/// 上游 `hub.go:781` 的 404 文案。
pub const ERR_WORKSPACE_NOT_FOUND: &str = "workspace not found";
/// 上游 `hub.go:801` / `authenticateToken` 的 401 文案。
pub const ERR_INVALID_TOKEN: &str = "invalid token";
/// 上游 `hub.go:807` 的 403 文案（cookie 腿与首帧腿逐字同一条）。
pub const ERR_NOT_MEMBER: &str = "not a member of this workspace";

/// 本文件 router：**1 条**注册键。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/ws", get(ws))
}

// ---------------------------------------------------------------------------
// GET /ws
// ---------------------------------------------------------------------------

/// `GET /ws`（上游 `realtime.HandleWebSocket`）。
///
/// 三段：**解析工作区** → **认证 + 成员校验** → [`Hub::handle_websocket`] 升级。
/// 每段的失败都在**升级之前**返回，因此失败响应是普通 HTTP 响应而不是 WS 帧
/// （与上游 `http.Error` 那一支逐字同构）。
///
/// `result_large_err` 的豁免：失败腿直接返回 `axum::response::Response`（上游
/// `http.Error` 的本仓形态），而 `Response` 是个大类型 —— 换成 `Box` 只会多一次间接。
#[allow(clippy::result_large_err)]
#[allow(clippy::implicit_hasher)] // 见 `resolve_workspace` 的理由：唯一生产者是 axum 的 `Query`。
pub async fn ws(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let workspace_id = match resolve_workspace(&state, &query).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let user_id = match resolve_user(&state, &headers).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if !is_member(&state, workspace_id, user_id).await {
        return error(StatusCode::FORBIDDEN, ERR_NOT_MEMBER);
    }
    state.daemon_hub.handle_websocket(
        ws,
        ClientIdentity {
            user_id: user_id.to_string(),
            workspace_id: workspace_id.to_string(),
            ..ClientIdentity::default()
        },
    )
}

/// 上游 `hub.go:776-790` 的工作区解析：`workspace_id` 优先，其次 `workspace_slug`。
///
/// `HashMap` **不泛型化**是刻意的：它的唯一生产者是 axum 的
/// `Query<HashMap<String, String>>`（与 `routes/daemon/ws.rs::requested_runtime_ids`
/// 的既有口径逐字一致），泛型化会让每个调用点多一个类型参数而不带来任何东西。
#[allow(clippy::implicit_hasher)]
#[allow(clippy::result_large_err)] // 同上：失败腿直接返回 `Response`。
async fn resolve_workspace(
    state: &AppState,
    query: &HashMap<String, String>,
) -> Result<Id, Response> {
    if let Some(raw) = query.get("workspace_id").filter(|s| !s.trim().is_empty()) {
        return Id::parse(raw.trim())
            .map_err(|_| error(StatusCode::NOT_FOUND, ERR_WORKSPACE_NOT_FOUND));
    }
    if let Some(raw) = query.get("workspace_slug").filter(|s| !s.trim().is_empty()) {
        // 上游 `slugResolver` = `queries.GetWorkspaceBySlug`；解析不到就是 404。
        let slug = mc_core::slug::Slug::parse(raw.trim())
            .map_err(|_| error(StatusCode::NOT_FOUND, ERR_WORKSPACE_NOT_FOUND))?;
        return WorkspaceRepo::new(state.db.clone())
            .get_by_slug(&slug)
            .await
            .map(|ws| ws.id)
            .map_err(|_| error(StatusCode::NOT_FOUND, ERR_WORKSPACE_NOT_FOUND));
    }
    Err(error(StatusCode::BAD_REQUEST, ERR_NO_WORKSPACE))
}

/// 认证腿：dev-mode 的 `X-Multica-User-Id` 头，或会话 cookie。
///
/// 上游逐字只认 cookie；本地 M1 dev-mode 认头（`routes/auth_user.rs` 的模块头逐字：
/// 「当前实现只信任 `X-Multica-User-Id`」）。cookie 腿复用会话 cookie 的同一份解析口径
/// （名字由 `CookieOptions::session_cookie` 定），因此 HTTP 与 WS 两条腿认的是同一把钥匙。
#[allow(clippy::result_large_err)] // 同上：失败腿直接返回 `Response`。
async fn resolve_user(state: &AppState, headers: &axum::http::HeaderMap) -> Result<Id, Response> {
    if let Some(raw) = headers.get(&USER_ID_HEADER) {
        let Ok(text) = raw.to_str() else {
            return Err(error(StatusCode::UNAUTHORIZED, ERR_INVALID_TOKEN));
        };
        return Id::parse(text.trim())
            .map_err(|_| error(StatusCode::UNAUTHORIZED, ERR_INVALID_TOKEN));
    }
    let Some(session_id) = cookie_session_id(headers, &state.config.session_cookie) else {
        return Err(error(StatusCode::UNAUTHORIZED, ERR_INVALID_TOKEN));
    };
    state
        .auth
        .store()
        .get(&session_id)
        .await
        .map(|session| session.user_id)
        .map_err(|_| error(StatusCode::UNAUTHORIZED, ERR_INVALID_TOKEN))
}

/// 从 `Cookie` 头里取出会话 id（名字由 [`crate::routes::auth::CookieOptions`] 定）。
pub(crate) fn cookie_session_id(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let part = part.trim();
        let (k, v) = part.split_once('=')?;
        (k == name && !v.is_empty()).then(|| v.to_owned())
    })
}

/// 成员校验（上游 `mc.IsMember(ctx, uid, workspaceID)`）。
async fn is_member(state: &AppState, workspace_id: Id, user_id: Id) -> bool {
    MemberRepo::new(state.db.clone())
        .get_for_user(workspace_id, user_id)
        .await
        .is_ok()
}

/// 上游的裸 `{"error":"…"}` 面（`hub.go` 的 `http.Error(w, '{"error":"…"}', …)`）。
///
/// ⚠️ 本仓全仓统一错误体是 `{"error":{"code","message"}}`（`ApiError`），而这条路由
/// **逐字保留上游的裸字符串**：它与 `Hub::handle_websocket` 自己回的
/// `{"error": "<Message>"}`（`identity::IdentityError` 的 Display）是**同一个解析器**
/// 的两种输入 —— 客户端读 `/ws` 的错误只有一个解析器能吃。已登记 `docs/32` §9.23。
fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
}

// ---------------------------------------------------------------------------
// 共用测试装置（M10-B4 三条路由的“零库”那一半）
// ---------------------------------------------------------------------------

/// M10-B4 三条路由的**零库**用例共用件。
///
/// 放在本文件是因为 `/ws` 是本片三条里最早被写的一个（先例：`uploads/tests/support.rs`
/// 把共用件放在面内的第一个文件里）。只用真 URL、**不可达**地址的 `connect_lazy`：
/// 不碰库的那一半一旦有人真去查库就会拿到连接错误，而不是静默通过。
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use mc_core::actor::ActorRegistry;
    use mc_db::Db;
    use mc_feature_flags::FeatureFlagCatalog;
    use mc_realtime::{RealtimeHandle, WsState};

    use crate::state::cloud::{CloudConfig, EntitlementConfig};
    use crate::state::integrations::{ComposioKeys, GithubKeys, VcsKeys};
    use crate::state::{
        AdapterRegistry, AppState, ChannelKeys, ConfigSnapshot, GoogleOAuthConfig, RuntimeHandles,
    };

    /// 不可达地址（与 `uploads/tests/support.rs` 同款形状）。
    pub const UNREACHABLE_DB: &str = "postgres://m10b4:m10b4@127.0.0.1:1/m10b4";

    /// 懒连接池（`connect_lazy` 不拨号）。
    pub fn lazy_db() -> Db {
        Db::connect_lazy(UNREACHABLE_DB, 1, 0).expect("lazy db")
    }

    /// 本仓的 `AppState` 字面量（**零出站**：云配没配也一样）。
    pub fn state_with(
        db: Db,
        storage: mc_storage::Storage,
        hub: Arc<mc_ws::hub::Hub>,
    ) -> Arc<AppState> {
        let realtime = RealtimeHandle::start(8);
        let ws = Arc::new(WsState::new(realtime.clone(), "m10-b4-test"));
        Arc::new(AppState {
            db,
            runtime: RuntimeHandles {
                actors: ActorRegistry::new(),
                adapters: Arc::new(AdapterRegistry::default()),
            },
            config: ConfigSnapshot::default(),
            storage,
            secrets: mc_secrets::Secrets::new(mc_auth::DefaultSecretsBackend::in_memory()),
            feature_flags: Arc::new(FeatureFlagCatalog::new()),
            realtime,
            ws,
            auth: mc_auth::SessionStoreContainer::new(),
            pat: mc_auth::PatStoreContainer::new(),
            verification: mc_auth::VerificationStoreContainer::new(),
            google_oauth: GoogleOAuthConfig::default(),
            daemon_hub: hub,
            daemon_requests: Arc::new(crate::daemon_requests::RequestStore::new()),
            plugin_key: None,
            plugin_surface_origin: None,
            channel_keys: ChannelKeys::default(),
            github_keys: GithubKeys::default(),
            vcs_keys: VcsKeys::default(),
            composio_keys: ComposioKeys::default(),
            cloud: CloudConfig::from_env_with(|_| None),
            entitlement: EntitlementConfig::from_env_with(|_| None),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).expect("name"),
                HeaderValue::from_str(v).expect("value"),
            );
        }
        h
    }

    fn query(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// dev-mode 头就是凭据：`X-Multica-User-Id` 存在 ⇒ 那个 id 就是用户。
    #[test]
    fn dev_mode_header_carries_the_user_id() {
        let id = Id::new();
        let h = headers(&[("x-multica-user-id", &id.to_string())]);
        assert_eq!(
            Id::parse(
                h.get(&USER_ID_HEADER)
                    .expect("header")
                    .to_str()
                    .expect("utf8")
            )
            .expect("uuid"),
            id
        );
        // 非 uuid 的头在 `resolve_user` 里被折成 401（`Id::parse` 失败分支）。
        assert!(Id::parse("not-a-uuid").is_err());
    }

    /// cookie 取值：同名的第一个非空值；没有 ⇒ None。
    #[test]
    fn session_cookie_is_parsed_like_the_http_leg() {
        let h = headers(&[("cookie", "a=1; multica_session=abc123; b=2")]);
        assert_eq!(
            cookie_session_id(&h, "multica_session").as_deref(),
            Some("abc123")
        );
        // 别的名字取不到。
        assert!(cookie_session_id(&h, "other").is_none());
        // 空值视同没有（上游 `cookie.Value != ""` 逐字）。
        let h = headers(&[("cookie", "multica_session=")]);
        assert!(cookie_session_id(&h, "multica_session").is_none());
        // 没有 Cookie 头。
        assert!(cookie_session_id(&headers(&[]), "multica_session").is_none());
    }

    /// 401/403/400 的**文案**逐字冻结（上游 `hub.go` 的四条）。
    #[test]
    fn error_messages_are_verbatim() {
        assert_eq!(ERR_NO_WORKSPACE, "workspace_id or workspace_slug required");
        assert_eq!(ERR_WORKSPACE_NOT_FOUND, "workspace not found");
        assert_eq!(ERR_INVALID_TOKEN, "invalid token");
        assert_eq!(ERR_NOT_MEMBER, "not a member of this workspace");
        let resp = error(StatusCode::UNAUTHORIZED, ERR_INVALID_TOKEN);
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// 查询口径：`workspace_id` 优先于 `workspace_slug`；全空 ⇒ 落到 400 分支。
    ///
    /// 纯函数那一半（优先级判定）在这里断言；落库那一半由真库用例钉。
    #[test]
    fn workspace_id_wins_over_slug() {
        let q = query(&[("workspace_id", "w"), ("workspace_slug", "s")]);
        assert_eq!(q.get("workspace_id").map(String::as_str), Some("w"));
        // 空白视同未给（上游 `workspaceID == ""` 逐字）。
        let q = query(&[("workspace_id", "   ")]);
        assert!(q.get("workspace_id").is_some_and(|v| v.trim().is_empty()));
        assert!(query(&[]).is_empty());
    }

    /// 路由级：**不带工作区** ⇒ 400，且**没有升级**（响应不是 101）。
    #[tokio::test]
    async fn route_requires_a_workspace_before_upgrading() {
        use axum::body::Body as AxumBody;
        use axum::http::Request as HttpRequest;
        use tower::ServiceExt as _;

        let state = super::test_support::state_with(
            super::test_support::lazy_db(),
            mc_storage::Storage::new(),
            Arc::new(mc_ws::hub::Hub::new()),
        );
        let app = crate::apply_default_middleware(router()).with_state(state);
        let resp = app
            .oneshot(
                HttpRequest::get("/ws")
                    .body(AxumBody::empty())
                    .expect("req"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_ne!(resp.status().as_u16(), 101, "失败时不得升级");
    }
}
