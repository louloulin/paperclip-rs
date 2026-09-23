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
#![cfg(feature = "test-util")]

mod auth;
mod cron;
mod read;
mod support;
mod usage;
