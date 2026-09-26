//! `POST /api/feedback` —— **写者 M9-5**（`docs/62` §4.1 的第 6 行）。
//!
//! ## anchor 期（本文件由 M9-0 `LUM-1815` 建桩，`docs/62` §5）
//!
//! **空** `Router::new()` ⇒ **零注册键**。🔴 **不得**注册 501 占位（这 1 条是**上游键**；
//! 占位会让 ⑦ 把它算成 `implemented_placeholder`，⑨ 会从 `unmounted` 变 `mismatch`）。
//!
//! ## M9-5 要填什么
//!
//! 上游：`internal/handler/feedback.go`（177 行）。表 = `feedback`
//! （仓储 = `mc_repos::feedback`，anchor 建的桩）。
//!
//! 三条 `DoD`（`docs/62` §6.5 的 M9-5 行）：
//!
//! 1. **`has_images` 标记**（布尔/计数，不是图片本体 —— 上传通道在附件面）；
//! 2. **限流 10/h ⇒ 429**：**必须复用** `mc_autopilot::webhook::ratelimit::SlidingWindowLimiter`
//!    （`docs/62` §2.3 的"不得重复实现"清单把它点名了）⇒ **禁止**新写第二份限流器；
//! 3. **`workspace_id` / `user_id` 来自鉴权上下文**，不来自请求体。
//!
//! 形态：上游是 plain `r.Post("/api/feedback", …)` ⇒ 只注册**无尾斜杠**那一形态
//! （补尾斜杠 = `EXTRA_ALIAS` 硬失败）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// feedback 切片（M9-5 在这里挂 1 条）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
