//! per-task MCP overlay 的**写入原语** —— **M8-0 anchor 建桩**（`LUM-1797` /
//! `docs/61-M8-PLAN.md` §3.3 / R-M8-9）。
//!
//! # 为什么单独一个文件、走独立 `UPDATE`（`docs/61` §3.2）
//!
//! `crates/mc-repos/src/task/store.rs`（782 行）里的 `NewTask` 字面量有**多处**构造点
//! （`store.rs` / `chat_task/send.rs` / `autopilot/run.rs`）；往 `NewTask` 里加字段会连锁改
//! 所有调用点、并且逼近门 ⑩ 的 800 行上限。所以 overlay 走一条**独立**的
//! `UPDATE agent_task_queue SET runtime_mcp_overlay = $1 WHERE id = $2`。
//!
//! 读侧**已经接好**：`crates/mc-repos/src/task/queries.rs` 已在 SELECT `runtime_mcp_overlay`；
//! 缺少的是**写侧的生产者**（这正是本原语 + `mc-composio` overlay 构建的存在理由）。
//!
//! # R-M8-9：本波**不**接 3 处 enqueue 调用点
//!
//! 本仓没有上游 `service/task.go` 那样的**中心 enqueue 函数**（task 行由 3 处 INSERT 分散
//! 创建）⇒ 「3 处 enqueue 接线」**不在本波写集内**，由 M8-7 登记为**明确尾账**并给出
//! <file:line> 清单。因此 `runtime_mcp_overlay` 在本波结束后**仍然恒 `NULL`** ——
//! 这是**登记过的缺口**，不是遗漏。

/// 把 per-task MCP overlay 写到某个 task 行上。
///
/// **anchor 期本函数未实现**（`todo!()`）：实现归 M8-INT 之后的尾账（R-M8-9）。
/// **调用它一定 panic** —— 这是刻意的：让它静默成功会让「overlay 其实没落库」变成
/// 运行期才发现的事。
///
/// # Errors
///
/// task 不存在 / DB 错误统一映射为 [`crate::RepoError`]。
pub async fn attach_runtime_mcp_overlay(
    _pool: &sqlx::PgPool,
    _task_id: mc_core::Id,
    _overlay: &serde_json::Value,
) -> Result<(), crate::RepoError> {
    todo!("M8 尾账（R-M8-9）：独立 UPDATE runtime_mcp_overlay（docs/61 §3.2）")
}
