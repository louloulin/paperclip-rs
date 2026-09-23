//! M5-0 anchor：`POST /api/webhooks/autopilots/:token`（autopilot webhook 入口）—— **空 router 占位**。
//!
//! - **写者**：M5-5（`docs/44` §3.2）。切片只实现本文件的 `router()`，不改
//!   `webhooks/mod.rs` / `mount.rs` / `routes/mod.rs`。
//! - **路由**（`router.go:1487`）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 21 | POST | `/api/webhooks/autopilots/:token` | `HandleAutopilotWebhook` | 298 |
//!
//! - **单形态**（plain 路由）⇒ 不要加尾斜杠别名。
//! - **与 M5-3 的 token 形态对齐**：路径形态由 `webhookPathForToken`4 定（trigger 写面铸造
//!   token 的地方），两边**逐字一致**；token → trigger/workspace 的反查在
//!   `mc_repos::autopilot::ingress`。
//! - **上游体量**：`handler/autopilot_webhook.go` 1,010 + admission 段 ⇒ 真值在
//!   `mc_autopilot::webhook/**`（签名 / 限流 / admission / provider 四个文件），本文件只做
//!   「取 token → 读 body → 调 service → 映射状态码」。
//! - **状态码契约**：入站被拒（签名不合法 / provider 不允许 / 限流）与「接受但跳过派发」是**两组**
//!   语义 —— 前者落 `webhook_delivery.status ∈ {rejected, failed}`，后者仍是 `dispatched`
//!   （跳过记在 `autopilot_run.status`），**不要用同一个状态码糊过去**。
//! - **⑨ 现状**：本路由**零 fixture**（`contracts/golden/autopilots/` 8 条只碰 2 条路由）
//!   ⇒ 等价证据靠本地 e2e（`crates/mc-http/tests/autopilots/**`，`docs/44` §6.2 的补救口径）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M5-5 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
