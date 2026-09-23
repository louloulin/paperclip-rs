//! `mc-daemon`：daemon 侧客户端 + （M3-8）执行环境。
//!
//! ## 本切片（M3-7 / LUM-1438）交付什么
//!
//! `feat/multica-rs-m3c-daemon` 这一片的 `mc-daemon` 部分只做**客户端**：
//!
//! | 模块 | 内容 |
//! |------|------|
//! | [`wire`] | daemon → server 的线上请求/响应类型 + 路径常量 |
//! | [`transport`] | [`DaemonTransport`] 传输缝 + 真 HTTP 实现 [`HttpTransport`] |
//! | [`state`] | [`ClientState`]：登记台账 / 心跳水位 / in-flight 认领去重 |
//! | [`client`] | [`DaemonClient`]：`register` / `heartbeat` / `claim` / `deregister` |
//!
//! 服务端那半边（36 条 `/api/daemon/*` + 8 条 `/api/runtimes/{id}` 异步往返 + ws hub）
//! 在 `crates/mc-http/src/routes/daemon/` 与 `crates/mc-ws`，不在本 crate。
//!
//! ## 不在本切片
//!
//! - **执行环境**（`src/execenv/`，上游 99 文件）：M3-8（`LUM-1440`），与本模块共用
//!   这个 `lib.rs` 与 `Cargo.toml`，所以两者是**串行**关系，不能并行开工。
//! - **WS 客户端**（`daemon:rpc_request` 帧）：本 crate 的 `tokio-tungstenite` 依赖是
//!   给 M3-8 的连接管理预声明的；M3-7 只实现 HTTP 腿。RPC 腿要做的就是再写一个
//!   [`DaemonTransport`] 实现 —— 客户端逻辑一行不改。
//! - **待办动作的执行**（升级 CLI / 拉模型列表 / 导入技能）：[`client::PendingWork`]
//!   只给出「该做什么」的清单，具体动作在 M3-8。
//!
//! ## 与上游的偏离
//!
//! 逐条记在 `docs/32-M3-DAEMON-FACE.md` 的偏离表里（D-1 dev-mode 身份、D-9 客户端
//! 侧 DTO 重复定义、D-10 `reqwest` 依赖等），不在代码里静默省略。

pub mod client;
pub mod state;
pub mod transport;
pub mod wire;

pub use client::{
    ClaimOutcome, ClientConfig, ClientError, DaemonClient, HeartbeatOutcome, PendingWork,
};
pub use state::{ClientState, Registration};
pub use transport::{DaemonTransport, HttpTransport, TransportError, CLIENT_VERSION_HEADER};
pub use wire::{
    ClaimRequest, ClaimResponse, ClaimedTask, DeregisterRequest, FailedProfile, RegisterRequest,
    RegisterResponse, RegisterRuntime, RegisteredRuntime, CLAIM_PATH, DEREGISTER_PATH,
    HEARTBEAT_PATH, REGISTER_PATH,
};
