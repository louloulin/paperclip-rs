//! M4 anchor scaffold（LUM-1470）：squad 面 `/api/squads*` 切片 —— **空 router 占位**。
//!
//! 归属：M4-2（`docs/42-M4-PLAN.md` §4.1，10 条路由 #36–#45）。本文件已由
//! `mount.rs::mount_slice_squad()` 接好，切片只需在此实现 `router()`，**无需改动**
//! `mount.rs` / `routes/mod.rs`。
//!
//! 路由面（`router.go` L2081–L2093，**逐字含尾斜杠**）：
//!
//! | 方法 | 路径 | handler |
//! | --- | --- | --- |
//! | GET / POST | `/api/squads/`（+ `/api/squads` 别名） | `ListSquads` / `CreateSquad` |
//! | GET / PUT / DELETE | `/api/squads/:id/`（+ 无斜杠别名） | `Get` / `Update` / `DeleteSquad` |
//! | GET / POST / DELETE | `/api/squads/:id/members` | `List` / `Add` / `RemoveSquadMember` |
//! | GET | `/api/squads/:id/members/status` | `ListSquadMemberStatus` |
//! | PATCH | `/api/squads/:id/members/role` | `UpdateSquadMemberRole` |
//!
//! 仓储面：`mc_repos::squad`（`squad` + `squad_member`，上游 `squad.sql` 22 条 query；
//! anchor 已预置 stub + `pub mod`）；领域逻辑（若需要）放 `mc-squad` crate
//! （anchor 已建空 crate + 预声明依赖）。
//!
//! # M0 占位已由本 anchor 预删（切片必读）
//!
//! `mount.rs` 里原来的 `GET|POST /api/squads`（无尾斜杠）M0 占位已由 M4-0 anchor 删除
//! （`docs/42` §5.2）。⇒ 真路由注册时不会撞重复注册 panic，但**必须**照上游形态注册：
//! 上游 `router.go:2082-2083` 是 `Route("/api/squads") + Get("/") / Post("/")` ⇒ chi 的
//! `Mount` 同时服务 `/api/squads` 与 `/api/squads/`，axum 0.7 不做归一化
//! ⇒ **两个形态都要注册，且方法集合逐字相同**（同理 `:id/`）。这条**不再有 allowlist 退路**
//! —— `docs/fixtures/slash-alias-allowlist.tsv` 里那 6 行已随本 anchor 删除，漏注册形态会被
//! 门 ⑦ 的 `slash_alias_audit.py` 判 `MISSING_ALIAS`。
//!
//! 反向：`:id/members[...]` 是 plain 子路由 ⇒ 只有**一个**形态，不要加尾斜杠别名
//! （`EXTRA_ALIAS` 警告）。注意 `DELETE /api/squads/:id/members` 在**无 body** 的语义下
//! 取 `:memberId`（query / body）—— 照上游 handler 逐条对齐，不要自造路径段。
//! 路径参数写 `:id`（matchit 0.7 把 `{id}` 当字面量段：编译过、恒 404）。
//!
//! ⚠️ 本片查询面跨到 M3 域的表（`agent` / `agent_runtime` / `agent_task_queue`），一律**只读**；
//! 状态取值对齐 `mc_task::status::TaskStatus` 与上游 CHECK（本仓
//! `migrations/0001_init.up.sql:230` 的 CHECK 是错的，不能当契约）。
//!
//! 若本文件逼近门 ⑩ 的 800 行硬上限（上游 `squad.go` 1243 行），拆成 `routes/squads/*`
//! （`docs/42` §4.2 已预判，照 `routes/agents/*` 写法）；**新文件不得进
//! `scripts/file_size_baseline.tsv`**。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M4-2 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
