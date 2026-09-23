//! M4 anchor scaffold（LUM-1470）：`squad` / `squad_member` 领域层 crate —— **占位，无实现**。
//!
//! 归属：M4-2（`docs/42-M4-PLAN.md` §4.1，10 条路由 #36–#45）。`docs/42` §5.1 第 1 项把
//! 本 crate 与 `mc-chat` / `mc-project` 一次建齐，依赖一次声明到位；此后 M4 各切片**不得**
//! 再新增三方依赖。
//!
//! 上游体量（`f41fae6b`，非测试行数）：`handler/squad.go` 1243 行 + `handler/squad_briefing.go`
//! 368 行（briefing 的路由在别处，不属本片）+ `service/squad_no_action.go` 21 行 ⇒ 对照门 ⑩
//! 的 800 行/文件硬上限，本 crate 若需要领域逻辑，**按 `squad` / `member` 两个子域拆文件**。
//!
//! 路由面（`router.go` L2081–L2093，逐字含尾斜杠）：
//!
//! | 方法 | 路径 |
//! | --- | --- |
//! | GET / POST | `/api/squads/` |
//! | GET / PUT / DELETE | `/api/squads/{id}/` |
//! | GET / POST / DELETE | `/api/squads/{id}/members` |
//! | GET | `/api/squads/{id}/members/status` |
//! | PATCH | `/api/squads/{id}/members/role` |
//!
//! 实现面主要在 `mc-repos/src/squad.rs`（`squad` + `squad_member`，上游 `squad.sql` 22 条
//! query）+ `crates/mc-http/src/routes/squads.rs`（已由本 anchor 接好 `mount_slice_squad()`）。
//! 本 crate **只在确实需要纯领域逻辑时才填**；判定不需要时可保持空实现并在 PR 说明
//! （与 M3-0 的 `mc-agent` 同一处置，见 `crates/mc-agent/src/lib.rs` 的文档）。
//!
//! # 不要做什么（anchor 边界）
//!
//! - scaffold 阶段**只有文档注释**：0 类型、0 实现、0 路由、0 SQL。
//! - **不建表、不写迁移**：`squad` 与 `squad_member` 同属 `084_squad.up.sql`，实测已在
//!   `migrations/upstream/`（`docs/42` §2）。
//! - **不引入本仓自造列**。
//! - 路径参数一律写 `:id`（matchit 0.7 把 `{id}` 当字面量段：编译过、恒 404）。
//!
//! # M4-2 填充（LUM-1473）
//!
//! 本 crate 只放了**纯领域逻辑**：[`status`]（member presence 派生，上游 `squad.go`
//! L591–L653 的 `deriveRuntimeAvailability` / `deriveSquadMemberStatus`）。SQL 与 HTTP 形状
//! 分别在 `mc_repos::squad` 与 `mc_http::routes::squads`；本 crate 不碰 DB、不碰 axum，
//! 因此那两段的全部分支可以用单元测试穷举（3×3×2），不必起库。

pub mod status;
