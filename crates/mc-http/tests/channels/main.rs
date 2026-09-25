//! 渠道面（slack / telegram）的端到端测试。
//!
//! `docs/60-M7-PLAN.md` §1.1 的 24 条注册键里，已落地平台各自的 4 条在这里各有一份文件：
//!
//! - `channels/slack.rs`：**M7-4**（`LUM-1769`）—— `GET|DELETE /api/workspaces/{id}/slack/installations[/{installationId}]`、
//!   `POST …/slack/install/byo`、`POST /api/slack/binding/redeem`；
//! - `channels/telegram.rs`：**M7-5**（`LUM-1770`）—— `GET|DELETE …/telegram/installations[/…]`、
//!   `POST …/telegram/install`、`POST /api/telegram/binding/redeem`。
//!
//! 全部 `#[ignore]`：需要真库（`MULTICA_TEST_DATABASE_URL`，门 ⑥ 用 `-- --ignored` 拉起）。
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://… bash scripts/gates.sh --with-db
//! ```
//!
//! 文件布局（门 ⑩ 单文件 800 行硬上限）：
//! - `channels/support.rs`：连接 / `AppState` 字面量 / 种子 / 请求 / 替身 starter
//! - `channels/slack.rs`：Slack 四条路由的「未配置 + 未授权」矩阵 + BYO 装/撤 + 绑定兑换幂等
//! - `channels/telegram.rs`：Telegram 四条路由的同名矩阵 + Bot API 替身
//! - `channels/telegram_round_trip.rs`：**M7-6**（`LUM-1771`）—— telegram 的端到端收发回路
//!   （替身造帧 → 真入站 → 真 DB → 真出站 → 帧回替身，`docs/60` §4.2 的渠道门禁证据）
#![cfg(feature = "test-util")]

mod slack;
mod support;
mod telegram;
mod telegram_round_trip;
