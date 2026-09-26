//! 云侧代理面聚合：**16 条**注册键分三个子文件（`docs/62-M9-PLAN.md` §4.1 的第 2/3/7 行）。
//!
//! ## ⚠️ 本文件由 M9-0 anchor（`LUM-1815`）冻结，M9 后续切片**不得**编辑
//!
//! 合并点在这里；三个子文件由**各自的写者**实作（`docs/62` §3.3）：
//!
//! | 文件 | 注册键 | 写者 | 上游 |
//! | --- | :-: | :-: | --- |
//! | `cloud/billing.rs` | 8 | **M9-1** | `cloud_billing.go` L356–L503 |
//! | `cloud/subscriptions.rs` | 7 | **M9-2** | `cloud_billing.go` L54–L355 |
//! | `cloud/webhook.rs` | 1 | **M9-6** | `cloud_billing.go` L504–L604 |
//!
//! 账：8 + 7 + 1 = **16** ✓（16 条出站代理 = 本波唯一需要"云侧可达"的簇）。
//!
//! ## ⚠️ `/api/cloud-runtime/*` 的 11 条**不在**本目录
//!
//! 它们的 crate 归属是 `mc-cloud`（同一份传输），但**路由文件**是
//! `crate::routes::cloud_runtime`（**单文件**切片，写者 **M9-11** / `LUM-2116`）——
//! 见那个文件的模块头与 `mc-cloud/src/runtime.rs`。这是 M9-0 起的**预声明升级**：
//! `docs/64` §5.2 要求 M9-0 在 `routes/cloud/mod.rs` 加 `pub mod runtime;`，但 M9-11 自己的
//! 写集点名的文件是 `crates/mc-http/src/routes/cloud_runtime.rs`（**顶层单文件**），
//! 而 `routes/{mod,mount}.rs` 的追加由 M9-11 自己承担 ⇒ anchor 改为**按 M9-11 的实际路径**
//! 预声明（`pub mod cloud_runtime;` + `mount_slice_cloud_runtime()`），**零注册键**。
//! 这样 M9-11 一行 frozen 文件都不用碰。裁定见 `docs/32` §9.13。
//!
//! ## 授权层（**每簇不同**，`docs/62` §1.5 的 4 种 × 10 簇）
//!
//! billing 8 条 = **账户级**（⇒ 必须挂机器凭据闸）；subscriptions 读 2 条 = member、
//! 写 5 条 = owner|admin + rollout flag；stripe = **公开**（无会话，凭据是云侧验签）。
//!
//! ## 锚点期**零注册键**
//!
//! 三个子文件现在都是**空** `Router::new()` ⇒ `mount_slice_commercial()` 合并进全局 router
//! 之后**注册键集合逐字不变**（`docs/62` §6.1 的 M9-0 行：`local 474 / baseline 473` 不动
//! —— 这是本仓**第三个**不刷 ⑦ 基线的 anchor，前两个是 M7-0 / M8-0）。

pub mod billing;
pub mod subscriptions;
pub mod webhook;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 云侧代理面的聚合 router（anchor 期 = 三次空 merge）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(billing::router())
        .merge(subscriptions::router())
        .merge(webhook::router())
}
