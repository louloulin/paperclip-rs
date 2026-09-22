//! `/api/agents*` 端到端测试（M3-5 / LUM-1428）：16 条路由中的 13 条 agent 面 +
//! 3 条 workspace 级聚合。
//!
//! 需要真实 PG：`agent` / `agent_runtime` / `agent_task_queue` /
//! `agent_invocation_target` / `issue_label` / `agent_to_label` 必须已迁移
//! （`contracts/upstream-schema.sql` 那一套，本地 `0001` 的 `agent` 形状不同）。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc:mc@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-http --test agents --features test-util -- --ignored
//! ```
//!
//! `MULTICA_TEST_DATABASE_URL` 未设置 → 每条用例打印跳过并 return（普通
//! `cargo test` 不会红）；**设置了却连不上 / 没建表 → panic**（不许静默假装绿）。
//!
//! 文件布局（R7：单文件 800 行硬上限，门 ⑩ `scripts/file_size_check.py`）：
//! - `agents/support.rs`：连接 / `AppState` / 种子 / 请求小工具
//! - `agents/crud.rs`：CRUD / 归档恢复 / 取消任务 / 任务列表
//! - `agents/labels.rs`：agent↔label 三条
//! - `agents/env.rs`：env 明文 + 审计 + `****` 哨兵
//! - `agents/stats.rs`：三条 workspace 级聚合
//! - `agents/auth.rs`：鉴权 / workspace 解析 / 私密可见性
#![cfg(feature = "test-util")]

mod auth;
mod crud;
mod env;
mod labels;
mod stats;
mod support;
