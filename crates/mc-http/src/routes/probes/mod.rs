//! ops 探针面聚合（M10 anchor 冻结）：3 个注册键 = 3 个子 router。
//!
//! ## ⚠️ 本文件由 M10-0 anchor（`LUM-2102`）冻结，M10 后续切片**不得**编辑
//!
//! 合并点在这里；三个子文件由**各自的写者**实作（`docs/64` §3.3）：
//!
//! | 文件 | 注册键 | 上游 handler | 写者 |
//! | --- | :-: | --- | :-: |
//! | `probes/live.rs` | `GET /health` | `health.go::liveHandler`（`f41fae6b08fb` L85-91） | **M10-1** |
//! | `probes/ready.rs` | `GET /healthz` + `GET /readyz` | `health.go::readyHandler`（L93-182，**一个 handler 挂两个路径**） | **M10-2** |
//! | `probes/realtime.rs` | `GET /health/realtime` | `health_realtime.go`（106 行） | **M10-3** |
//!
//! 账：3 个子文件 = **4 条注册键**（`ready.rs` 一个 handler 挂两条路径）。
//!
//! ## 锚点期**零注册键**（也正是本片 ⑦ 读数只减 1 的原因）
//!
//! 三个子文件现在都是**空** `Router::new()` ⇒ `mount_slice_probes()` 合并进全局 router 之后
//! **注册键集合只少 1 个**（M10-0 同时预删的幽灵占位 `/api/feature-flags`）、**不增**。
//!
//! ## 三条接线纪律（与前几波同款）
//!
//! 1. **不得**在 anchor 期注册 501 占位：这 4 条是**上游键**，占位会让 ⑦ 把它们算成
//!    `implemented_placeholder`，而 ⑨ 会从 `unmounted` 变 `mismatch`（期望 200 / 得到 501）。
//! 2. 同 path+method 重复注册 ⇒ axum 在**启动时 panic**（`docs/15` §9.6.6）。
//! 3. 形态：上游 4 条全是 plain `r.Get("/a/b", h)` ⇒ **只注册无尾斜杠**那一形态
//!    （`docs/64` §1.4 实测 `dual-form required: 0`；补尾斜杠 = `EXTRA_ALIAS` 硬失败，
//!    本波 `slash-alias-allowlist.tsv` 是 0 数据行、没有豁免退路）。
//!
//! ⚠️ 三条键**都在根路径**（`/health`、`/healthz`、`/readyz`、`/health/realtime`）、**不在**
//! `/api` 前缀下 ⇒ 不得塞进任何 `mount_slice_*` 的 `/api` 子树，也不得被既有的 `/api/*`
//! 鉴权提取器拦住（本地 `AuthUser` 提取器按路由挂、不全局 ⇒ 天然满足）。

pub mod live;
pub mod ready;
pub mod realtime;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// ops 探针面聚合 router（anchor 期 = 空 `Router::new()` 的三次 merge）。
pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .merge(live::router(state.clone()))
        .merge(ready::router(state.clone()))
        .merge(realtime::router(state))
}
