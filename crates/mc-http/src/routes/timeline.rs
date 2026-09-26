//! `GET /api/issues/:id/timeline` —— **写者 M9-8**（`docs/62` §4.1 的第 9 行）。
//!
//! # 这条路由的历史（anchor 的「原地搬运」，`docs/62` §3.1 / §5）
//!
//! 本路由原先注册在 `crate::routes::issues::router()`（`issues/mod.rs` 的那一行
//! `.route("/api/issues/:id/timeline", get(not_implemented))`，handler `not_implemented`）。
//! M9-0 把它**搬到这里**，因为它的真实现归 M9-8，而 M9-8 不该回来改 `issues/mod.rs`
//! （冻结的 anchor 文件）。搬运的四条不变量：
//!
//! 1. **注册键逐字不变**（`GET /api/issues/:id/timeline`）⇒ ⑦ 的 `local 474` 不动；
//! 2. **handler 名仍是 `not_implemented`**（门 ⑦ 的占位正则是
//!    `\b(?:placeholder|not_implemented)\b`，`route_parity.py:116`）⇒
//!    `implemented_placeholder` 的 **3** 不变、`implemented_real` 不变
//!    ⇒ 本 anchor **不刷** `docs/fixtures/route-parity-baseline.json`；
//! 3. 路由**必须存在**（`docs/62` §2.7 第 7 条：不得为了 ⑦ 好看而删键）；
//! 4. **`implemented + known_gap == 456`** 的不变式在这一步前后逐字相同
//!    （搬运不改变 `known_gap` —— 这条键的 owner 仍是 `M9`，仍然是缺口）。
//!
//! # M9-8 要填什么
//!
//! 上游：`internal/handler/activity.go` 的 **`L63–L393`**（`L394` 起是 M2-A 的
//! `GetAssigneeFrequency`，**不属本波**）。
//!
//! 四条 `DoD`（`docs/62` §6.5 的 M9-8 行）：
//!
//! 1. comments + `activity_log` 合并的**顺序与去重**；
//! 2. keyset 四参（`before` / `after` / `around` / `limit`）的边界；
//! 3. 🔴 **两侧独立截断、不 clamp 到同一个 floor**（上游注释逐字）+ 响应头
//!    `X-Timeline-Truncated`（**handler 侧加，本文件**）；
//! 4. **非本 workspace 的 issue ⇒ 404**（不是 403、也不是空列表）。
//!
//! 数据面在 `mc_repos::timeline`（anchor 建的桩）；行形状与合并语义的**判据**在
//! `docs/62` 与 `crates/mc-repos/src/timeline.rs` 的模块头。
//!
//! ⚠️ **不碰** `GetAssigneeFrequency`（`crate::routes::stats` 已交付，是 M2-A 的账）；
//! **不补** `activity_log` 的写入面（R-M9-4 由 M9-10 登记）。

use axum::routing::get;
use axum::Router;
use std::sync::Arc;

// 搬运的第 2 条不变量：**handler 名逐字保留** —— 门 ⑦ 按名字认占位
// （`route_parity.py:116` 的 `PLACEHOLDER_HANDLER`）。换成任何别的名字都会让
// `implemented_placeholder` 减一（⑦ 读数变化 + 基线要刷），而本片**不刷基线**。
use crate::routes::issues::not_implemented;
use crate::state::AppState;

/// issue timeline 切片（搬运后仍是 1 条 501 占位，M9-8 把它换成真实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/issues/:id/timeline", get(not_implemented))
}
