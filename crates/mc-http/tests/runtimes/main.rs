//! `/api/runtimes*` + `/api/workspaces/{id}/runtime-profiles*` 端到端测试
//! （M3-4 / LUM-1427：15 条路由 + 3 条尾斜杠别名）。
//!
//! 需要真实 PG：`agent_runtime` / `runtime_profile` / `agent` / `agent_task_queue` /
//! `task_usage` / `task_usage_hourly` 必须已迁移（`migrations/` 的 upstream 那一套）。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc:mc@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-http --test runtimes --features test-util -- --ignored
//! ```
//!
//! `MULTICA_TEST_DATABASE_URL` 未设置 → 每条用例打印跳过并 return（普通
//! `cargo test` 不会红）；**设置了却连不上 / 没建表 → panic**（不许静默假装绿）。
//!
//! 文件布局（R7：单文件 800 行硬上限，门 ⑩ `scripts/file_size_check.py`）：
//! - `runtimes/support.rs`：连接 / `AppState` / 种子 / 请求小工具
//! - `runtimes/profiles.rs`：6 条 runtime-profile 路由
//! - `runtimes/ledger.rs`：9 条 runtimes 台账路由（含两条删除路径与别名）
//! - `runtimes/usage.rs`：usage / by-agent / by-hour / activity
#![cfg(feature = "test-util")]

mod ledger;
mod profiles;
mod support;
mod usage;
