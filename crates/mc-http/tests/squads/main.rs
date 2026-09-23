//! `/api/squads*` 端到端测试（M4-2 / LUM-1473：10 条路由 + 4 条尾斜杠别名）。
//!
//! 需要真实 PG：`squad` / `squad_member` / `agent` / `agent_runtime` /
//! `agent_task_queue` / `agent_invocation_target` / `issue` / `autopilot` /
//! `workspace` / `member` 必须已迁移（`migrations/` 的 upstream 那一套）。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-http --test squads --features test-util -- --ignored
//! ```
//!
//! `MULTICA_TEST_DATABASE_URL` 未设置 → 每条用例直接 return（普通 `cargo test`
//! 不会红）；**设置了却连不上 → panic**（不许静默假装绿）。
//!
//! 为什么不用 M4-0b 的 golden fixture：本切片交付时 `contracts/golden/` 里还没有
//! squads 面（M4-0b 未产出），按 `docs/42` §6.4 的口径改用这组 axum 级集成测试自证，
//! 并在交付评论里记账。
//!
//! 文件布局（R7：单文件 800 行硬上限，门 ⑩ `scripts/file_size_check.py`）：
//! - `squads/support.rs`：连接 / `AppState` / 种子 / 请求小工具
//! - `squads/crud.rs`：`/api/squads` 与 `/api/squads/:id` 的增删改查 + 别名形态
//! - `squads/members.rs`：成员 CRUD 与成员状态派生（5 个桶 + 人类成员）
#![cfg(feature = "test-util")]

mod crud;
mod members;
mod support;
