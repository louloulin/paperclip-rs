//! `POST /api/contact-sales` —— **写者 M9-5**（`docs/62` §4.1 的第 6 行）。
//!
//! ## anchor 期（本文件由 M9-0 `LUM-1815` 建桩，`docs/62` §5）
//!
//! **空** `Router::new()` ⇒ **零注册键**。🔴 **不得**注册 501 占位（理由同上）。
//!
//! ## M9-5 要填什么
//!
//! 上游：`internal/handler/contact_sales.go`（323 行）。表 = `contact_sales_inquiry`
//! （仓储 = `mc_repos::contact_sales`，anchor 建的桩）。
//!
//! 四条 `DoD`（`docs/62` §6.5 的 M9-5 行）：
//!
//! 1. 🔴 **"公开面"的一种**：上游用**无会话**请求验它 ⇒ 这条是**公开**路由，
//!    **不得**挂 `AuthUser` 提取器；`workspace_id` 也不参与（`docs/62` §4.2 的 contact-sales 行）；
//! 2. **企业邮箱域名拒绝**（反例必测）；
//! 3. **`company_size` 枚举校验**；
//! 4. **限流 5/h ⇒ 429**（`RATE_LIMIT_CONTACT_SALES`，缺省用默认值）：同样**复用**
//!    `SlidingWindowLimiter`，**禁止**新写。
//!
//! 形态：上游是 plain `r.Post("/api/contact-sales", …)` ⇒ 只注册**无尾斜杠**形态。
//!
//! ⚠️ 本路由**不在** `mount_slice_commercial()` 的 "无会话" 特殊处理里 —— 它只是
//! **不挂**鉴权提取器而已（axum 的提取器按 handler 挂、不全局 ⇒ 天然满足，
//! 与 `probes/*` 的根路径路由同理）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// contact-sales 切片（M9-5 在这里挂 1 条，且**不挂**鉴权提取器）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
