//! M4 anchor scaffold（LUM-1470）：`project` / `project_resource` 领域层 crate —— **占位，无实现**。
//!
//! 归属：M4-1（`docs/42-M4-PLAN.md` §4.1，10 条路由 #26–#35）。`docs/42` §5.1 第 1 项把
//! 本 crate 与 `mc-chat` / `mc-squad` 一次建齐，依赖一次声明到位；此后 M4 各切片**不得**
//! 再新增三方依赖。
//!
//! 上游体量（`f41fae6b`，非测试行数）：`handler/project.go` 962 行 +
//! `handler/project_resource.go` 1061 行 ⇒ 对照门 ⑩ 的 800 行/文件硬上限，本 crate 若需要
//! 领域逻辑，**按 `project` / `project_resource` 两个子域拆文件**（与
//! `mc-repos/src/{project,project_resource}.rs` 同名对齐）。
//!
//! 路由面（`router.go` L2064–L2078，逐字含尾斜杠）：
//!
//! | 方法 | 路径 |
//! | --- | --- |
//! | GET | `/api/projects/search`（**无**尾斜杠） |
//! | GET / POST | `/api/projects/` |
//! | GET / PUT / DELETE | `/api/projects/{id}/` |
//! | GET / POST | `/api/projects/{id}/resources` |
//! | PUT / DELETE | `/api/projects/{id}/resources/{resourceId}` |
//!
//! 实现面主要在 `mc-repos/src/project.rs` + `mc-repos/src/project_resource.rs` +
//! `crates/mc-http/src/routes/projects.rs`（已由本 anchor 接好 `mount_slice_project()`）。
//! 本 crate **只在确实需要纯领域逻辑时才填**；判定不需要时可保持空实现并在 PR 说明
//! （与 M3-0 的 `mc-agent` 同一处置，见 `crates/mc-agent/src/lib.rs` 的文档）。
//!
//! # 不要做什么（anchor 边界）
//!
//! - scaffold 阶段**只有文档注释**：0 类型、0 实现、0 路由、0 SQL。
//! - **不建表、不写迁移**：`project`（`034_projects`）与 `project_resource`
//!   （`065_project_resources`）实测已在 `migrations/upstream/`（`docs/42` §2）。
//! - **不引入本仓自造列**。
//! - 路径参数一律写 `:id` / `:resourceId`（matchit 0.7 把 `{id}` 当字面量段：编译过、恒 404）。
