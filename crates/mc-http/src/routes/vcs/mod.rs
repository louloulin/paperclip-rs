//! VCS 面聚合：**5 条**注册键（`docs/61-M8-PLAN.md` §1.1 的第 8–12 行）—— 写者 **M8-2**。
//!
//! | 注册键 | 方法 | `router.go` | 文件 |
//! | --- | :-: | ---: | --- |
//! | `/api/workspaces/:id/vcs/connections` | GET | 1676 | `connections.rs` |
//! | `/api/workspaces/:id/vcs/connections` | POST | 1761 | `connections.rs` |
//! | `/api/workspaces/:id/vcs/connections/:connectionId` | DELETE | 1763 | `connections.rs` |
//! | `/api/workspaces/:id/vcs/connections/:connectionId/rotate-webhook` | POST | 1762 | `connections.rs` |
//! | `/api/webhooks/vcs/:connectionId` | POST | 1500 | `webhook.rs` |
//!
//! - **授权层**：3 条写面（POST/DELETE/rotate）在 workspace **admin** 组；connections 列表在
//!   **member** 组；`webhooks/vcs/:connectionId` 在**公开块**（凭据是**每连接**的签名，见 §1.6）。
//! - **两层语义（别统一成 503）**：`isVCSAvailable()==false`（产品边界）⇒ 403/404；
//!   `isVCSConfigured()==false`（缺密钥）⇒ 503（`docs/61` §2.5）。
//! - **凭据纪律**：per-connection secret 落库必须是 `secretbox` **密文**（明文入库即失败）；
//!   响应/日志不得回显 PAT 或 webhook secret（`docs/61` §2.4 / M8-2 的 `DoD`）。
//! - **anchor 期**：三个子文件全是**空** `Router::new()` ⇒ 合并后**零注册键**。

pub mod connections;
pub mod dto;
pub mod webhook;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// VCS 面的聚合 router（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(connections::router())
        .merge(webhook::router())
}
