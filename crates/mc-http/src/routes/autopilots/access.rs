//! M5-0 anchor：autopilot 面**权限/可见性**共享模块（**非路由**，不含 `router()`）。
//!
//! - **写者**：M5-1（`docs/44` §3.2）。
//! - **上游**：`autopilotWriteByOwnership`14 + `memberCanWriteAutopilot`70 +
//!   `autopilotActingUserID`36 + `requireAutopilotActingMember`35 + `loadAutopilotInWorkspace`25
//!   （`handler/autopilot.go` 的权限段，合计 157 行 = ⑨ 的 `TestAutopilot…Forbidden` 族真值）。
//! - **两条判负语义**（`docs/44` §4.2）：
//!   1. **非本 workspace / 非成员一律 404**（不是 403）—— 与 `invitations::require_workspace_member`
//!      的既有约定一致，避免资源存在性泄露；
//!   2. **成员但无写权 → 403**（`memberCanWriteAutopilot` 判负）。
//! - **不要在本文件重新实现 workspace 解析**：复用 `crate::routes::workspaces` /
//!   `invitations` 的既有入口（`resolve_workspace` 一族），否则会出现第二份成员真值。
//! - **写面（M5-2/3/4）只调用本模块**，不要各自复制一份权限判断。
