//! `/api/inbox*`（14 条）+ `/api/issues/{id}/{subscribers,subscribe,unsubscribe*}`（4 条）
//! 的端到端测试。
//!
//! 分两类：
//! - `route_paths_are_mounted`：**不需要数据库**，用 `Db::connect_lazy` 装配完整 router，
//!   逐个打 18 条路径。命中路由时"没带用户头"必然 401、"带了用户头没带 workspace"必然
//!   400（workspace 解析在成员校验之前，不碰 DB）；一旦某条路径写错（或用了 axum 0.8 的
//!   `{id}` 字面量写法），就会掉到 404 兜底而失败。CI 无库也能挡回归。
//! - 其余测试需要真实 PG（`MULTICA_TEST_DATABASE_URL`），全部 `#[ignore]`。
//!
//! 运行示例：
//! ```
//! cargo test -p mc-http --test inbox --features test-util                 # 仅路由守卫
//! MULTICA_TEST_DATABASE_URL=postgres://u:p@host:5432/db \
//!   cargo test -p mc-http --test inbox --features test-util -- --ignored  # 全量
//! ```
//!
//! 文件布局（门 ⑩ 单文件 800 行硬上限，`scripts/file_size_check.py`；`LUM-2544` 由
//! 单文件 1149 行拆来，先例 = `tests/vcs/{main,support}.rs` 与 `tests/vcs/connections/`）：
//! - `inbox/support.rs`：装配 `call`/`get`/`post`、`AppState`、夹具 `Fx`/`seed`/`new_item`
//! - `inbox/route_guard.rs`：1. 路由存在性守卫（18 条路径，无需库）
//! - `inbox/list_read.rs`：2. 列表 / 已读 / 可见性
//! - `inbox/archived_view.rs`：3. 归档视图 facets + 游标分页 + 过滤
//! - `inbox/bulk_and_summary.rs`：4. 批量操作 + 跨 workspace 未读汇总
//! - `inbox/subscribers.rs`：5. issue 订阅者
//!
//! **纯移动**：断言、SQL、夹具调用逐字未改；唯一的规范化是 `support.rs` 里拆分必需的
//! 可见性窄化（`fn` → `pub(crate) fn`、`struct Fx` 及其字段同理），逐条登记在 PR 描述。
#![cfg(feature = "test-util")]

mod archived_view;
mod bulk_and_summary;
mod list_read;
mod route_guard;
mod subscribers;
mod support;
