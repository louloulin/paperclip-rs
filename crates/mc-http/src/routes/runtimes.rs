//! `/api/runtimes*` + `/api/workspaces/:id/runtime-profiles*`（M3-4 / LUM-1427，15 条）。
//!
//! 上游 handler 在 `server/internal/handler/runtime.go`（台账 / 用量 / 删除）、
//! `runtime_profile.go`（自定义 runtime profile）与 `service/runtime_teardown.go`
//! （拆除事务）；SQL 在 `server/pkg/db/queries/{runtime,runtime_profile,runtime_usage}.sql`。
//! 路由与门禁见 `server/cmd/server/router.go`：`/api/runtimes` L2266-2293，
//! `/api/workspaces/{id}/runtime-profiles` L1680-1681（member）+ L1717-1720（owner/admin）。
//!
//! 本切片做**同步台账面** + 8 条**异步往返**：运行时实例的读 / 改名 / 改可见性 / 删除 +
//! profile 台账 CRUD + 四个用量聚合（`ledger.rs` / `profiles.rs` / `usage.rs`），
//! 以及 update / models / local-skills* 的入队与轮询（`async_requests.rs`，M3-7）。
//! 异步请求的执行方是 daemon（心跳领取 + 上报），服务端只做台账与门禁。
//!
//! ## 鉴权
//!
//! 沿用 M1/M2 的 dev-mode 约定：当前用户来自 `X-Multica-User-Id`（[`AuthUser`]），
//! workspace 来自 `X-Workspace-ID` header 或 `?workspace_id=`。**不用**
//! `middleware::authn` 的 `require_*` 守卫 —— 那套走 `x-multica-session` / cookie，
//! 与本仓既有测试约定分叉（见 `docs/39-M3-4-RUNTIME-PROFILES.md` §2）。
//!
//! 状态码语义与上游一致：未认证 **401**、非成员 **404**（不暴露资源是否存在）、
//! 角色不足 **403**、profile 重名 **409**、活跃 agent 挡删除 **409**。
//!
//! ## 与上游的偏离
//!
//! 完整清单（含逐条理由）见 `docs/39-M3-4-RUNTIME-PROFILES.md` §4。要点：
//! 标准错误体是本仓的嵌套 `{"error":{"code","message"}}`（上游扁平 `{"error":"msg"}`），
//! 只有前端要按 `code` 分支的那几个 409 用扁平体；`runtime_id` / `profile id` 非法时
//! 本地回 400 `"<field> must be a uuid"`；`Json<T>` 提取器对类型不符回 422（上游 400）。
//!
//! ## 尾斜杠
//!
//! 上游是 chi 的 `Route("/api/runtimes") + Get("/")`，客户端两种写法都能命中。
//! matchit 0.7 把尾斜杠当**有效段**（`/x` 与 `/x/` 是两条不同路由，注册两条不会
//! panic），所以这里对 `/api/runtimes` 与 `/api/runtimes/:runtimeId` 的
//! PATCH/DELETE 各注册两条（`inbox.rs` 同款处理）。
//!
//! 文件布局（R7：单文件 800 行硬上限，`scripts/file_size_check.py` + 门 ⑩）：
//! - `runtimes.rs`（本文件）：模块文档 + `router()`
//! - `access.rs`：成员/角色校验、runtime 载入、错误与时间格式化
//! - `protocol.rs`：`protocol_family` / `runtime_type` / `launch_header` 派生
//! - `dto.rs`：响应 DTO + 请求体
//! - `refusals.rs`：三类 409 拒绝体（活跃 agent / 计划漂移 / profile 实例）
//! - `profiles.rs`：6 条 profile 路由
//! - `ledger.rs`：4 条台账路由（list / patch / delete / unbind）
//! - `usage.rs`：4 条用量路由（含 `days` 窗口与时区解析）
//! - `async_requests.rs`：8 条异步往返路由（update / models / local-skills*，M3-7）
#![allow(clippy::option_option)]

mod access;
pub(crate) mod async_requests;
pub(crate) mod dto;
mod ledger;
mod profiles;
mod protocol;
mod refusals;
mod usage;

use axum::routing::{get, patch, post};
use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/runtimes*` + `/api/workspaces/:id/runtime-profiles*`（15 条上游路由）。
///
/// 注意：axum 0.7（matchit 0.7）路径参数写 `:id`；`{id}` 会被当字面量段，编译通过但恒 404。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // ---- 自定义 runtime profile（workspace 作用域，id 是 workspace） ----
        .route(
            "/api/workspaces/:id/runtime-profiles",
            get(profiles::list_profiles).post(profiles::create_profile),
        )
        .route(
            "/api/workspaces/:id/runtime-profiles/:profileId",
            get(profiles::get_profile)
                .patch(profiles::update_profile)
                .put(profiles::update_profile)
                .delete(profiles::delete_profile),
        )
        // ---- 运行时台账 ----
        .route("/api/runtimes", get(ledger::list_runtimes))
        .route("/api/runtimes/", get(ledger::list_runtimes))
        .route(
            "/api/runtimes/:runtimeId",
            patch(ledger::update_runtime).delete(ledger::delete_runtime),
        )
        .route(
            "/api/runtimes/:runtimeId/",
            patch(ledger::update_runtime).delete(ledger::delete_runtime),
        )
        // ---- 用量 / 活动 ----
        .route(
            "/api/runtimes/:runtimeId/usage",
            get(usage::get_runtime_usage),
        )
        .route(
            "/api/runtimes/:runtimeId/usage/by-agent",
            get(usage::get_runtime_usage_by_agent),
        )
        .route(
            "/api/runtimes/:runtimeId/usage/by-hour",
            get(usage::get_runtime_usage_by_hour),
        )
        .route(
            "/api/runtimes/:runtimeId/activity",
            get(usage::get_runtime_activity),
        )
        // ---- 确认删除（cascade）；archive-* 是装过的旧客户端走的遗留路径，同一 handler ----
        .route(
            "/api/runtimes/:runtimeId/unbind-agents-and-delete",
            post(ledger::unbind_agents_and_delete),
        )
        .route(
            "/api/runtimes/:runtimeId/archive-agents-and-delete",
            post(ledger::unbind_agents_and_delete),
        )
        // ---- 异步往返（服务端入队 + 客户端轮询；`docs/16` §6.2 表 B）----
        .route(
            "/api/runtimes/:runtimeId/update",
            post(async_requests::initiate_update),
        )
        .route(
            "/api/runtimes/:runtimeId/update/:updateId",
            get(async_requests::get_update),
        )
        .route(
            "/api/runtimes/:runtimeId/models",
            post(async_requests::initiate_list_models),
        )
        .route(
            "/api/runtimes/:runtimeId/models/:requestId",
            get(async_requests::get_model_list_request),
        )
        .route(
            "/api/runtimes/:runtimeId/local-skills",
            post(async_requests::initiate_list_local_skills),
        )
        .route(
            "/api/runtimes/:runtimeId/local-skills/import",
            post(async_requests::initiate_import_local_skill),
        )
        // 注意：静态段 `import` 与 `:requestId` 同层 —— matchit 0.7 按「静态优先」匹配，
        // 所以 `GET .../local-skills/import/<id>` 命中下面那条，其余命中上面这条。
        .route(
            "/api/runtimes/:runtimeId/local-skills/import/:requestId",
            get(async_requests::get_local_skill_import_request),
        )
        .route(
            "/api/runtimes/:runtimeId/local-skills/:requestId",
            get(async_requests::get_local_skill_list_request),
        )
}
