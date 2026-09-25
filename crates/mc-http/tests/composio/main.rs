//! composio 面（M8-6 / `LUM-1803`）的端到端测试入口。
//!
//! 覆盖 5 条路由：`connect/init` + `toolkits` + `connections` + `connections/{id}`（会话级，
//! Auth 组内）+ 公开回调。全部 `#[ignore]`：需要真库（门 ⑥ 用 `-- --ignored` 拉起）。
//!
//! 离线替身的接缝 = `set_composio_api_base`（进程级，**只注入一次**，行为按请求的
//! `x-api-key` 分派 ⇒ 用例之间零竞态，见 `support.rs` 的文件头）。
//!
//! 文件布局（门 ⑩ 单文件 800 行硬上限）：
//! - `composio/support.rs`：连接 / `AppState` 字面量 / 种子 / 替身（按 key 分派）
//! - `composio/gating.rs`：4 条会话路由的「未配置 × 匿名」矩阵 + 公开回调的 401
//! - `composio/callback.rs`：公开回调的四态 + 幂等 / 重放 / 三族失败
//! - `composio/flows.rs`：connect → callback → 落库 → toolkits → 断开 + 会话/overlay
#![cfg(feature = "test-util")]

mod callback;
mod flows;
mod gating;
mod support;
