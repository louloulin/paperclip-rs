//! 渠道面（slack）的四条路由端到端测试（M7-4 / `LUM-1769`）。
//!
//! 覆盖 `docs/60-M7-PLAN.md` §1.1 里属于 M7-4 的 **4** 条注册键：
//! `GET|DELETE /api/workspaces/{id}/slack/installations[/{installationId}]`、
//! `POST …/slack/install/byo`、`POST /api/slack/binding/redeem`。
//!
//! 全部 `#[ignore]`：需要真库（`MULTICA_TEST_DATABASE_URL`，门 ⑥ 用 `-- --ignored` 拉起）。
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://… bash scripts/gates.sh --with-db
//! ```
//!
//! 文件布局（门 ⑩ 单文件 800 行硬上限）：
//! - `channels/support.rs`：连接 / `AppState` 字面量 / 种子 / Slack 替身 starter / 请求
//! - `channels/slack.rs`：四条路由的「未配置 + 未授权」矩阵 + BYO 装/撤 + 绑定兑换幂等
#![cfg(feature = "test-util")]

mod slack;
mod support;
