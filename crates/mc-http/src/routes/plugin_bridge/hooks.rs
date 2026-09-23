//! bridge 面 hook 回调：`POST /api/plugin-bridge/v1/hooks/:key`（**1 个注册键**，M6-8 的**唯一**路由）。
//!
//! - **写者**：M6-8（`docs/57` §3.2；`docs/57` §4.1 给 M6-8 记的 1 条路由就是它）。
//! - **上游**：`router.go:1598`。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/plugin-bridge/v1/hooks/:key` | POST | `router.go:1598` |
//!
//! ## 为什么落在这里（而不是 `routes/plugins/hooks_job.rs`）
//!
//! `docs/57` §3.2 把 M6-8 的这条路由挂在 `routes/plugins/hooks_job.rs` 名下；本 anchor 按
//! **路径前缀归位**把它放进 `plugin_bridge/hooks.rs`（同一份文件的路径前缀就是路由前缀，
//! 更不容易漏挂），`routes/plugins/hooks_job.rs` 因此保留为 0 路由的 **job 粘合**落点。
//! 注册键总数不变（57/57）；差异已登记 `docs/32` §9 的文件→写者表。
//!
//! ## 语义（M6-8 的验收靠**端到端行为**，`docs/57` §9.3）
//!
//! 1. **签名校验**：请求带 hook 签名头，密钥由部署密钥派生（`mc_plugin_host::credentials`）；
//!    校验失败 ⇒ 401，且**不落** `plugin_invocation` 的 `status='ok'` 行。⚠️ 密钥未配置
//!    （`state.plugin_key()` 为 `None`）时按上游口径 **fail-closed**（503 `plugin_disabled`），
//!    **不得**跳校验。
//! 2. **`hook_key` 必须属于该安装的 manifest**（`contributes.hooks`，读
//!    `mc_plugin_host::manifest`）：未知 key ⇒ 404。
//! 3. 每次调用落一行 `plugin_invocation`（`trigger='event'`，`attempt` 记重试，
//!    `latency_ms` 记耗时，`error` ≤500 字符）—— 写侧在 `mc_repos::plugin::hook`（M6-8 同写者）。
//! 4. `:key` 是**回调标识**不是安装 id：它标识的是「哪条 hook 回调」，安装的解析走该标识
//!    （⚠️ 不要用 `Uuid` 提取器强解）。
//! - **不做什么**：不做定时触发（那是 `hooks_job.rs` 的 job）、不做 remote MCP 调用
//!   （`mc_mcp`，M6-6 的运行时面）。
//!
//! **状态：M6-8 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 320 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `POST /api/plugin-bridge/v1/hooks/:key`（M6-8 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
