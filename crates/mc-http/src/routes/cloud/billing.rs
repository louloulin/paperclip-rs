//! `/api/cloud-billing/*` 8 条（**写者 M9-1** / `docs/62` §4.1 的第 2 行）。
//!
//! ## anchor 期（本文件由 M9-0 `LUM-1815` 建桩，`docs/62` §5）
//!
//! **空** `Router::new()` ⇒ **零注册键**（本片 ⑦ 读数逐字不变：`local 474 / baseline 473 /
//! implemented 389 real + 3 placeholder / known_gap 64 / owners.M9 33 / local_only 8`）。
//! 🔴 **不得**在这里注册 501 占位：这 8 条是**上游键**，占位会让 ⑦ 把它们算成
//! `implemented_placeholder`（`owners.M9` 假清零），而 ⑨ 会从 `unmounted` 变 `mismatch`。
//!
//! 上游：`internal/handler/cloud_billing.go` 的 `L356–L503`。八条全是 `proxyCloudRuntime` 的**纯透传**：
//! 本地 0 张表（`docs/62` §9.4）⇒ 出站路径表见 `mc-cloud/src/billing.rs` 的模块头。
//!
//! 🔴 八条**全部**要挂机器凭据闸（`crate::actor_guard::require_human_actor`，R-M9-2：
//! 「没有这条闸就不许合并 M9-1/M9-2」）。
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

/// billing 切片（M9-1 在这里挂 8 条 owner-credit 出站代理）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
