//! M3 anchor scaffold（LUM-1406）→ daemon 协议冻结 crate（LUM-1407 / M3-1 已填充）。
//!
//! 产物是 `docs/16-M3-DAEMON-PROTOCOL.md` 里那张**冻结契约表**对应的 Rust 类型：
//!
//! - 上游来源：`server/pkg/protocol/messages.go`（**29** 个 payload 类型 + `const` 块）、
//!   `server/pkg/protocol/events.go`（109 条事件常量）、`server/internal/daemonws/hub.go`
//!   与 `server/internal/handler/daemon_rpc.go`（帧/RPC 契约）；
//!   冻结上游 commit：`f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`；
//! - 只定型 body / 帧与 RPC method 名表，**不实现** ws 传输、连接管理、hub、notifier、metrics；
//! - 不落库；不引 `tokio-tungstenite`（属 M3-7 的 `mc-daemon`，已在那边预声明）。
//!
//! 约定（scaffold 冻结，切片不得改）：
//! - 字段名 / 可选性 / `omitempty` 语义与上游 Go 结构体逐字一致（golden JSON 反序列化断言）；
//! - 未知字段容忍（`#[serde(default)]`），未知 event 忽略；
//! - 超 800 行按 `messages/`、`events/`、`rpc/` 拆（docs/15 §7.7 的 R7）。
//!
//! # 模块
//!
//! | 模块 | 内容 |
//! |------|------|
//! | [`messages`] | 29 个 payload 类型（按 `envelope`/`daemon`/`task`/`chat` 分文件） |
//! | [`events`] | 109 条事件常量 + [`events::is_known_event`] |
//! | [`capabilities`] | 12 条能力常量 + 三个方向的协商规则 |
//! | [`rpc`] | RPC method 名表 + 传输/状态码常量 |
//!
//! 本 crate 的 `lib.rs` 只做**声明与再导出**：所有内容都在上面四个模块里，因此
//! W3b/W3c 的 handler 切片按 `use mc_daemon_proto::...` 取类型即可，不需要、也不应该
//! 再改本文件（它同时是 M3-1 与 M3-7 的共享锚点）。
//!
//! # 与上游的差异（全部已登记在 `docs/16` §11）
//!
//! - `json.RawMessage` → `serde_json::Value`（`serde_json/raw_value` 需要改 `Cargo.toml`，
//!   而 scaffold 冻结了它）；
//! - 上游 nil slice 出站是 `null`，Rust `Vec` 出站是 `[]`（入站两者都落 `Vec::default()`）；
//! - 上游 `mustMarshalRaw` 序列化失败即 panic，本 crate 返回 `Result`。

#![forbid(unsafe_code)]

pub mod capabilities;
pub mod events;
pub mod messages;
pub mod rpc;

pub use capabilities::*;
pub use events::*;
pub use messages::*;
pub use rpc::*;
