//! crate 级错误类型与 HTTP 映射。
//!
//! - **写者**：M5-1（框架）。其余切片加自己的错误变体时**只准加变体**，不改既有签名。
//! - **上游**：上游没有集中的 error 类型（handler 各自 `writeError` + 状态码）；本地统一收敛到
//!   本文件，再由 `mc-http` 的 `ApiError` 转响应。
//! - **要覆盖的状态码语义**：400（参数/三态补丁非法）、403（`autopilotWriteByOwnership` /
//!   `memberCanWriteAutopilot` 判负）、404（`loadAutopilotInWorkspace` 判负 —— 非成员一律 404，
//!   与 `invitations::require_workspace_member` 一致）、409（订阅者并发锁 / token 唯一性冲突）。
//! - **纪律**：`AutopilotError → ApiError` 的映射是**契约**（M5-2..M5-6 都吃它），落地时要和
//!   `docs/44` §1.1 的路由级状态码逐条对齐；不要在切片里各自拼 `StatusCode`。
