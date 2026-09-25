//! `POST /api/webhooks/github` + `GET /api/issues/:id/pull-requests` 端到端测试
//! （M8-4 / `LUM-1801`，`docs/61-M8-PLAN.md` §4.2 / §6.5 的 M8-4 行）。
//!
//! 覆盖 M8-4 的 **2** 条路由：公开的入站 webhook + issue ↔ PR 读面。全部 `#[ignore]`：
//! 需要真库（`MULTICA_TEST_DATABASE_URL`，门 ⑥ 用 `-- --ignored` 拉起）。
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc_lum1801:…@127.0.0.1:5432/multica_lum1801 \
//!   cargo test -p mc-http --test github_webhook --features test-util -- --ignored
//! ```
//!
//! **离线替身方案**（`docs/61` §4.2 的 M8-4 格）：GitHub 侧不需要本地 HTTP 服务端 —— 这条
//! 入站链的方向是「GitHub 打我们」，所以替身是**发帧的那一侧**：用例构造**真实
//! HMAC-SHA256 头**的帧（[`sign_webhook_body`] 与被测代码共用同一份 HMAC 实现），断言链 =
//! webhook → PR 行 → issue 关联 → 自动关闭 → 快照入队，**中间零 mock**（业务路径一行都不替）。
//!
//! 文件布局（门 ⑩ 单文件 800 行硬上限）：
//! - `github_webhook/support.rs`：连接 / `AppState` 字面量 / 种子 / 真实帧构造 / 记录型端口
//! - `github_webhook/webhook.rs`：三族事件 + 幂等 + 两个反例（验签失败 / 缺密钥）
//! - `github_webhook/issue_pr.rs`：读面（卡片形状 + 越权 + 「路由仍在」）
#![cfg(feature = "test-util")]

mod ci;
mod issue_pr;
mod support;
mod webhook;
