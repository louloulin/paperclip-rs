//! WS 帧封装与通用 RPC 载荷 —— 上游 `server/pkg/protocol/messages.go` L77–L116 冻结。
//!
//! # `RawMessage` 的 Rust 映射（偏差登记，见 `docs/16-M3-DAEMON-PROTOCOL.md` §11）
//!
//! 上游 `json.RawMessage`（`Message.Payload`、`RPC*.Body`）是**字节切片**：解码不校验、
//! 编码原样回填。Rust 侧用 `serde_json::Value` 表示，因为
//! `serde_json::value::RawValue` 需要 `serde_json/raw_value` feature，而本 crate 的
//! `Cargo.toml` 由 scaffold 冻结（M3-0 / LUM-1406，不得改）。
//!
//! 由此产生两处**已登记**的差异，都只在「原样字节」这个层面：
//!
//! 1. 键序会被规范化（`RawMessage` 保留对端字节序，`Value` 不保证）；
//! 2. `"payload": null` 与 `"payload"` 缺失都会落成 `Value::Null`（上游分别得到
//!    `"null"` 4 字节与 `nil`）。
//!
//! 语义层（能不能解析出目标类型、未知字段是否忽略）不受影响 —— 那才是路由关心的东西。

use serde::{Deserialize, Serialize};

use super::omit;

/// 所有 WebSocket 消息的信封（`messages.go:112` `Message`）。
///
/// 上游字段名是 Go 关键字外的 `Type`，线上键名固定为 `"type"`；Rust 侧字段叫 `kind`
/// 以避免 `r#type`，`#[serde(rename = "type")]` 保证线上键名逐字不变。
///
/// 上游**未知 `type` 一律忽略**（`hub.go:978` 的 `default` 分支），所以本结构体本身
/// 不校验 `kind` 是否已知 —— 判定交给 [`crate::events::is_known_event`]。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Message {
    /// 线上键 `"type"`。缺省 `""`（Go 零值），与上游「缺失字段不报错」一致。
    #[serde(rename = "type")]
    pub kind: String,
    /// 线上键 `"payload"`。上游无 `omitempty`，但 `json.RawMessage` 缺失时为 `nil`，
    /// 因此这里也默认成 [`serde_json::Value::Null`]。
    pub payload: serde_json::Value,
}

impl Message {
    /// 按 `kind` + 可序列化载荷构造一帧（上游 `mustMarshalRaw` 的 Result 版）。
    ///
    /// 上游 `mustMarshalRaw` 在序列化失败时 **panic**；协议载荷都是可序列化的结构体，
    /// panic 分支实际不可达。Rust 侧返回 `Result`，把「不可达」写成类型而不是注释。
    ///
    /// # Errors
    ///
    /// 载荷无法序列化时返回 `serde_json::Error`。
    pub fn new<T: Serialize>(kind: &str, payload: &T) -> Result<Self, serde_json::Error> {
        Ok(Self {
            kind: kind.to_owned(),
            payload: serde_json::to_value(payload)?,
        })
    }

    /// 把整帧编码成线上 JSON 文本。
    ///
    /// # Errors
    ///
    /// 序列化失败时返回 `serde_json::Error`（`Value` 载荷实际不会失败）。
    pub fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// 把 `payload` 解析成具体载荷类型（解码侧不做种类校验，调用方按 `kind` 分派）。
    ///
    /// # Errors
    ///
    /// 载荷形状与 `T` 不符时返回 `serde_json::Error`。未知**字段**不会导致失败
    /// （见各 payload 的 `#[serde(default)]`）。
    pub fn decode_payload<T: for<'de> Deserialize<'de>>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.payload.clone())
    }
}

/// daemon → server 的通用 RPC 请求信封（`messages.go:87`），帧类型
/// [`crate::events::DAEMON_RPC_REQUEST`]。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RPCRequestPayload {
    /// 关联 id；响应按它回填（`hub.go:1002` 缺失时报错丢弃帧）。
    pub request_id: String,
    /// 服务端 handler 选择器；表见 [`crate::rpc::method::KNOWN`]。
    pub method: String,
    /// method 专属请求体，`omitempty`。
    #[serde(skip_serializing_if = "omit::option")]
    pub body: Option<serde_json::Value>,
    /// 服务端执行预算（毫秒）。`0` = 不设服务端上限（只受连接生命周期约束），
    /// 且 `omitempty` 会省略它（`messages.go:87`；`hub.go:1026` 只在 `> 0` 时设超时）。
    #[serde(skip_serializing_if = "omit::i64")]
    pub timeout_ms: i64,
}

/// server → daemon 的 RPC 响应（`messages.go:104`），帧类型
/// [`crate::events::DAEMON_RPC_RESPONSE`]。
///
/// `status` 就是 HTTP 状态码；`body` 与 `error` **只有一个是有效的** —— 2xx 带 `body`，
/// 失败带 `error`（`hub.go:1037`、L1040）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RPCResponsePayload {
    /// 回声请求的 `request_id`。
    pub request_id: String,
    /// HTTP 语义状态码；常量见 [`crate::rpc`]。
    pub status: i32,
    /// 成功时的响应体（`omitempty`）。
    #[serde(skip_serializing_if = "omit::option")]
    pub body: Option<serde_json::Value>,
    /// 失败时的错误文本（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub error: String,
}
