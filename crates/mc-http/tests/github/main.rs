//! `GET /api/github/setup` + `/api/workspaces/{id}/github/*` 端到端测试
//! （M8-1 / LUM-1798，`docs/61-M8-PLAN.md` §4.2 / §6.5 的 M8-1 行）。
//!
//! 覆盖 M8-1 的 **5** 条路由：`setup`（公开回调）+ `connect` / `installations` /
//! `repositories` / `delete`（workspace 面）。全部 `#[ignore]`：需要真库
//! （`MULTICA_TEST_DATABASE_URL`，门 ⑥ 用 `-- --ignored` 拉起）。
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc_lum1798:…@127.0.0.1:5432/mc_lum1798 \
//!   cargo test -p mc-http --test github --features test-util -- --ignored
//! ```
//!
//! 文件布局（门 ⑩ 单文件 800 行硬上限）：
//! - `github/support.rs`：连接 / `AppState` 字面量 / 种子 / GitHub 替身 starter
//! - `github/install.rs`：四条 workspace 路由的「未配置 + 未授权」矩阵 + 离线替身端到端
//! - `github/setup.rs`：公开回调的六个失败分支 + 成功路径（含 pending 消费）
#![cfg(feature = "test-util")]

mod install;
mod setup;
mod support;
