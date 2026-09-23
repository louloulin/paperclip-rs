//! M4 anchor scaffold（LUM-1470）：project 面 `/api/projects*` 切片 —— **空 router 占位**。
//!
//! 归属：M4-1（`docs/42-M4-PLAN.md` §4.1，10 条路由 #26–#35）。本文件已由
//! `mount.rs::mount_slice_project()` 接好，切片只需在此实现 `router()`，**无需改动**
//! `mount.rs` / `routes/mod.rs`。
//!
//! 路由面（`router.go` L2064–L2078，**逐字含/不含尾斜杠**）：
//!
//! | 方法 | 路径 | handler |
//! | --- | --- | --- |
//! | GET | `/api/projects/search`（**无**尾斜杠） | `SearchProjects` |
//! | GET / POST | `/api/projects/`（+ `/api/projects` 别名） | `ListProjects` / `CreateProject` |
//! | GET / PUT / DELETE | `/api/projects/:id/`（+ 无斜杠别名） | `Get` / `Update` / `DeleteProject` |
//! | GET / POST | `/api/projects/:id/resources` | `List` / `CreateProjectResource` |
//! | PUT / DELETE | `/api/projects/:id/resources/:resourceId` | `Update` / `DeleteProjectResource` |
//!
//! 仓储面：`mc_repos::project` + `mc_repos::project_resource`（anchor 已预置 stub + `pub mod`）；
//! 领域逻辑（若需要）放 `mc-project` crate（anchor 已建空 crate + 预声明依赖）。
//!
//! # M0 占位已由本 anchor 预删（切片必读）
//!
//! `mount.rs` 里原来的 `GET|POST /api/projects`（无尾斜杠）M0 占位已由 M4-0 anchor 删除
//! （`docs/42` §5.2）。⇒ 真路由注册时不会撞重复注册 panic，但**必须**照上游形态注册：
//! 上游 `router.go:2067-2068` 是 `Route("/api/projects") + Get("/") / Post("/")` ⇒ chi 的
//! `Mount` 同时服务 `/api/projects` 与 `/api/projects/`，axum 0.7 不做归一化
//! ⇒ **两个形态都要注册，且方法集合逐字相同**（同理 `:id/`）。这条**不再有 allowlist 退路**
//! —— `docs/fixtures/slash-alias-allowlist.tsv` 里那 6 行已随本 anchor 删除，漏注册形态会被
//! 门 ⑦ 的 `slash_alias_audit.py` 判 `MISSING_ALIAS`。
//!
//! 反向：`/api/projects/search` 与 `:id/resources[...]` 是 plain 子路由 ⇒ 只有**一个**形态，
//! 不要加尾斜杠别名（`EXTRA_ALIAS` 警告）。路径参数写 `:id` / `:resourceId`
//! （matchit 0.7 把 `{id}` 当字面量段：编译过、恒 404）。
//!
//! ⚠️ 契约证据缺口（`docs/42` §6.1）：`contracts/golden/projects/` 的 3 条 fixture 不是
//! project 契约测试（只把 `POST /api/projects` 当装置），且 ⑨ 里全 unevaluable
//! ⇒ 本片**没有 ⑨ 契约兜底**，以 `router.go` / `project.sql` / handler 源码为真值。
//!
//! 若本文件逼近门 ⑩ 的 800 行硬上限（上游 handler 合计 2023 行），拆成
//! `routes/projects/*`（`docs/42` §4.2 已预判，照 `routes/agents/*` 写法）；
//! **新文件不得进 `scripts/file_size_baseline.tsv`**。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M4-1 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
