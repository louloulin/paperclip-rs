//! `/api/issues*` + `/api/issue-statuses*` 端到端测试（M2-A / LUM-1348）。
//!
//! 需要真实 PG：`issue` / `issue_status` / `issue_reaction` 三张表必须已迁移。
//! 通过 `MULTICA_TEST_DATABASE_URL` 触发；没有该 env 时静默 skip（与
//! `tests/invitations.rs` 一致）。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-http --test issues --features test-util -- --ignored
//! ```
//!
//! 文件布局（R7：单文件 800 行硬上限，`scripts/file_size_check.py` + 门 ⑩ 执行）：
//! - `issues/support.rs`：连接 / `AppState` / workspace 种子 / 请求小工具
//! - `issues/crud.rs`：CRUD 往返
//! - `issues/filters.rs`：过滤 / 搜索 / 分组
//! - `issues/children.rs`：父子关系 / move / batch
//! - `issues/reactions.rs`：reactions / metadata / properties
//! - `issues/statuses.rs`：issue-statuses 目录生命周期
//! - `issues/auth.rs`：鉴权 / workspace 解析 / 501 占位
//! - `issues/validation.rs`：`(assignee_type, assignee_id)` 存在性 + `attachment_ids` 形态（LUM-1410）
//! - `issues/wakeups.rs`：issue wakeup 8 路由（M5-6 / LUM-1565）
#![cfg(feature = "test-util")]

mod auth;
mod children;
mod crud;
mod filters;
mod reactions;
mod statuses;
mod support;
mod validation;
mod wakeups;
