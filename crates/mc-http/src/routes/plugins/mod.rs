//! plugin 面聚合：`/api/workspaces/:id/plugins*`（**17 个注册键** = M6-5 的 13 + M6-6 的 4）
//! 外加 hook job 的粘合（0 路由，见 `hooks_job.rs`）。
//!
//! ## ⚠️ 本文件由 M6-0 anchor 冻结，M6 后续切片**不得**编辑
//!
//! 子路由的合并点在这里；各切片的文件由各自的写者实作。发现格子有遗漏时**不要**直接加到
//! 这里 —— 记到 `docs/32` §9 的文件→写者表，由集成方（M6-10）统一加。
//!
//! ## 路由账（`docs/57` §4.1）
//!
//! | 注册键 | 方法 | 上游 | 写者 |
//! | --- | :-: | --- | :-: |
//! | `/api/workspaces/:id/plugins` | GET, POST | `router.go:1690` / `1736` | M6-5 |
//! | `/api/workspaces/:id/plugins/preview` | POST | `router.go:1735` | M6-5 |
//! | `/api/workspaces/:id/plugins/:installationId` | DELETE | `router.go:1747` | M6-5 |
//! | `/api/workspaces/:id/plugins/:installationId/config` | PUT | `router.go:1744` | M6-5 |
//! | `/api/workspaces/:id/plugins/:installationId/enable` | POST | `router.go:1745` | M6-5 |
//! | `/api/workspaces/:id/plugins/:installationId/disable` | POST | `router.go:1746` | M6-5 |
//! | `/api/workspaces/:id/plugins/:installationId/token` | POST, DELETE | `router.go:1738-1739` | M6-5 |
//! | `/api/workspaces/:id/plugins/packages` | GET, POST | `router.go:1724-1725` | M6-5 |
//! | `/api/workspaces/:id/plugins/packages/local` | POST | `router.go:1726` | M6-5 |
//! | `/api/workspaces/:id/plugins/packages/:packageId` | DELETE | `router.go:1727` | M6-5 |
//! | `/api/workspaces/:id/plugins/:installationId/invocations` | GET | `router.go:1737` | M6-6 |
//! | `/api/workspaces/:id/plugins/:installationId/mcp/:hookKey/tools` | GET, PUT | `router.go:1742-1743` | M6-6 |
//! | `/api/workspaces/:id/plugins/:installationId/surfaces/:surfaceKey/launch` | GET | `router.go:1694` | M6-6 |
//!
//! ⚠️ 上游是 chi，`Route("/api/workspaces/{id}/plugins")` **只**服务带尾斜杠形态的
//! `Get("/")` / `Post("/")` 吗？——本波**不动**这里的形态：这 17 条在
//! `slash_alias_audit.py` 的 M6 清单里**不是**双形态要求（只有 `/api/skills` 与
//! `/api/skills/:id` 那 5 个键是）。切片按 `docs/fixtures/upstream-routes.tsv` 的**逐字路径**注册，
//! 不要自作主张加/去尾斜杠（多加会与 ⑦ 的 `slash_aliases()` 折叠规则打架）。
//!
//! ⚠️ 路径参数必须写 `:id` / `:installationId` / `:hookKey` / `:surfaceKey`（matchit 0.7 把 `{…}`
//! 当**字面量**段：编译通过、恒 404）。

pub mod hooks_job;
pub mod install;
pub mod mcp;
pub mod packages;
pub mod surface_launch;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/workspaces/:id/plugins*` 的聚合 router。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(install::router())
        .merge(packages::router())
        .merge(mcp::router())
        .merge(surface_launch::router())
        .merge(hooks_job::router())
}
