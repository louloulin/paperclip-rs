//! Terminal WebSocket — R628 复刻 paperclip Node
//! `server/src/realtime/environment-custom-image-terminal-ws.ts` (766 LOC)。
//!
//! 范围（本轮）：核心数据契约与可单测的纯函数。
//! - [`frame`]：客户端/服务端双向 JSON 帧协议（auth / resize / output / ready / error）
//! - [`path`]：WS upgrade URL 解析
//! - [`traits`]：`TerminalSshConnector` + `TerminalSshShell` trait（mockable）
//!
//! 留待后续轮次：
//! - `handler.rs`：完整 WS 升级 + auth 超时 + SSH stream 桥接
//! - `ssh2_connector.rs`：`russh` 或 `ssh2` 真实实现（feature-gated）
//! - 与 `environment_custom_image_terminal_session_store` 集成
//!
//! 设计原则（与 OpenClaw Gateway / Cursor Cloud execute 同款）：
//! - **trait 抽象**：所有 IO / 时间副作用通过 trait，可单测 + fake
//! - **零 unsafe**：纯 safe Rust
//! - **错误模型**：`TerminalWsError` 单一 enum，无 anyhow
//! - **路径 1:1 对齐 Node 上游**：URL 形状、frame schema 完全一致，方便互操作

pub mod frame;
pub mod handler;
pub mod path;
pub mod session_store;
pub mod traits;

pub use frame::{ClientFrame, ClientFrameError, ServerFrame};
pub use handler::{handle_socket, parse_upgrade_path};
pub use path::{parse_terminal_path, TerminalPathError};
pub use session_store::{HostKeyVerifier, InMemoryStore, TerminalSessionRecord, TerminalSessionStore};
pub use traits::{
    FakeSshConnector, FakeSshShell, SshConnectionParams, TerminalDimensions, TerminalSshConnector,
    TerminalSshShell,
};
