//! `mc-composio`：composio 集成（SDK 客户端 / 服务 / state HMAC / toolkit 目录 /
//! per-task MCP overlay 构建）。
//!
//! **状态：M8-0 anchor 只落文件与边界**（`LUM-1797` / `docs/61-M8-PLAN.md` §2.1 / §5）——
//! 本文件只有模块声明与下面的归属表；`client.rs` 落 `api_base` 接缝与「未配置」判据，
//! `service.rs` / `state.rs` / `catalog.rs` / `overlay.rs` 是**桩**（签名 + `todo!()`）。
//! 所以本 crate 现在可编译、可门禁、可独立验收，但**不实现任何路由、不碰任何平台 wire**。
//!
//! ## 为什么是**独立** crate（`docs/61` §2.2 判据 1 的第二半）
//!
//! 上游 `router.go:1252` 逐字 `h.TaskService.Composio = svc` ⇒ composio 服务**同时**被
//! ① 5 条 HTTP 路由、② task 派发服务（算 per-task MCP overlay）使用。生产者必须在
//! `mc-http` 与派发层都能拿到 ⇒ 独立 crate（塞进 `mc-http` 会让依赖方向反向）。
//!
//! ## 四个「未配置」条件缺一即不装配（`docs/61` §2.5 / §2.4）
//!
//! `COMPOSIO_API_KEY`、flag（`mc-feature-flags`）、`COMPOSIO_STATE_SECRET`|`JWT_SECRET`、
//! `COMPOSIO_CALLBACK_BASE_URL`|`MULTICA_PUBLIC_URL` —— 四者缺一，4 条会话路由返回 **503**。
//! 装配判据集中在 [`service::ComposioConfig::is_configured`]，**不许**散在 handler 里。
//!
//! ## 凭据纪律（`docs/61` §2.4 的四条判据）
//!
//! `COMPOSIO_API_KEY` / state secret / 会话 URL 里的 bearer **都不派生 `Debug`**：
//! 只经 [`client::ComposioClient`] 与 [`state::StateSigner`] 的不透明接口流转。
//!
//! ## 上游与写者（`docs/61` §3.3 的写集表）
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `src/lib.rs` / `src/client.rs` | M8-0 | —— |
//! | `src/service.rs` / `src/state.rs` / `src/catalog.rs` / `src/overlay.rs` | M8-6 | `integrations/composio/**`（1,050 行）+ `handler/integrations_composio.go` |
//!
//! ## 不做什么（anchor 的硬边界）
//!
//! - **零路由逻辑**：5 条 composio 路由全在 `mc-http/src/routes/composio/**`，由 M8-6 填；
//! - **零平台 wire**：没有 SDK 调用、没有回调处理、没有 state 签发；
//! - **零新迁移**：`user_composio_connection` 表已在 `migrations/upstream/**`（`docs/61` §6.4）；
//! - **不读 env**：部署密钥的唯一读取口在 `mc_http::state`（`composio` 字段）。

pub mod catalog;
pub mod client;
pub mod overlay;
pub mod service;
pub mod state;

pub use client::ComposioClient;
pub use service::{ComposioConfig, ComposioService};
pub use state::{StateError, StateSigner};
