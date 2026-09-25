//! composio 目录与连接面：**3 条**路由（`router.go:1867/1868/1869`）—— 写者 **M8-6**（`LUM-1803`）。
//!
//! | 注册键 | 方法 | 授权层 | 未配置 | 上游 |
//! | --- | :-: | --- | --- | --- |
//! | `/api/integrations/composio/toolkits` | GET | Auth 组内（匿名 ⇒ **401**） | **403** `composio_not_configured` | `ListComposioToolkits`（`integrations_composio.go:174`） |
//! | `/api/integrations/composio/connections` | GET | 同上 | 同上 | `ListComposioConnections`（`:135`） |
//! | `/api/integrations/composio/connections/:id` | DELETE | 同上 | 同上 | `DeleteComposioConnection`（`:203`） |
//!
//! # 鉴权顺序（401 → 403 → 400 → 404 → 502）
//!
//! 上游把 3 条路由挂在 Auth 组**内** ⇒ 匿名请求到不了 handler。本仓在 handler 签名里用
//! [`AuthUser`] 提取器复刻 middleware 的那一步（M8-1 / M8-2 同款，见 `connect.rs` 的文件头）。
//!
//! # 两条「不是 401/403」的分支
//!
//! - `DELETE` 的连接不属于调用者 / 不存在 ⇒ **404** `not_found`（两者**同判**：上游注释逐字
//!   without leaking existence across users）。`parse_uuid` 失败在前 ⇒ **400**；
//! - `toolkits` 的 resolve 失败（auth-config 或目录上游不可达）⇒ **502** `upstream_error`
//!   （上游注释逐字：a resolver error is NOT masked into an everything-not-connectable
//!   catalog —— 那会渲染成一个误导性的「没有 App 已配置」空状态）。
//!
//! # write-only / redaction
//!
//! 三个响应体里**没有** `connected_account_id` / `auth_config_id`（上游 `Service.Connection`
//! 的注释逐字：they are server-internal handles, not API surface），也**没有** bearer
//! （`x-api-key` 只活在 `mc-composio` 的会话 URL 路径上）。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use mc_composio::service::{ComposioError, ConnectionView, ToolkitView};
use serde::Serialize;

use crate::routes::agents::{not_found, parse_uuid};
use crate::routes::auth_user::AuthUser;
use crate::routes::composio::connect::{
    error_response, not_configured, service_from, upstream_error,
};
use crate::state::AppState;

/// 本文件的路由切片。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/integrations/composio/toolkits", get(list_toolkits))
        .route(
            "/api/integrations/composio/connections",
            get(list_connections),
        )
        .route(
            "/api/integrations/composio/connections/:id",
            delete(delete_connection),
        )
}

/// 一条连接的 wire 形状（上游 `ComposioConnectionResponse` 逐字）。
#[derive(Debug, Serialize)]
struct ConnectionResponse {
    id: String,
    toolkit_slug: String,
    status: String,
    connected_at: String,
    last_used_at: Option<String>,
}

impl ConnectionResponse {
    fn from_view(view: ConnectionView) -> Self {
        Self {
            id: view.id,
            toolkit_slug: view.toolkit_slug,
            status: view.status,
            connected_at: view.connected_at,
            last_used_at: view.last_used_at,
        }
    }
}

/// 一个 toolkit 的 wire 形状（上游 `ComposioToolkitResponse` 逐字）。
///
/// ⚠️ `connectable` 恒 `true`（目录里只剩可连接的）但**必须**发出：老桌面客户端按它分支，
/// 删掉会让它们把每个条目都当成不可连接并**隐藏 Connect 按钮**
/// （上游注释逐字：dropping it would make them treat every entry as non-connectable）。
#[derive(Debug, Serialize)]
struct ToolkitResponse {
    slug: String,
    name: String,
    /// 上游 `logo,omitempty`。
    #[serde(skip_serializing_if = "String::is_empty")]
    logo: String,
    /// 上游 `category,omitempty`。
    #[serde(skip_serializing_if = "String::is_empty")]
    category: String,
    connectable: bool,
}

impl ToolkitResponse {
    fn from_view(view: ToolkitView) -> Self {
        Self {
            slug: view.slug,
            name: view.name,
            logo: view.logo_url,
            category: view.category,
            connectable: view.connectable,
        }
    }
}

/// 上游 `ListComposioConnections`（`integrations_composio.go:135`）。
async fn list_connections(State(state): State<Arc<AppState>>, user: AuthUser) -> Response {
    let service = service_from(&state);
    if !service.enabled() {
        return not_configured();
    }
    match service.list_connections(user.id()).await {
        Ok(connections) => Json(
            connections
                .into_iter()
                .map(ConnectionResponse::from_view)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => {
            tracing::error!(%error, "composio: failed to list connections");
            internal_error("failed to list composio connections")
        }
    }
}

/// 上游 `ListComposioToolkits`（`integrations_composio.go:174`）。
///
/// 顺序：鉴权 401 → 未配置 403 → 目录 502 ⇒ **没有** 400（这条路由没有入参）。
async fn list_toolkits(State(state): State<Arc<AppState>>, user: AuthUser) -> Response {
    // 上游这一条只 `requireUserID`（不解析成 uuid），所以 `user` 只用来说「有会话」。
    let _ = user;
    let service = service_from(&state);
    if !service.enabled() {
        return not_configured();
    }
    match service.list_toolkits().await {
        Ok(toolkits) => Json(
            toolkits
                .into_iter()
                .map(ToolkitResponse::from_view)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => {
            tracing::warn!(%error, "composio: failed to list toolkits");
            upstream_error("failed to list composio toolkits")
        }
    }
}

/// 上游 `DeleteComposioConnection`（`integrations_composio.go:203`）。
///
/// 成功 **204**（无 body）；不属于调用者 / 不存在 ⇒ **404**；上游故障 ⇒ **502**。
async fn delete_connection(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(raw_id): Path<String>,
) -> Response {
    let service = service_from(&state);
    if !service.enabled() {
        return not_configured();
    }
    let connection_id = match parse_uuid(&raw_id, "connection id") {
        Ok(id) => mc_core::Id(id),
        Err(error) => return error_response(error),
    };
    match service.disconnect(user.id(), connection_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(ComposioError::ConnectionNotFound) => error_response(not_found("composio connection")),
        Err(error) => {
            tracing::warn!(%error, "composio: failed to disconnect");
            upstream_error("failed to disconnect composio connection")
        }
    }
}

/// 500（本仓 `internal_error` 家族；`list connections` 的上游是 500 而不是 502）。
fn internal_error(message: &str) -> Response {
    crate::routes::composio::connect::error_with_code(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_composio::service::{ComposioConfig, ComposioService};

    fn service(enabled: bool) -> ComposioService {
        if enabled {
            ComposioService::new(ComposioConfig {
                api_key: Some("ak_test".into()),
                state_secret: Some("state-secret".into()),
                callback_base_url: Some("https://api.example.test".into()),
                feature_enabled: true,
                api_base: Some("http://127.0.0.1:9".into()),
                ..ComposioConfig::default()
            })
        } else {
            ComposioService::new(ComposioConfig::default())
        }
    }

    #[test]
    fn the_connectable_flag_is_always_true_and_kept_on_the_wire() {
        let json = serde_json::to_value(ToolkitResponse::from_view(ToolkitView {
            slug: "notion".into(),
            name: "Notion".into(),
            logo_url: "https://logos.example/notion".into(),
            category: "productivity".into(),
            connectable: true,
        }))
        .expect("json");
        assert_eq!(json["connectable"], serde_json::json!(true));
        assert_eq!(
            json["logo"],
            serde_json::json!("https://logos.example/notion")
        );
        assert_eq!(json["category"], serde_json::json!("productivity"));
    }

    #[test]
    fn empty_logo_and_category_are_omitted_like_upstream_omitempty() {
        let json = serde_json::to_value(ToolkitResponse::from_view(ToolkitView {
            slug: "notion".into(),
            name: "Notion".into(),
            logo_url: String::new(),
            category: String::new(),
            connectable: true,
        }))
        .expect("json");
        let object = json.as_object().expect("object");
        assert!(!object.contains_key("logo"));
        assert!(!object.contains_key("category"));
        let mut keys: Vec<&String> = object.keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["connectable", "name", "slug"],
            "键集是契约（`serde_json::Value` 的对象按键排序，顺序本身不承重）"
        );
    }

    #[test]
    fn the_connection_view_never_exposes_server_internal_handles() {
        let json = serde_json::to_value(ConnectionResponse::from_view(ConnectionView {
            id: "11111111-1111-1111-1111-111111111111".into(),
            toolkit_slug: "notion".into(),
            status: "active".into(),
            connected_at: "2026-09-25T10:00:00Z".into(),
            last_used_at: None,
        }))
        .expect("json");
        let rendered = json.to_string();
        assert!(!rendered.contains("connected_account"), "{rendered}");
        assert!(!rendered.contains("auth_config"), "{rendered}");
        assert!(!rendered.contains("composio_user_id"), "{rendered}");
        let mut keys: Vec<&String> = json.as_object().expect("object").keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "connected_at",
                "id",
                "last_used_at",
                "status",
                "toolkit_slug"
            ]
        );
        assert_eq!(
            json["last_used_at"],
            serde_json::Value::Null,
            "可空字段是显式 null"
        );
    }

    #[test]
    fn the_enabled_gate_is_the_four_condition_one() {
        assert!(service(true).enabled());
        assert!(
            !service(false).enabled(),
            "四条件缺一 ⇒ 空服务（路由回 403）"
        );
    }
}
