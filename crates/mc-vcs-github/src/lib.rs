//! `mc-vcs-github`：GitHub App 的**运行时**（App JWT / installation token 缓存 / REST /
//! webhook 验签与事件分派 / PR 镜像 / 自动关联与关闭 / ghsnapshot 管道）。
//!
//! **状态：M8-0 anchor 只落文件与边界**（`LUM-1797` / `docs/61-M8-PLAN.md` §2.1 / §5）——
//! 本文件只有模块声明与下面的归属表；`port.rs` / `app.rs` 定的是**契约**（且 `app.rs` 的
//! RS256 原语在本片**实测签发→验证往返**），`ghsnapshot/client.rs` 放 `api_base` 接缝，
//! 其余都是**桩**（签名 + `todo!()`）。所以本 crate 现在可编译、可门禁、可独立验收，
//! 但**不实现任何路由、不碰任何平台 wire**。
//!
//! ## 两个宿主（同一 crate，靠 `port.rs` 的 trait 分界）
//!
//! `ghsnapshot::Manager` 是**长期后台 worker**（worker 池 + TTL sweeper + 限流暂停），
//! 宿主必须是 `apps/mc-server`；而 `Client` / `payload` / `mirror` 是**请求内**的事，
//! 宿主是 `mc-http`。两者不拆 crate 的理由：它们**共用同一套 App 凭据链**
//! （两边都读 `GITHUB_APP_ID` / `GITHUB_APP_PRIVATE_KEY`）⇒ 拆开会立刻出现第二份 JWT
//! 签名与第二份 token 缓存（`docs/61` §2.2 被否决备选第 2 条）。
//!
//! ## 离线替身的接缝（R-M8-1，`docs/61` §4.2）
//!
//! - `rest.rs` 的 Client 与 `ghsnapshot/client.rs` 的 `Client` 都有 `api_base` 字段
//!   （默认 `https://api.github.com`），上游逐字注释「Mutable so tests can…」⇒
//!   本地替身按 REST / GraphQL 形状答，端到端断言链不需要真连 GitHub。
//!
//! ## 凭据纪律（`docs/61` §2.4 的四条判据）
//!
//! App 私钥（PEM）与 installation token **都不实现 `Debug` 派生**：只经
//! [`app::AppJwtSigner`] / [`token_cache::InstallationToken`] 的不透明接口流转，
//! 任何 `tracing::*` 都不得插值它们。
//!
//! ## 上游与写者（`docs/61` §3.3 的写集表）
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `src/lib.rs` / `src/port.rs` / `src/ghsnapshot/mod.rs` | M8-0 | —— |
//! | `src/app.rs` / `src/token_cache.rs` / `src/rest.rs` / `src/dto.rs` / `ghsnapshot/client.rs` | M8-1 | `handler/github.go` L1–L963 + `ghsnapshot/client.go` |
//! | `src/payload.rs` / `src/webhook.rs` / `src/mirror.rs` / `src/closepolicy.rs` / `src/links.rs` | M8-4 | `handler/github.go` L964–L1997 |
//! | `src/ghsnapshot/snapshot.rs` / `src/ghsnapshot/refresh.rs` | M8-5 | `ghsnapshot/{snapshot,refresh}.go` |
//!
//! ## 不做什么（anchor 的硬边界）
//!
//! - **零路由逻辑**：7 条 GitHub 路由全在 `mc-http/src/routes/github/**`，由 M8-1/M8-4 填；
//! - **零平台 wire**：没有 REST 调用、没有 GraphQL 查询、没有 webhook 事件处理；
//! - **零新迁移**：7 张 GitHub 面表都在 `migrations/upstream/**`（`docs/61` §6.4）；
//! - **不读 env**：部署密钥的唯一读取口在 `mc_http::state`（`github` 字段）。

pub mod app;
pub mod closepolicy;
pub mod dto;
pub mod ghsnapshot;
pub mod links;
pub mod mirror;
pub mod payload;
pub mod port;
pub mod rest;
pub mod token_cache;
pub mod webhook;

pub use app::{AppJwtError, AppJwtSigner};
pub use port::{
    DisabledPrRefresh, GithubAppConfig, PrRefreshPort, PrRefreshRequest, RefreshReason,
    SharedPrRefresh,
};
pub use token_cache::{InstallationToken, InstallationTokenCache};
