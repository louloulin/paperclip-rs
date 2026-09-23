//! `/api/autopilots*` 端到端测试（M5-1 / LUM-1564）。
//!
//! 需要真实 PG（`autopilot*` 全部是上游形状，`contracts/upstream-schema.sql`）。
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc_lum1563:…@127.0.0.1:5432/multica_lum1563 \
//!   cargo test -p mc-http --test autopilots --features test-util -- --ignored
//! ```
//!
//! 文件布局（R7：单文件 800 行硬上限，门 ⑩）：
//! - `autopilots/support.rs`：连接 / `AppState` / 种子 / 请求小工具
//! - `autopilots/read.rs`：列表 + 详情（派生列、`can_write` 三态、凭据抹除）
//! - `autopilots/auth.rs`：鉴权与可见性（401 / 400 / 404 三档）
//! - `autopilots/cron.rs`：`cron-preview`（含扁平错误体与 `Z` 结尾的秒精度）
//! - `autopilots/usage.rs`：`usage` 的 off 形态 + 装 stub 平面后的 observe 形态
//! - `autopilots/crud.rs`：写面 CRUD（create / 三态 patch / 规则版本 / 删除 = 归档，M5-2）
//! - `autopilots/crud_access.rs`：写面并发、指派校验、鉴权与协作者（M5-2）
//! - `autopilots/crud_support.rs`：上面两个文件的共享夹具（种子 / 探针）
#![cfg(feature = "test-util")]

mod auth;
mod cron;
mod crud;
mod crud_access;
mod crud_support;
mod read;
mod support;
mod usage;
