//! M4-1（LUM-1472）：project 面 `/api/projects*` —— 10 条路由（`docs/42-M4-PLAN.md`
//! §4.1 #26–#35）。
//!
//! 上游真值：`server/cmd/server/router.go:2064-2078`（路由表）、
//! `server/internal/handler/project.go`（962 行）、`handler/project_resource.go`（1061 行）、
//! `queries/project.sql`（9 条）+ `queries/project_resource.sql`（10 条）。
//!
//! 路由面（**逐字含/不含尾斜杠**）：
//!
//! | 方法 | 路径 | handler |
//! | --- | --- | --- |
//! | GET | `/api/projects/search`（**无**尾斜杠） | `SearchProjects` |
//! | GET / POST | `/api/projects/`（+ `/api/projects` 别名） | `ListProjects` / `CreateProject` |
//! | GET / PUT / DELETE | `/api/projects/:id/`（+ 无斜杠别名） | `Get` / `Update` / `DeleteProject` |
//! | GET / POST | `/api/projects/:id/resources` | `List` / `CreateProjectResource` |
//! | PUT / DELETE | `/api/projects/:id/resources/:resourceId` | `Update` / `DeleteProjectResource` |
//!
//! # 尾斜杠双形态（M4-0 anchor 实测，硬约束）
//!
//! 上游 `router.go:2067-2068` 是 `Route("/api/projects") + Get("/") / Post("/")`，走 chi 的
//! `Mount` ⇒ 同时服务 `/api/projects` 与 `/api/projects/`；axum 0.7 / matchit 0.7 不做归一化
//! （少注册一个形态是 **404 而不是 307**，`docs/37` §14.2）。`docs/fixtures/slash-alias-allowlist.tsv`
//! 里那 6 行已随 M4-0 anchor 删除 ⇒ **漏注册形态会被门 ⑦ 的 `slash_alias_audit.py` 判
//! `MISSING_ALIAS`，没有退路**。因此集合与 item root 一律注册两个形态，且两个形态的方法集合
//! 逐字相同（本片 5 个双形态键，见 `docs/fixtures/m4-declared-routes.tsv` L53–L62）：
//!
//! - `GET|POST /api/projects` + `GET|POST /api/projects/`
//! - `GET|PUT|DELETE /api/projects/:id` + `GET|PUT|DELETE /api/projects/:id/`
//!
//! 反向：`/api/projects/search` 与 `:id/resources[...]` 是 plain 子路由（上游
//! `r.Get("/search")` / `r.Route("/{id}/resources")` 之外的普通注册）⇒ **只注册一个形态**，
//! 加尾斜杠别名会被判 `EXTRA_ALIAS` 警告。路径参数写 `:id` / `:resourceId`
//! （matchit 0.7 把 `{id}` 当字面量段：编译过、恒 404）。
//!
//! # 仓储与领域层
//!
//! - 仓储：`mc_repos::project`（集合 / 单体 CRUD + 搜索 + 事务级联删除）、
//!   `mc_repos::project_resource`（资源子集合 CRUD）。**本片不写迁移**：`project` 已在
//!   `034_projects.up.sql`（`035` 加 `priority`、`166` 加 `start_date`/`due_date`）、
//!   `project_resource` 已在 `065_project_resources.up.sql`。
//! - `mc-project` crate 保持为空：没有任何 crate 依赖它，接线要动 `Cargo.toml` +
//!   `Cargo.lock`，而本片门禁是 `--locked` 构建（`docs/42` §6.3「M4 切片不得新增三方依赖」）。
//!
//! # 与上游的有意偏离（逐条记账）
//!
//! 1. **workspace 来源**：上游从 session / task token 取；本仓沿用 M1 的 dev-mode
//!    提取器（`x-workspace-id` / `?workspace_id=`，支持 slug），因此每个端点都显式要求
//!    调用方是 workspace 成员（`invitations::require_workspace_member`，非成员 → 404
//!    `workspace`）。上游只有 `DeleteProject` 有角色门（owner/admin）——本片同样：
//!    其余端点只要求成员身份。
//! 2. **错误体形状**：本仓统一 `{"error":{"code","message"}}`（上游是扁平 `{"error": "…"}`）。
//!    例外：worktree 能力门的 422 逐字复刻上游的扁平体（含 `code` / `current_version` /
//!    `min_version` / `daemon_id`），客户端按 `code` 分支是上游明写的意图。
//! 3. **实时事件**：上游在 6 个写路径 publish `project.*` / `project_resource.*` 事件；
//!    本仓 M4 未接 realtime（跨片缺口，`docs/39` §4.8），本片不发明事件名。
//! 4. **`POST /api/projects` 的捆绑创建**：上游在事务里写 project + `resources[]`；
//!    本片落在 `ProjectRepo::create_with_resources`（同事务），逐条预校验 + 逐 daemon
//!    去重与 worktree 门，均在开事务之前。
//!
//! # 文件布局（R7：单文件 800 行硬上限，门 ⑩ `scripts/file_size_check.py`）
//!
//! - `mod.rs`（本文件）：模块文档 + `pub fn router()`（**所有 `.route(...)` 字面量都留在这里**，
//!   门 ⑦ 的两条静态抽取脚本按 `.route("<literal>"` 扫 `crates/mc-http/src/**/*.rs`）
//! - `helpers.rs`：workspace / 成员 / 错误映射 / 请求体读取 / 枚举校验
//! - `dto.rs`：响应结构（`ProjectResponse` / `ProjectResourceResponse` / 搜索投影）+ 请求体
//! - `resource_ref.rs`：`resource_ref` 归一化与校验（`github_repo` / `local_directory`）+
//!   `local_directory` 的 worktree 能力门（422 扁平体）
//! - `crud.rs`：`/api/projects` 集合与单体（list / get / create / update / delete）
//! - `resources.rs`：`/api/projects/:id/resources*`（list / create / update / delete）
//! - `search.rs`：`/api/projects/search`
//!
//! ⚠️ 契约证据缺口（`docs/42` §6.1）：`contracts/golden/projects/` 的 3 条 fixture 不是
//! project 契约测试（只把 `POST /api/projects` 当装置），⑨ 里全 `unevaluable`
//! ⇒ 本片**没有 ⑨ 契约兜底**，以 `router.go` / handler / `project.sql` 源码为真值。

mod crud;
mod dto;
mod helpers;
mod resource_ref;
mod resources;
mod search;

use axum::routing::{get, put};
use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/projects*` 路由（10 条上游路由 / 14 个注册键，含 5 个尾斜杠别名）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // ---- 搜索（plain 子路由：上游 `r.Get("/search")`，只有无斜杠一个形态）----
        .route("/api/projects/search", get(search::search_projects))
        // ---- 集合（双形态：上游 `Route("/api/projects")` + `Get("/")/Post("/")` 走 chi Mount）----
        .route(
            "/api/projects",
            get(crud::list_projects).post(crud::create_project),
        )
        .route(
            "/api/projects/",
            get(crud::list_projects).post(crud::create_project),
        )
        // ---- 单体（双形态，方法集合与无斜杠形态逐字相同）----
        .route(
            "/api/projects/:id",
            get(crud::get_project)
                .put(crud::update_project)
                .delete(crud::delete_project),
        )
        .route(
            "/api/projects/:id/",
            get(crud::get_project)
                .put(crud::update_project)
                .delete(crud::delete_project),
        )
        // ---- 资源子集合（上游 `r.Route("/{id}/resources")` 下的 plain 注册：单形态）----
        .route(
            "/api/projects/:id/resources",
            get(resources::list_resources).post(resources::create_resource),
        )
        .route(
            "/api/projects/:id/resources/:resourceId",
            put(resources::update_resource).delete(resources::delete_resource),
        )
}
