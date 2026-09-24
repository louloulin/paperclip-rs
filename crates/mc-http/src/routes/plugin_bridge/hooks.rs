//! bridge 面 hook 回调：`POST /api/plugin-bridge/v1/hooks/:key`（**1 个注册键**，M6-8 的**唯一**路由）。
//!
//! - **写者**：M6-8（`docs/57` §3.2；`docs/57` §4.1 给 M6-8 记的 1 条路由就是它）。
//! - **上游**：`router.go:1598` → `handler.InvokePluginHook`（`internal/handler/plugin_hook.go`）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/plugin-bridge/v1/hooks/:key` | POST | `router.go:1598` |
//!
//! ## 为什么落在这里（而不是 `routes/plugins/hooks_job.rs`）
//!
//! `docs/57` §3.2 把 M6-8 的这条路由挂在 `routes/plugins/hooks_job.rs` 名下；M6-0 anchor 按
//! **路径前缀归位**把它放进 `plugin_bridge/hooks.rs`（同一份文件的路径前缀就是路由前缀，
//! 更不容易漏挂），`routes/plugins/hooks_job.rs` 因此保留为 0 路由的 **job 粘合 + 引擎**落点。
//! 注册键总数不变（57/57）；归位与「实现在 `routes::plugins::hooks_job`」两处差异都在
//! `docs/32` §9.10 登记。
//!
//! ## 语义（上游 `InvokePluginHook`，逐条）
//!
//! 1. **凭据是会话**（`docs/57` §2.3 的「会话中继面」）：`policy::resolve_caller(state, headers, "")`
//!    —— 安装来自 `x-multica-plugin-installation` 头，成员身份按**安装行**的 workspace 判定。
//!    插件令牌（`mpi_` / `mpc_`）在桥面**不认**；`policy::apply_bridge` 已经先挡了一层。
//! 2. **必须有真人**（`actor.requireMember`）：插件自己的服务器调这个端点 = 求宿主回调自己 =
//!    一个没有人的循环，`.map_err` 折成 403 `member_required`。
//! 3. **触发者只收 `ui` / `manual`**：`event` 由宿主派发、`agent` 走 MCP；从浏览器收下它们等于
//!    让客户端挑一个本该带别种身份的调用点 ⇒ 400 `trigger must be ui or manual`。
//! 4. `:key` 是**回调标识**不是安装 id（⚠️ 不要用 `Uuid` 提取器强解）；未知 key ⇒ 404。
//! 5. 给了 `issue_id` 就必须过第三步授权（`plugin_issue_for_caller`）：范围外的 issue 是 404。
//! 6. **出站签名与四个头**在 `routes::plugins::hooks_job` 的引擎里（`invoke_hook`）—— 桥面只挂载，
//!    Job、`ui`/`manual` 两条调用点共用同一份实现，所以「限流 / 熔断 / `net:` 目的地检查 / 调用记录」
//!    只有一处。
//!
//! **状态：M6-8 已落地。**
//!
//! 行预算（门 ⑩）：本文件 ≤120 行。

use axum::routing::post;
use axum::Router;
use std::sync::Arc;

use crate::routes::plugins::hooks_job::invoke_bridge_hook;
use crate::routes::v1::policy;
use crate::state::AppState;

/// `POST /api/plugin-bridge/v1/hooks/:key`（M6-8 落地）。
///
/// ⚠️ 参数写**冒号形态**（`matchit` 0.7 把 `{…}` 当**字面量**段：编译通过、恒 404）；
/// `policy::apply_bridge` 是桥面的会话信任边界层（`hooks.rs` 自己那一层，与
/// `context.rs` / `issues.rs` / `storage.rs` 同款）。
pub fn router() -> Router<Arc<AppState>> {
    policy::apply_bridge(
        Router::new().route("/api/plugin-bridge/v1/hooks/:key", post(invoke_bridge_hook)),
    )
}
