//! M5 anchor scaffold（LUM-1563）：`/api/autopilots*` 面路由切片聚合 —— **空 router 占位**。
//!
//! 归属：`docs/44-M5-PLAN.md` §1.1（29 条路由 = autopilot 20 + wakeup 8 + webhook 1）与
//! §3.2（写集矩阵）。autopilot 面按**子文件**拆给三个切片，本文件是公共聚合点
//! （anchor 预建，此后**任何切片都不改本文件**）。
//!
//! # 子模块 → 路由 → 切片
//!
//! | 子模块 | 路由（`router.go` L2101–L2123） | 切片 |
//! | --- | --- | --- |
//! | [`list`] | #1-4 `GET /api/autopilots/`(+无斜杠) / `cron-preview` / `usage` / `GET /api/autopilots/:id/`(+无斜杠) | M5-1 |
//! | [`access`] | **非路由**：`autopilotWriteByOwnership`14 / `memberCanWriteAutopilot`70 / `autopilotActingUserID`36 / `requireAutopilotActingMember`35 / `loadAutopilotInWorkspace`25 | M5-1 |
//! | [`dto`] | **非路由**：`autopilotToResponse`37 / `triggerToResponse`50 / `runToResponse`32 / `runToResponseSlim`88 | M5-1 |
//! | [`crud`] | #5-7 `POST /api/autopilots/`(+无斜杠) / `PATCH|DELETE /api/autopilots/:id/`(+无斜杠) | M5-2 |
//! | [`subscribers`] | #8-9 `POST /api/autopilots/:id/collaborators` / `DELETE .../collaborators/:userId` | M5-2 |
//! | [`assignee`] | **非路由**：`validateAutopilotAssigneeForSave`76 / `isValidAutopilotAssigneeType`19（crud 与 trigger 共用） | M5-2 |
//! | [`trigger`] | #10-12 `POST /api/autopilots/:id/triggers` / `PATCH|DELETE /api/autopilots/:id/triggers/:triggerId/`(+无斜杠) | M5-3 |
//! | [`credentials`] | #13-14 `POST .../rotate-webhook-token` / `PUT .../signing-secret` | M5-3 |
//! | [`execution`] | #15-17 `POST /api/autopilots/:id/trigger` / `GET .../runs` / `GET .../runs/:runId` | M5-4 |
//! | [`delivery`] | #18-20 `GET .../deliveries` / `GET .../deliveries/:deliveryId` / `POST .../replay` | M5-4 |
//!
//! `access` / `dto` / `assignee` 是**非路由共享模块**（§6.3 把它们和路由文件一起拆出来，正是为了
//! 让 `crud.rs` 不撞门 ⑩ 的 800 行）⇒ 本文件**不** merge 它们，它们也不含 `router()`。
//!
//! `mount.rs::mount_slice_autopilot()` 已合并本文件的 `router()`，本文件再合并 7 个子 router
//! ⇒ 切片**只需实现自己子文件里的 `router()`**，不必改 `mount.rs` / `routes/mod.rs`。
//!
//! # M0 占位已由本 anchor 预删（切片必读）
//!
//! `mount.rs` 里原来的 `GET|POST /api/autopilots`（**无**尾斜杠形态）M0 占位已由 M5-0 anchor
//! 删除（`docs/44` §3.1）。⇒ 切片注册真路由时不会撞重复注册 panic，但**必须**照上游形态注册：
//!
//! - 上游 `router.go:2101-2123` 是 `Route("/api/autopilots") + Get("/") / Post("/")` ⇒ chi 的
//!   `Mount` 同时服务 `/api/autopilots` 与 `/api/autopilots/`；axum 0.7 / matchit 0.7 **不做
//!   归一化**（少注册一个就是 404，不是 307）⇒ **两个形态都要注册，且方法集合逐字相同**
//!   （规则与全仓对账见 `docs/37-M3-W3C-PREFLIGHT.md` §15.1/§15.3）。
//! - 这条**不再有 allowlist 退路**：`docs/fixtures/slash-alias-allowlist.tsv` 里那 2 行
//!   （`GET|POST /api/autopilots`）已随本 anchor 删除，漏注册形态会被门 ⑦ 的
//!   `slash_alias_audit.py` 直接判红（`MISSING_ALIAS`）。
//! - 反向：`cron-preview` / `usage` / `:id/trigger` / `:id/triggers` / `:id/runs` /
//!   `:id/deliveries` / `collaborators` 都是 `r.Get("/…")` 之类的 plain 子路由，上游只有
//!   **一个**形态 ⇒ **不要**加尾斜杠别名（会被判 `EXTRA_ALIAS` 警告）。
//! - **尾斜杠双形态的实测基线**：⑨/⑦ 本轮实测 M5 域的双形态键 7 个 / 单形态 22 个
//!   （`slash_alias_audit.py --declared`，M4 域是 15 键 **不许照抄**）。
//!
//! 路径参数一律写 `:id` / `:triggerId` / `:runId` / `:deliveryId` / `:userId`
//! （matchit 0.7 把 `{id}` 当**字面量段**：编译过、恒 404，`docs/09` §7.4）。
//! 另外 matchit 里**字面量段优先于参数段** ⇒ `cron-preview` / `usage` 不会被 `:id` 吃掉，
//! 但注册顺序仍按 #1→#4 的语义顺序写，避免日后有人「优化」成通配。

pub mod access;
pub mod assignee;
pub mod credentials;
pub mod crud;
pub mod delivery;
pub mod dto;
pub mod execution;
pub mod list;
pub mod subscribers;
pub mod trigger;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// autopilot 面聚合切片：7 个**路由**子模块各自的 `router()` 在这里合并。
///
/// 子 router 仅声明路由表，不在内部 `with_state` —— 真正的 state 由
/// `apps/mc-server/src/main.rs` 在 `mc_http::routes::router().with_state(state)` 时一次性注入。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(list::router())
        .merge(crud::router())
        .merge(subscribers::router())
        .merge(trigger::router())
        .merge(credentials::router())
        .merge(execution::router())
        .merge(delivery::router())
}
