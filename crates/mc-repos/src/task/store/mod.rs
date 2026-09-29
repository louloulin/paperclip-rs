//! `TaskStore` 端口的 Pg 实现 + 路由面创建/写入（W3b / M3-6）。
//!
//! [`TaskRepo`](super::TaskRepo) 的**写侧**：创建、CAS 迁移、认领、取消、
//! cancel-ack、usage upsert —— 全部对照上游 `server/pkg/db/queries/agent.sql`
//! 与 `task_usage.sql` 的语句逐条移植。
//!
//! # 端口缺口（**本仓更正**）
//!
//! M3-3 的 [`TaskStore::insert`](mc_task::store::TaskStore::insert) 形状是
//! `insert(id, state: &TaskState)`，注释称
//! 「创建迁移的写集就是这些字段的初值」。**这不成立**：
//! `agent_task_queue.agent_id` 是 `NOT NULL`（`contracts/upstream-schema.sql:995`），
//! 而 [`TaskState`](mc_task::state::TaskState) 里没有 `agent_id` / `issue_id` / `priority` /
//! `context` / `trigger_comment_id` / `chat_session_id` / … —— 端口无法表达一行任务的**身份面**。
//!
//! 因此：
//! - 真实创建走 [`TaskRepo::create_task`](super::TaskRepo::create_task)（吃 [`NewTask`]，含身份面）；
//! - 端口实现由 [`PgTaskStore`] 承担 —— 一个极薄的适配器，构造时带上要创建的那一行的
//!   [`NewTask`]，`insert` 用它补齐身份列，其余方法直接转发到 [`TaskRepo`](super::TaskRepo)。
//!
//! 这个缺口无法在 M3-6 修（`mc-task/src/store.rs` 不在本切片写集内），已登记在
//! `docs/41`。端口语义（CAS 返回 `LostRace` 而非错误、认领栅栏必须在 SQL 侧、
//! 策略值由调用方传入）由本实现逐条满足。
//!
//! 文件布局（R7 拆文件，门 ⑩ 的 800 行上限）：
//! - `new_task.rs`：[`NewTask`] 身份面 + `impl NewTask` + 取消语句的 `SET` 拼装
//! - `repo.rs`：`impl TaskRepo`（创建 / 读态 / CAS / 认领 / 取消 / cancel-ack / usage）
//! - `adapter.rs`：[`PgTaskStore`] 端口适配器
//! - `helpers.rs`：DB 错误映射、`SET` 子句拼装与列绑定

mod adapter;
mod helpers;
mod new_task;
mod repo;

pub use adapter::PgTaskStore;
pub use new_task::NewTask;
