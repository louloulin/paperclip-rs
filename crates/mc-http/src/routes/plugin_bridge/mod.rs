//! 插件 bridge 聚合：`/api/plugin-bridge/v1/*`（**10 个注册键** = M6-7 的 9 + M6-8 的 1）。
//!
//! ## ⚠️ 本文件由 M6-0 anchor 冻结，M6 后续切片**不得**编辑
//!
//! ## 路由账（`docs/57` §4.1 / §9.2）
//!
//! | 注册键 | 方法 | 上游 | 写者 |
//! | --- | :-: | --- | :-: |
//! | `/api/plugin-bridge/v1/context` | GET | `router.go:103` | M6-7 |
//! | `/api/plugin-bridge/v1/issues/:issue_ref` | GET, PATCH | `router.go:104-105` | M6-7 |
//! | `/api/plugin-bridge/v1/issues/:issue_ref/comments` | GET, POST | `router.go:106-107` | M6-7 |
//! | `/api/plugin-bridge/v1/storage/:scope` | GET | `router.go:108` | M6-7 |
//! | `/api/plugin-bridge/v1/storage/:scope/:key` | GET, PUT, DELETE | `router.go:109-111` | M6-7 |
//! | `/api/plugin-bridge/v1/hooks/:key` | POST | `router.go:1598` | M6-8 |
//!
//! **为什么 bridge 与 `/v1` 是「同一组 handler 挂两次」**：上游 `router.go` 的 103-111 在两个
//! 前缀下各挂一份（`docs/57` §9.2 把 M6 的路由账记成「plugin 安装面 17 + bridge/v1/surface
//! 20」——20 = bridge 10 + `/v1` 9 + surface 1）。本仓把实现
//! 放在 `routes/v1/{context,issues,storage}.rs` 的 `pub(crate)` 函数里，这里只挂 router 切片，
//! 因此投影/授权口径**只有一份**。
//!
//! ⚠️ 与 `/v1` 的差别在**凭据与限流**：bridge 面是 iframe 内的插件调用（回调令牌），
//! 公开面是安装令牌/会话。桥面**不套** `/v1` 的 `policy::apply`（配额档位不同）——
//! M6-7 若需要桥面的层，加在**本文件**的合并点（anchor 冻结的那一层，改这行要走
//! `docs/32` §9 的登记），或按 M6-7 自己的文件（`hooks.rs` 之外的桥面文件）里挂。
//!
//! ⚠️ `:issue_ref` 是不透明引用、`:scope` 是 `workspace`/`user`、`:key` 是存储键
//! （长度有 CHECK，见 `344`）。参数写冒号形态（matchit 0.7 的 `{…}` 是字面量段）。

pub mod context;
pub mod hooks;
pub mod issues;
pub mod storage;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/plugin-bridge/v1/*` 的聚合 router。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(context::router())
        .merge(issues::router())
        .merge(storage::router())
        .merge(hooks::router())
}
