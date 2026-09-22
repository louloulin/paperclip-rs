//! M3 anchor scaffold（LUM-1406）：daemon 协议冻结 crate —— **占位，无实现**。
//!
//! 由 M3-1（`feat/multica-rs-m3a-daemon-proto`）填充，产物是
//! `docs/16-M3-DAEMON-PROTOCOL.md` 里那张**冻结契约表**对应的 Rust 类型：
//!
//! - 上游来源：`server/pkg/protocol/messages.go`（42 个 payload 类型）、
//!   `server/pkg/protocol/events.go`、`server/internal/daemonws/hub.go`；
//! - 只定型 body / 帧与 RPC method 名表，**不实现** ws 传输、连接管理、hub、notifier、metrics；
//! - 不落库；不引 `tokio-tungstenite`（属 M3-7 的 `mc-daemon`，已在那边预声明）。
//!
//! 约定（scaffold 冻结，切片不得改）：
//! - 字段名 / 可选性 / `omitempty` 语义与上游 Go 结构体逐字一致（golden JSON 反序列化断言）；
//! - 未知字段容忍（`#[serde(default)]`），未知 event 忽略；
//! - 超 800 行按 `messages/`、`events/`、`rpc/` 拆（docs/15 §7.7 的 R7）。
//!
//! scaffold 阶段本文件只有文档注释：一个类型都不定义，避免 W3a 三片在共享锚点之外
//! 再撞同一个文件（`Cargo.lock` 与 `crates/*` 成员集合已由本片一次性落定）。
