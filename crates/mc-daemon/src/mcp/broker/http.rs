//! broker 的 HTTP 出口形状："把纯数据变成 axum 响应"这一层的唯一实现点。
//!
//! 拆出来的理由：它是**纯数据 ↔ 框架类型**的适配，与「闸的顺序」「出网代理」两条逻辑无
//! 关；而且门 ⑩ 是逐文件 800 行硬上限（`docs/57` §6.3）。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use super::super::id_or_null;

/// 一次 broker 请求的答案（纯数据，便于不起 socket 就能断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerHttpResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// 响应体的 `Content-Type`。
    pub content_type: Option<String>,
    /// 透传回来的 `Mcp-Session-Id`。
    pub session_id: Option<String>,
    /// 透传回来的 `Mcp-Protocol-Version`。
    pub protocol_version: Option<String>,
    /// 响应体。
    pub body: Vec<u8>,
}

impl BrokerHttpResponse {
    pub(super) fn json(body: &Value) -> Self {
        Self {
            status: 200,
            content_type: Some("application/json".to_string()),
            session_id: None,
            protocol_version: None,
            body: serde_json::to_vec(body).unwrap_or_default(),
        }
    }

    pub(super) fn not_found() -> Self {
        Self {
            status: 404,
            content_type: Some("text/plain; charset=utf-8".to_string()),
            session_id: None,
            protocol_version: None,
            body: b"not found".to_vec(),
        }
    }

    /// JSON-RPC 错误（上游 `writeRemoteMCPError`：**HTTP 200** + `error` 对象）。
    pub(super) fn error(id: Option<&Value>, code: i64, message: &str) -> Self {
        Self::json(&json!({
            "jsonrpc": "2.0",
            "id": id_or_null(id),
            "error": { "code": code, "message": message },
        }))
    }
}

impl IntoResponse for BrokerHttpResponse {
    fn into_response(self) -> Response {
        let mut builder =
            Response::builder().status(StatusCode::from_u16(self.status).unwrap_or(StatusCode::OK));
        if let Some(content_type) = self.content_type {
            builder = builder.header("content-type", content_type);
        }
        if let Some(session_id) = self.session_id {
            builder = builder.header(mc_mcp::client::SESSION_ID_HEADER, session_id);
        }
        if let Some(version) = self.protocol_version {
            builder = builder.header("mcp-protocol-version", version);
        }
        builder
            .body(axum::body::Body::from(self.body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    }
}
