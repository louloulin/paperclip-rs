//! M4 anchor scaffold（LUM-1470）：chat 面路由切片聚合 —— **空 router 占位**。
//!
//! 归属：`docs/42-M4-PLAN.md` §4.2 把 chat 拆成两个切片，**按子文件分写集**，本文件是
//! 两个切片的公共聚合点（anchor 预建，此后**任何切片都不改本文件**）：
//!
//! | 子模块 | 内容 | 切片 |
//! | --- | --- | --- |
//! | [`session`] | 会话集合/单体 CRUD + `pin` / `archive` / `read` + `draft-restores` | M4-3 |
//! | [`message`] | 消息读取面（`messages` + `messages/page`，含分页游标） | M4-3 |
//! | [`bar`] | 快捷栏：`/api/chat/pinned-agents*` | M4-3 |
//! | [`task`] | 派发与生成面：发消息 / onboarding / quick-actions / pending-task / queued-tasks / pending-tasks / history / thread | M4-4 |
//!
//! `mount.rs::mount_slice_chat()` 已合并本文件的 `router()`，本文件再合并 4 个子模块的
//! `router()` ⇒ 切片**只需实现自己子文件里的 `router()`**，不必改 `mount.rs` / `routes/mod.rs`。
//!
//! # M0 占位已由本 anchor 预删（切片必读）
//!
//! `mount.rs` 里原来的 `GET|POST /api/chat/sessions`（**无**尾斜杠形态）M0 占位已由
//! M4-0 anchor 删除（`docs/42` §5.2）。⇒ 切片注册真路由时**不会**撞重复注册 panic，
//! 但**必须**照上游形态注册：
//!
//! - 上游 `router.go:2335-2336` 是 `Route("/api/chat/sessions") + Post("/") / Get("/")`
//!   ⇒ chi 的 `Mount` 同时服务 `/api/chat/sessions` 与 `/api/chat/sessions/`，
//!   axum 0.7 不做归一化 ⇒ **两个形态都要注册，且方法集合逐字相同**
//!   （规则与全仓对账见 `docs/37-M3-W3C-PREFLIGHT.md` §15.1/§15.3）。
//! - 这条不再有 allowlist 退路：`docs/fixtures/slash-alias-allowlist.tsv` 里那 6 行
//!   （`GET|POST /api/{chat/sessions,projects,squads}`）**已随本 anchor 删除**，
//!   漏注册形态会被门 ⑦ 的 `slash_alias_audit.py` 直接判红（`MISSING_ALIAS`）。
//! - 反向：`/api/chat/pending-tasks`（`router.go:2360`）、`/api/chat/pinned-agents`
//!   （`:2364`）是 `r.Get(...)` 的 plain 子路由，上游**只有无斜杠**那一个形态
//!   ⇒ **不要**加尾斜杠别名（会被判 `EXTRA_ALIAS` 警告）。
//!
//! 路径参数一律写 `:id`（matchit 0.7 把 `{id}` 当字面量段：编译过、恒 404）。

pub mod bar;
pub mod message;
pub mod session;
pub mod task;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// chat 面聚合切片：4 个子模块各自的 `router()` 在这里合并。
///
/// 子 router 仅声明路由表，不在内部 `with_state` —— 真正的 state 由
/// `apps/mc-server/src/main.rs` 在 `mc_http::routes::router().with_state(state)` 时一次性注入。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(session::router())
        .merge(message::router())
        .merge(bar::router())
        .merge(task::router())
}

/// 剥掉 `mc_errors::Error` 的 `Display` 内部前缀，取回上游 Go 的原文（**仅测试用**）。
///
/// `mc-errors` 的 `#[error("validation error: {message}")]` / `#[error("not found: {resource}")]`
/// 会把前缀带进线上 body 的 `message`，而上游 `writeError` 写的是裸文案；断言上游文案时
/// 先剥前缀，避免把内部前缀写死进测试（与 `tests/agents/support.rs::error_message`、
/// `tests/tasks/support.rs` 同款约定）。
///
/// 注意 `not found: <resource>` 是本仓对上游 `<resource> not found` 的**已知偏离**
/// （见 `session.rs` 文件头与 `inbox.rs`）：剥完只剩资源名，所以本文件的 `not found`
/// 断言写 `"chat session"` 而不是上游的 `"chat session not found"`。
#[cfg(test)]
pub(crate) fn upstream_text(text: impl std::fmt::Display) -> String {
    const PREFIXES: [&str; 12] = [
        "validation error: ",
        "not found: ",
        "conflict: ",
        "unprocessable entity: ",
        "forbidden: ",
        "unauthorized: ",
        "workspace not found: ",
        "workspace archived: ",
        "database error: ",
        "internal error: ",
        "io error: ",
        "upstream error: ",
    ];
    let raw = text.to_string();
    for prefix in PREFIXES {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return rest.to_owned();
        }
    }
    raw
}
