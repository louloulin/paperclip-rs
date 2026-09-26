//! 两条 **DEPRECATED** shim（`runtime-bootstrap` / `no-runtime-bootstrap`）—— **写者 M9-3**。
//!
//! ## anchor 期（本文件由 M9-0 `LUM-1815` 建桩，`docs/62` §5）
//!
//! **空** `Router::new()` ⇒ **零注册键**（本片 ⑦ 读数逐字不变：`local 474 / baseline 473 /
//! implemented 389 real + 3 placeholder / known_gap 64 / owners.M9 33 / local_only 8`）。
//! 🔴 **不得**在这里注册 501 占位：这 2 条是**上游键**，占位会让 ⑦ 把它们算成
//! `implemented_placeholder`（`owners.M9` 假清零），而 ⑨ 会从 `unmounted` 变 `mismatch`。
//!
//! 上游：`internal/handler/onboarding_shim.go`（623 行）+ `bootstrapOnboarding{Runtime,NoRuntime}Request/Response`。
//! ⚠️ 这两条**是活路由**（不是反向验收）：`docs/62` §1.8 实测本波有 2 条 DEPRECATED 活路由。
//! provision 链要**逐行断言**（helper agent + starter issue 各一行）。
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

/// DEPRECATED shim 切片（M9-3）：两条 provision 链（helper agent + starter issue）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
