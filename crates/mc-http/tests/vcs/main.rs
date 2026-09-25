//! `POST /api/webhooks/vcs/{connectionId}` + `/api/workspaces/{id}/vcs/*` 端到端测试
//! （M8-2 / `LUM-1799`，`docs/61-M8-PLAN.md` §4.2 / §6.5 的 M8-2 行）。
//!
//! 覆盖 M8-2 的 **5** 条路由：`connections`（GET/POST）+ `rotate-webhook` + `delete` +
//! 公开的 `webhooks/vcs/:connectionId`。全部 `#[ignore]`：需要真库
//! （`MULTICA_TEST_DATABASE_URL`，门 ⑥ 用 `-- --ignored` 拉起）。
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc_lum1799:…@127.0.0.1:5432/mc_lum1799 \
//!   cargo test -p mc-http --test vcs --features test-util -- --ignored
//! ```
//!
//! 文件布局（门 ⑩ 单文件 800 行硬上限）：
//! - `vcs/support.rs`：连接 / `AppState` 字面量 / 种子 / 两个离线实例替身
//! - `vcs/connections.rs`：四条 workspace 路由的「产品边界 × 未配置 × 未授权」矩阵 + 出站 e2e
//! - `vcs/webhook.rs`：三种签名方案的正反例、失败阶梯、镜像的幂等与单调
#![cfg(feature = "test-util")]

mod connections;
mod support;
mod webhook;
