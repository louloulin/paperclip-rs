//! `POST /api/webhooks/stripe` 1 条（**写者 M9-6** / `docs/62` §4.1 的第 7 行）。
//!
//! ## anchor 期（本文件由 M9-0 `LUM-1815` 建桩，`docs/62` §5）
//!
//! **空** `Router::new()` ⇒ **零注册键**（本片 ⑦ 读数逐字不变：`local 474 / baseline 473 /
//! implemented 389 real + 3 placeholder / known_gap 64 / owners.M9 33 / local_only 8`）。
//! 🔴 **不得**在这里注册 501 占位：这 1 条是**上游键**，占位会让 ⑦ 把它们算成
//! `implemented_placeholder`（`owners.M9` 假清零），而 ⑨ 会从 `unmounted` 变 `mismatch`。
//!
//! 上游：`internal/handler/cloud_billing.go` 的 `L504–L604`。三段本地语义的**顺序**是 `DoD` 的一部分
//! （`docs/62` §6.5 的 M9-6 行）。⚠️ 这条**不挂**机器凭据闸（上游也**不**注入 `X-User-ID`
//! —— 它没有人类身份；`mc-cloud::transport::Request::user_id` 留 `None`）。
//!
//! 形态（`docs/62` §1.4 实测 `declared 34 / dual-form required: 3`）：本波**只有**
//! `/api/notification-preferences` 那 3 条需要补尾斜杠形态；本文件的路由**只按上游字面量
//! 注册那一形态**。路径参数写 `:name`（matchit 0.7 把 `{name}` 当字面量 ⇒ 编译通过且恒 404）。
//!
//! 同 path+method 重复注册 ⇒ axum 在**启动时 panic**（`docs/15` §9.6.6）—— 这是本 anchor
//! 把合并点与子文件分开的唯一理由。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// stripe 转发切片（M9-6 在这里挂那一条：403 → 429 → 401 → 413 → 原始体转发）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
