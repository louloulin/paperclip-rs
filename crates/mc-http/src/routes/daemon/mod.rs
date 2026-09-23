//! M3-7（LUM-1438）：daemon 面 `/api/daemon*` + ws 服务端。
//!
//! 按域拆文件，每个 ≤800 行（⑩ 门）：`claims`（claim 批）/ `tasks`（任务生命周期）/
//! `lifecycle`（注册、心跳、下线、ws 升级）/ `scope`（daemon 鉴权提取器）/
//! `skills`（skill bundle 打包 + 本地导入口径）/ `requests`（`*/result` 上报面）/
//! `gc`（回收探针）/ `dto`（请求响应类型）。
//!
//! 本文件只做聚合与 `router()`，外加 [`install_ws_handlers`]（把 rpc / heartbeat
//! 两个 handler 装进 `mc-ws` hub）；`mount.rs::mount_slice_daemon(state)` 已接好本模块。
//!
//! 注意（M1-D 实测踩过的坑）：axum 0.7（matchit 0.7）路径参数必须写 `:id`，不写 `{id}`。

pub mod claims;
pub mod dto;
pub mod gc;
pub mod lifecycle;
pub mod messages;
pub mod requests;
pub mod scope;
pub mod skills;
pub mod tasks;
mod ws;

use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// daemon 面路由表（`docs/16` §6.1 表 A 的 36 条）。
///
/// 用户面那 8 条 `POST/GET /api/runtimes/:runtimeId/…`（表 B）在
/// `routes::runtimes::async_requests`，与这里的 4 条 `*/result` 共用同一个
/// [`crate::daemon_requests::RequestStore`]。daemon 面**不**注册 `/api/runtimes/*`。
#[allow(clippy::too_many_lines)] // 36 条路由的挂载表：拆开反而看不出「一条都不少」
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // ---- 生命周期与身份（表 A 1–7） ------------------------------------
        .route("/api/daemon/register", post(lifecycle::register))
        .route("/api/daemon/deregister", post(lifecycle::deregister))
        .route("/api/daemon/heartbeat", post(lifecycle::heartbeat))
        .route("/api/daemon/ws", get(lifecycle::ws))
        .route("/api/daemon/workspaces", get(lifecycle::list_workspaces))
        .route(
            "/api/daemon/workspaces/:workspaceId/repos",
            get(lifecycle::workspace_repos),
        )
        .route(
            "/api/daemon/workspaces/:workspaceId/runtime-profiles",
            get(lifecycle::runtime_profiles),
        )
        // ---- claim 面（表 A 8–13） ----------------------------------------
        .route(
            "/api/daemon/runtimes/:runtimeId/tasks/claim",
            post(claims::claim_for_runtime),
        )
        // 两条历史路径同一个 handler —— 老 daemon 打 `/api/daemon/claim`。
        .route("/api/daemon/tasks/claim", post(claims::claim_batch))
        .route("/api/daemon/claim", post(claims::claim_batch))
        .route(
            "/api/daemon/runtimes/:runtimeId/tasks/:taskId/prepare-lease",
            post(claims::prepare_lease),
        )
        // 上游 `router.go:1547`：`tasks/pending` 下**没有** `{taskId}` 段。
        .route(
            "/api/daemon/runtimes/:runtimeId/tasks/pending",
            get(claims::list_pending),
        )
        // 上游 `router.go:1570`：`recover-orphans` 直接挂在 runtimeId 下，**没有** `tasks/` 段。
        .route(
            "/api/daemon/runtimes/:runtimeId/recover-orphans",
            post(claims::recover_orphans),
        )
        .route(
            "/api/daemon/runtimes/:runtimeId/tasks/:taskId/skill-bundles/resolve",
            post(claims::resolve_skill_bundles),
        )
        // ---- 任务生命周期（表 A 14–29） -----------------------------------
        .route("/api/daemon/tasks/:taskId/status", get(tasks::task_status))
        .route("/api/daemon/tasks/:taskId/start", post(tasks::start_task))
        .route(
            "/api/daemon/tasks/:taskId/wait-local-directory",
            post(tasks::wait_local_directory),
        )
        .route(
            "/api/daemon/tasks/:taskId/progress",
            post(tasks::report_progress),
        )
        .route(
            "/api/daemon/tasks/:taskId/complete",
            post(tasks::complete_task),
        )
        .route("/api/daemon/tasks/:taskId/fail", post(tasks::fail_task))
        .route("/api/daemon/tasks/:taskId/usage", post(tasks::report_usage))
        .route(
            "/api/daemon/tasks/:taskId/messages",
            get(messages::list_messages).post(messages::report_messages),
        )
        .route(
            "/api/daemon/tasks/:taskId/cancel-ack",
            post(tasks::ack_cancelled),
        )
        .route(
            "/api/daemon/tasks/:taskId/session",
            post(tasks::pin_session),
        )
        .route(
            "/api/daemon/tasks/:taskId/plugin-hooks",
            post(tasks::plugin_hooks),
        )
        .route(
            "/api/daemon/tasks/:taskId/plugin-mcp/:contributionId/credential",
            get(tasks::plugin_mcp_credential),
        )
        // ---- 三段异步往返的 daemon 侧收口（表 A 30–34 之外，见 §6.2 表 B） --
        .route(
            "/api/daemon/runtimes/:runtimeId/update/:updateId/result",
            post(requests::report_update_result),
        )
        .route(
            "/api/daemon/runtimes/:runtimeId/models/:requestId/result",
            post(requests::report_model_list_result),
        )
        .route(
            "/api/daemon/runtimes/:runtimeId/local-skills/:requestId/result",
            post(requests::report_local_skill_list_result),
        )
        .route(
            "/api/daemon/runtimes/:runtimeId/local-skills/import/:requestId/result",
            post(requests::report_local_skill_import_result),
        )
        // ---- GC 探针（表 A 30–34） ----------------------------------------
        .route(
            "/api/daemon/workspaces/:workspaceId/issues/gc-check",
            post(gc::batch_issue_gc_check),
        )
        .route(
            "/api/daemon/issues/:issueId/gc-check",
            get(gc::get_issue_gc_check),
        )
        .route(
            "/api/daemon/chat-sessions/:sessionId/gc-check",
            get(gc::get_chat_session_gc_check),
        )
        .route(
            "/api/daemon/autopilot-runs/:runId/gc-check",
            get(gc::get_autopilot_run_gc_check),
        )
        .route(
            "/api/daemon/tasks/:taskId/gc-check",
            get(gc::get_task_gc_check),
        )
}

/// 把 ws 面的两个 handler 装进 `mc-ws` hub（幂等）。
///
/// `hub` 里的 handler 槽是 `Arc<dyn Fn…>`，而 handler 又需要 `Arc<AppState>` ——
/// 直接互相持有可能成环（`AppState` → hub → handler → `AppState`）。这里的做法是
/// **一次性注入**：调用方拿一个 `Weak<AppState>` 进来，handler 每次执行时再 upgrade，
/// 槽里不常驻强引用，环不存在。
///
/// **幂等闸按 hub 判**（不是按进程）：`set_rpc_handler` / `set_heartbeat_handler` 是
/// 「覆盖」语义，重复装只是换掉等价的闭包，所以拿 `rpc_handler().is_some()` 当已装标记
/// 就够。**不能**用进程级 `OnceLock`：一个进程可以有多个 hub（测试里每个用例一个），
/// 进程级闸会把第一个 hub 之后的全部漏装，症状是连接升级成功但 RPC 一律 503
/// `rpc handler unavailable`。
///
/// 调用点：`GET /api/daemon/ws` 的升级 handler（[`lifecycle::ws`]）—— 那里同时握着
/// `Arc<AppState>` 与 `daemon_hub`，而 `mount.rs::mount_slice_daemon()` 只有
/// `Router<Arc<AppState>>`（拿不到 Arc）。handler 只在真有人升级时才走到，所以「懒装」
/// 不会漏装。每次升级重复调用时上面那道闸直接短路（多一次 `Arc::clone`，不做分配）。
pub fn install_ws_handlers(state: &Arc<AppState>) {
    let hub = &state.daemon_hub;
    if hub.rpc_handler().is_some() && hub.heartbeat_handler().is_some() {
        return;
    }
    // 实现体在 `ws.rs`（与 rpc / heartbeat 的真实逻辑放在一起）。
    ws::install(state);
}

#[cfg(test)]
mod tests {
    /// 表 A 的 36 个端点必须全部注册。
    ///
    /// axum 不暴露路由清单，所以靠源码计数：`/tasks/:taskId/messages` 一条 `.route(`
    /// 同时挂 GET + POST，因此 36 个端点 = 35 条 `.route(`。
    #[test]
    fn table_a_registers_every_endpoint() {
        let source = include_str!("mod.rs");
        let body = source
            .split("pub fn router()")
            .nth(1)
            .expect("router() 存在")
            .split("/// 把 ws 面的两个 handler")
            .next()
            .expect("router() 体存在");
        assert_eq!(
            body.matches(".route(").count(),
            35,
            "表 A 应有 35 条 .route("
        );
        assert_eq!(
            body.matches("get(").count() + body.matches("post(").count(),
            36
        );
    }
}
