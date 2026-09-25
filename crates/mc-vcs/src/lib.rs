//! `mc-vcs`：自建 Git provider 的**抽象层**（`Provider` trait / registry / 两种验签方案 /
//! forgejo · gitlab 两个 adapter）。
//!
//! **状态：M8-0 anchor 只落文件与边界**（`LUM-1797` / `docs/61-M8-PLAN.md` §2.1 / §5）——
//! 本文件只有模块声明与下面的归属表；`provider.rs` / `registry.rs` / `signature.rs` 定的是
//! **契约**，`forgejo.rs` / `gitlab.rs` 是**桩**（`register()` 空体 + `todo!()` 位）。
//! 所以本 crate 现在可编译、可门禁、可独立验收，但**不实现任何路由、不碰任何平台 wire**。
//!
//! ## 为什么是**一个**新 crate（`docs/61` §2.2 的三条判据）
//!
//! 1. **不被 daemon 与 http 同时依赖**：每连接签名与事件解析都是**请求内**的事
//!    ⇒ 不落 `mc-core`；
//! 2. **per-provider trait 抽象是硬需求**（判据 2）：上游 `integrations/vcs` 有
//!    `Provider` 接口 + `registry`（`register()` 在 `init()` 里），而 GitHub 是
//!    `handler/github.go` 里的单实现、直连 REST/GraphQL ⇒ **crate 数 ≥ 2**；
//! 3. **凭据共用不构成合并理由**（判据 3）：四类部署密钥都经 `mc-secrets::secretbox`
//!    或直接读 env，而**验签算法分三种**（GitHub HMAC-SHA256、Forgejo/Gitea HMAC-SHA256、
//!    GitLab 明文比较）⇒ 共用件已在 `mc-secrets`，不在这里。
//!
//! ## 边界契约（`docs/61` §2.7，逐条可测）
//!
//! 1. **`mc-vcs` 不知道 GitHub**：`Provider` trait 的 6 个方法不得出现 `github` 字样；
//!    GitHub 也**不注册**进本 crate 的 registry。
//! 2. **凭据只经 `secretbox` 或 env**：任何 adapter / 路由不得把明文 secret 放进
//!    `Debug` / `Display` / 日志插值。
//! 3. **验签必须常量时间**：HMAC 用 `hmac::Mac::verify_slice`；GitLab 的明文 token 比较
//!    也必须走 [`signature::constant_time_eq`]（反例：签名差 1 位必失败）。
//!
//! ## 上游与写者（`docs/61` §3.3 的写集表）
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `src/lib.rs` | M8-0（本 anchor） | —— |
//! | `src/provider.rs` | M8-0 建 / **M8-2 填** | `vcs/vcs.go`（`Provider` 六方法 / `Account` / `ErrUnauthorized`） |
//! | `src/events.rs` | M8-0 建 / **M8-2 填** | `vcs/vcs.go`（`PullRequestEvent` / `CIStatusEvent` / `Terminal()`） |
//! | `src/registry.rs` | M8-0 建 / **M8-2 填** | `vcs/vcs.go`（`register` / `For`） |
//! | `src/signature.rs` | M8-0 建 / **M8-2 填** | `vcs/forgejo.go` + `vcs/gitlab.go`（两种验签） |
//! | `src/forgejo.rs` / `src/gitlab.rs` | M8-0 建桩 / **M8-2 原地填充** | `vcs/forgejo.go`（257 行）/ `vcs/gitlab.go`（248 行） |
//!
//! ## 不做什么（anchor 的硬边界）
//!
//! - **零路由逻辑**：5 条 VCS 路由全在 `mc-http/src/routes/vcs/**`，由 M8-2 填；
//! - **零平台 wire**：没有 Forgejo/GitLab 的载荷解析、没有 `/api/v1/user` 调用；
//! - **零新迁移**：4 张 VCS 表都在 `migrations/upstream/**`（`docs/61` §6.4）；
//! - **不读 env**：每连接密钥的读取口在 `mc_http::state`（`MULTICA_VCS_SECRET_KEY`）。

pub mod events;
pub mod forgejo;
pub mod gitlab;
pub mod provider;
pub mod registry;
pub mod signature;

pub use events::{Account, CIStatusEvent, EventKind, PullRequestEvent};
pub use forgejo::ForgejoProvider;
pub use gitlab::GitLabProvider;
pub use provider::{Provider, VcsError};
pub use registry::{Registry, RegistryError};
