//! per-task MCP overlay 的**纯函数**合并 —— M8-0 anchor 建桩，**实现归 M8-3**
//! （`LUM-1800` / `docs/61-M8-PLAN.md` §2.3 第 2 条 / §3.3）。
//!
//! # 合并契约（上游 `handler/mergeMCPOverlay` 的逐字语义）
//!
//! 输入双方都是 Claude 风格 `{"mcpServers": {<name>: <object>}}`；任何不在 `mcpServers`
//! 下的顶层键**只从 agent 侧保留**（overlay 今天只携带 server 条目，不得悄悄引入别的顶层键）。
//!
//! 合并**按 server 名**进行，**overlay 胜出**（overlay 携带的是用户自己的实时 session URL，
//! 例如 Composio 的 bearer；agent 侧同名条目多半是过期/管理员共享的占位）。
//!
//! 两侧都为空 / `null` ⇒ 返回 `None`（让 daemon 的 `hasManagedCursorMcpConfig` 短路继续
//! 把 task 当作「完全无托管 MCP」）。
//!
//! **失败模式**：输入非法时返回 agent 侧原值 + 错误（调用侧**不得**因为 overlay 坏了就
//! 静默丢掉 agent 已保存的 servers）。
//!
//! # 与 daemon 侧既有语义的关系
//!
//! daemon 侧 `crates/mc-daemon/src/mcp/**` 已实现「runtime 层做底、agent 层同名覆盖」；
//! 本函数是**上游一层**（agent 已解析后的 `mcp_config` ← per-task overlay）。M8-3 实现时
//! 必须与 `docs/32` §9.10 的对照表逐条一致，**不得**改动 daemon 文件。

use serde_json::Value;

/// overlay 合并错误（agent 侧原值始终随错误一起返回给调用侧）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum McpOverlayError {
    #[error("agent mcp_config is not a JSON object")]
    AgentConfigNotObject,
    #[error("overlay is not a JSON object")]
    OverlayNotObject,
    #[error("mcpServers is not a JSON object")]
    ServersNotObject,
}

/// 把 per-task overlay 叠加到 agent 的 `mcp_config` 上。
///
/// anchor 期本函数**未实现**（`todo!()`）：语义归 M8-3（`docs/61` §4.1 的 M8-3 行 /
/// §6.5 的 M8-3 专属 DoD「overlay 合并纯函数与既有合并语义逐条一致」）。
/// **调用它一定 panic** —— 这是刻意的：让它静默返回输入会让「overlay 还没接上」
/// 变成运行期才发现的事。
pub fn merge_task_overlay(
    _agent_mcp_config: &Value,
    _overlay: &Value,
) -> Result<Option<Value>, McpOverlayError> {
    todo!("M8-3：per-task overlay 合并纯函数（docs/61 §2.3 / §6.5 的 M8-3 行）")
}
