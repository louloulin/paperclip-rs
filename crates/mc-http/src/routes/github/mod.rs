//! GitHub 面聚合：**7 条**注册键（`docs/61-M8-PLAN.md` §1.1 的第 1–7 行）。
//!
//! ## ⚠️ 本文件由 M8-0 anchor 冻结，M8 后续切片**不得**编辑
//!
//! 合并点在这里；子文件由**各自的写者**实作（`docs/61` §3.3）。若某切片发现需要的子文件
//! 不在下面的清单里，**不要**直接加到这里 —— 记到 `docs/32` §10 的文件→写者表里，由集成方
//! （M8-7）统一加。
//!
//! ## 路由账（逐条点名到 `router.go` 行号）
//!
//! | 注册键 | 方法 | `router.go` | 写者 | 文件 |
//! | --- | :-: | ---: | :-: | --- |
//! | `/api/github/setup` | GET | 1491 | M8-1 | `setup.rs` |
//! | `/api/workspaces/:id/github/connect` | GET | 1757 | M8-1 | `install.rs` |
//! | `/api/workspaces/:id/github/installations` | GET | 1672 | M8-1 | `install.rs` |
//! | `/api/workspaces/:id/github/installations/:installationId/repositories` | GET | 1758 | M8-1 | `install.rs` |
//! | `/api/workspaces/:id/github/installations/:installationId` | DELETE | 1759 | M8-1 | `install.rs` |
//! | `/api/webhooks/github` | POST | 1490 | M8-4 | `webhook.rs` |
//! | `/api/issues/:id/pull-requests` | GET | 2011 | M8-4 | `issue_pr.rs` |
//!
//! 账：M8-1 **5** + M8-4 **2** = **7** ✓（与 `docs/fixtures/m8-declared-routes.tsv` 的 25 行
//! 中 owner=github 的行逐字相等）。
//!
//! ## 授权层（**每一行不同**，`docs/61` §1.1 的 6 簇）
//!
//! `setup` 与 `webhooks/github` 在**公开块**（无会话 middleware；凭据是 state HMAC / webhook
//! HMAC-SHA256）；`installations*` 在 workspace **member** 组；connect 在 workspace **admin**
//! 组；`pull-requests` 在 issue 子路由（workspace member 组）。
//!
//! ## 形态与锚点期注册键
//!
//! `docs/61` §1.4 实测 `dual-form required: 0` ⇒ 每条**只按上游字面量注册那一形态**。
//! anchor 期本目录合并后**只贡献 1 条注册键**（`pull-requests` 的 501 占位 —— 从
//! `routes/issues/mod.rs` **原地搬运**过来，handler 名仍是 `not_implemented`）⇒
//! **注册键集合与占位计数逐字不变**（`docs/61` §6.1 的 M8-0 行）。
//!
//! ⚠️ `dto.rs` 的写者是 **M8-1**，**M8-4 只读**（两片共用的响应映射，`docs/61` §1.2）。

pub mod dto;
pub mod install;
pub mod issue_pr;
pub mod setup;
pub mod webhook;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// GitHub 面的聚合 router（state 由 `main.rs` 的 `with_state` 一次性注入）。
///
/// anchor 期只有 `issue_pr` 贡献 1 条注册键（搬运的 501 占位）；其余子 router 为空。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(setup::router())
        .merge(install::router())
        .merge(webhook::router())
        .merge(issue_pr::router())
}
