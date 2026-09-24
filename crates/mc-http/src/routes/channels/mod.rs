//! 渠道面聚合：24 条注册键（5 个平台文件 = 5 个写者）。
//!
//! ## ⚠️ 本文件由 M7-0 anchor 冻结，M7 后续切片**不得**编辑
//!
//! 合并点在这里；五个平台文件由**各自的写者**实作（`docs/60` §3.3）。若某切片发现需要的
//! 子文件不在下面的清单里，**不要**直接加到这里 —— 记到 `docs/32` §10 的文件→写者表里，
//! 由集成方（M7-21）统一加。
//!
//! ## 路由账（`docs/60` §1.1 的 24 条，逐条点名到 `router.go` 行号）
//!
//! | 注册键 | 方法 | `router.go` | 写者 |
//! | --- | :-: | ---: | :-: |
//! | `/api/workspaces/:id/lark/installations` | GET | 1783 | M7-14 |
//! | `/api/workspaces/:id/lark/installations/:installationId` | DELETE | 1784 | M7-14 |
//! | `/api/workspaces/:id/lark/install/begin` | POST | 1790 | M7-14 |
//! | `/api/workspaces/:id/lark/install/:sessionId/status` | GET | 1791 | M7-14 |
//! | `/api/workspaces/:id/slack/installations` | GET | 1801 | M7-4 |
//! | `/api/workspaces/:id/slack/installations/:installationId` | DELETE | 1806 | M7-4 |
//! | `/api/workspaces/:id/slack/install/byo` | POST | 1807 | M7-4 |
//! | `/api/workspaces/:id/wecom/installations` | GET | 1802 | M7-15 |
//! | `/api/workspaces/:id/wecom/installations/:installationId` | DELETE | 1808 | M7-15 |
//! | `/api/workspaces/:id/wecom/install/byo` | POST | 1809 | M7-15 |
//! | `/api/workspaces/:id/dingtalk/installations` | GET | 1814 | M7-9 |
//! | `/api/workspaces/:id/dingtalk/groups` | GET | 1815 | M7-9 |
//! | `/api/workspaces/:id/dingtalk/installations/:installationId/groups/:conversationId` | DELETE | 1816 | M7-9 |
//! | `/api/workspaces/:id/dingtalk/installations/:installationId` | DELETE | 1817 | M7-9 |
//! | `/api/workspaces/:id/dingtalk/install/byo` | POST | 1818 | M7-9 |
//! | `/api/workspaces/:id/telegram/installations` | GET | 1825 | M7-5 |
//! | `/api/workspaces/:id/telegram/installations/:installationId` | DELETE | 1829 | M7-5 |
//! | `/api/workspaces/:id/telegram/install` | POST | 1830 | M7-5 |
//! | `/api/lark/binding/redeem` | POST | 1841 | M7-14 |
//! | `/api/slack/binding/redeem` | POST | 1847 | M7-4 |
//! | `/api/dingtalk/binding/redeem` | POST | 1850 | M7-9 |
//! | `/api/wecom/binding/redeem` | POST | 1854 | M7-15 |
//! | `/api/telegram/binding/redeem` | POST | 1858 | M7-5 |
//! | `/api/agents/:id/dingtalk/groups` | GET | 2192 | M7-9 |
//!
//! 账：M7-4 **4** + M7-5 **4** + M7-9 **7** + M7-14 **5** + M7-15 **4** = **24** ✓
//! （与 `docs/fixtures/m7-declared-routes.tsv` 的 24 行逐字相等；复算见 `docs/60` §10 命令 1）。
//!
//! ## 锚点期**零注册键**（也正是本片 ⑦ 读数不变的原因）
//!
//! 五个平台文件现在都是**空** `Router::new()`，所以 `mount_slice_channel()` 合并进全局
//! router 之后**注册键集合逐字不变**（`docs/60` §6.1 的 M7-0 行：`local 406` 不动）。
//! 这也是五轮里**第一个不刷 ⑦ 基线的 anchor**：零路由删除、零 M0 占位。
//!
//! ## 两个结构决策（anchor 期定死，登记 `docs/32` §10）
//!
//! 1. **全路径注册，不 `nest`**：上游把这 18 条 workspace 级路由注册在既有
//!    `/api/workspaces/{id}` 子路由**内部**（`router.go` 的同一个 `r.Route("/{id}", …)` 块，
//!    五组注册位置互不相邻）。本仓若照抄 `nest` 到 `workspaces.rs`，就会与该文件已有的
//!    `/api/workspaces/:id` 路由抢同一个挂载点（axum 0.7 的嵌套与既有路径重叠会 panic）。
//!    所以每个平台文件写**完整路径** `/api/workspaces/:id/<platform>/…`，由
//!    `mount_slice_channel()` 在顶层 merge —— 路径唯一，不会撞既有注册。
//! 2. **参数名照上游语义**：`:installationId` / `:sessionId` / `:conversationId` / `:id`。
//!    ⚠️ 语法必须是 `:name`（matchit 0.7 把 `{name}` 当**字面量**段：编译通过、恒 404）。
//!
//! ## 形态纪律（M7 **没有** allowlist 退路）
//!
//! `docs/60` §1.4 实测：24 条**全部**是完整子路径的 plain 注册 ⇒ `dual-form required: 0`。
//! 所以各片**只按上游字面量注册那一形态**：既不补尾斜杠形态（补了 = `EXTRA_ALIAS` 缺陷，
//! axum 会同时服务 `/x` 与 `/x/` 而上游只服务 `/x`），也不得漏成带斜杠形态
//! （`MISSING_ALIAS` / `MISSING_EXACT` 都是硬失败）。
//!
//! ## 不做什么
//!
//! - anchor 期**零 handler**：24 条的 handler 全在各平台文件里，由五个写者填；
//! - 不在这里做鉴权（`authn` / `authz` 中间件已在全局链上）；
//! - `GET /api/workspaces/:id/dingtalk/group-routes` **不在此表**：上游已退役该路由，
//!   且 `integration_test.go:786` 主动断言它 **404** ⇒ M7-9 的 `DoD` 是"保持不存在"
//!   （`docs/60` §1.6）。

pub mod dingtalk;
pub mod lark;
pub mod slack;
pub mod telegram;
pub mod wecom;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 24 条渠道路由的聚合 router（state 由 `main.rs` 的 `with_state` 一次性注入）。
///
/// anchor 期五个子 router 都是空的 ⇒ 本函数的返回值**不加任何注册键**。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(slack::router())
        .merge(telegram::router())
        .merge(dingtalk::router())
        .merge(lark::router())
        .merge(wecom::router())
}
