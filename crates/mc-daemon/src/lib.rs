//! M3 anchor scaffold（LUM-1406）：daemon client + execenv crate —— **占位，无实现**。
//!
//! 由两片填充，都在 W3c：
//!
//! - **M3-7**（`feat/multica-rs-m3c-daemon`）：client + 注册/心跳/claim 状态。
//!   路由在 `crates/mc-http/src/routes/daemon.rs`（scaffold 已接好 `mount_slice_daemon()`），
//!   cover docs/15 §1.5 的 36 条 + §1.2 的 8 条异步往返；
//!   ws 服务端放 `crates/mc-ws` + `crates/mc-realtime`（**唯一**写这两者的切片）。
//!   上游体量：`internal/daemon/daemon.go` 6056 行、`internal/daemonws/*` 1370 行
//!   ⇒ 必然撞 800 行上限，预先按域拆 6 个文件（`daemon/{register,heartbeat,claims,tasks,requests,gc}.rs`）。
//! - **M3-8**（`feat/multica-rs-m3c-adapters`）：`src/execenv/`（上游 99 文件）
//!   + `mc-runtime/src/adapters/` 的 25 个 adapter，分 3 批（8/8/9）各一个 PR。
//!
//! 协议类型不在本 crate：走 `mc-daemon-proto`（M3-1 冻结，依赖已预声明）。
//! `axum`（`ws` feature）与 `tokio-tungstenite` 的版本确认见 `Cargo.toml` 注释。
//!
//! scaffold 阶段本文件只有文档注释：一个类型都不定义。
