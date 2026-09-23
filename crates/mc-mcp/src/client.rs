//! remote MCP 的 JSON-RPC 客户端（`initialize` → `notifications/initialized` → `tools/list` → `tools/call`）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。M6-6 / M6-9 只读。
//! - **上游**：`pkg/remotemcp/client.go`。
//! - **握手顺序是协议的一部分**：`initialize` 之后必须发 `notifications/initialized`
//!   （**通知**：无 `id`、不等回包）；跳过它，有些实现会在 `tools/list` 上直接断连 ——
//!   这不是「可选的礼貌」，照抄上游的顺序。
//! - **工具采纳的交叉校验**：`tools/list` 的结果必须与 `mcp_approvals` 里已采纳的工具
//!   逐条比对（名字 + schema 摘要）。**远端改了 schema ⇒ 视为未采纳**（要求重新采纳），
//!   不能静默沿用旧授权 —— 上游 `validatePinnedRemoteMCPTools` 就是这个语义。
//! - **本仓约定**：`reqwest::Client` 在调用方构造一次并复用（不要每次调用新建连接池）；
//!   每次调用显式超时；错误是 `thiserror` 枚举且能区分「超时 / 传输 / 协议 / 远端业务错误」，
//!   route 层据此写 `plugin_invocation.status` 的 `timeout` / `failed`。
//! - **不做什么**：不做重试策略（调用是同步面，重试由用户/调度重放决定）；不做 SSE / 流式
//!   分片传输（上游这一代是 HTTP 单次往返）。
//!
//! **状态：M6-1 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 380 行以内（四个方法 + 错误分类 + 用例）。接近 800 行门时按
//! 「握手 / 工具调用」拆兄弟文件。
