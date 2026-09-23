//! 公开 Action API（`/v1/*`）聚合：**9 个注册键**。
//!
//! ## ⚠️ 本文件由 M6-0 anchor 冻结，M6 后续切片**不得**编辑
//!
//! 合并点与**中间件挂载点**都在这里。`docs/57` §3.2 把 `routes/v1/*.rs` 判给 M6-7；本 anchor
//! 只把「三个子 router 的合并 + 一层 policy」这条骨架固定下来，子文件各自实作。
//!
//! ## 路由账（`docs/57` §4.1；上游 `router.go:103-111`）
//!
//! | 注册键 | 方法 | 写者 |
//! | --- | :-: | :-: |
//! | `/v1/context` | GET | M6-7 |
//! | `/v1/issues/:issue_ref` | GET, PATCH | M6-7 |
//! | `/v1/issues/:issue_ref/comments` | GET, POST | M6-7 |
//! | `/v1/storage/:scope` | GET | M6-7 |
//! | `/v1/storage/:scope/:key` | GET, PUT, DELETE | M6-7 |
//!
//! ⚠️ 同一组 handler 在 `/api/plugin-bridge/v1/*` **再挂一次**（上游就是这样：`router.go` 的
//! 103-111 出现在两个前缀下，`docs/57` §9.2 的「17 + bridge 20」）。本仓的落点是
//! `routes/plugin_bridge/*`（M6-7 同一个写者），**不要**在这里再挂一份。
//!
//! ⚠️ `:issue_ref` 是**不透明引用**（不是 uuid，可能是 `owner/repo#123` 形态）⇒ 路径段按
//! `String` 收，别用 `Uuid` 提取器（会 400）。参数写 `:issue_ref`（matchit 0.7 的 `{…}` 是字面量）。
//!
//! ## ⚠️ 中间件只在**合并点之后**加一次
//!
//! `tower_governor` 的限流桶是**按 router 实例分片**的：三个子 router 各加一层 = 3 倍配额。
//! 所以 `policy::apply(...)` 必须包在 `.merge(...)` 的**外面**（就是下面这个形状），
//! 子文件**不得**自己 `.layer(...)`。

pub mod context;
pub mod issues;
pub mod policy;
pub mod storage;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/v1/*` 的聚合 router：合并三个子面，再把 policy 层加在合并点之后。
pub fn router() -> Router<Arc<AppState>> {
    policy::apply(
        Router::new()
            .merge(context::router())
            .merge(issues::router())
            .merge(storage::router()),
    )
}
