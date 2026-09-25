//! per-task MCP overlay 的**构建** —— 上游 `integrations/composio/dispatch.go` 的
//! session URL / overlay 部分（M8-0 anchor 建桩，**实现归 M8-6**）。
//!
//! # 与 `mc-core::mcp::overlay` 的分工（`docs/61` §2.3 / R-M8-9）
//!
//! - 本文件（M8-6）：**构建** overlay 的那一端 —— 用已连接的 composio session 生成
//!   `{"mcpServers": {"composio": {...}}}`；
//! - `mc-core::mcp::overlay`（M8-3）：把 overlay **合并**进 agent 的 `mcp_config` 的纯函数。
//!
//! ⚠️ R-M8-9：本仓没有上游 `service/task.go` 那样的**中心 enqueue 函数**（task 行由 3 处
//! INSERT 分散创建）⇒ 「3 处 enqueue 接线」**不在本波写集内**，由 M8-7 登记为明确尾账。
//! 因此 `runtime_mcp_overlay` 在本波结束后**仍然恒 NULL** —— 这是**登记过的缺口**，
//! 不是遗漏。

use serde_json::Value;

/// 用 composio 的会话信息构建 per-task overlay —— **anchor 期是桩**，实现归 M8-6。
///
/// 返回 `None` = 该用户没有可用的 composio 连接（⇒ 不注入任何 server）。
pub fn build_task_overlay(
    _toolkit_slug: &str,
    _session_url: &str,
    _composio_user_id: &str,
) -> Option<Value> {
    todo!("M8-6：构建 per-task overlay（docs/61 §4.1 的 M8-6 行）")
}
