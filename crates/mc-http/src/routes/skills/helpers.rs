//! skill 面**共用件**：会话/工作区解析、`load_skill_for_user`、响应投影。
//!
//! - **写者**：M6-2（**W**，本文件唯一写者；`docs/57` §3.2）。M6-3 只**读**。
//!   ⚠️ M6-3 若需要一个还不存在的 helper，**加到自己的** `import.rs` / `refresh.rs`，
//!   **不要**回头改本文件（否则两片同改一个文件 —— 这正是 anchor 要消灭的东西）。
//! - **本文件没有 `router()`**：它不是路由文件，只是被兄弟模块 `use` 的共享模块
//!   （照 `routes/autopilots/access.rs` / `dto.rs` 的既有形态）。
//! - **上游**：`internal/handler/skill.go` 的 `resolveWorkspaceID` / `requireWorkspaceMember`
//!   / `loadSkillForUser`（+ `skill_create.go` 的公共校验）。
//! - **本仓约定**：
//!   - 鉴权沿用 `routes::auth_user::AuthUser` 提取器 + `workspace_role` 家族查询；
//!   - 跨工作区 / 非成员一律 **404**（不是 403）—— 与上游一致，避免探测存在性；
//!   - `load_skill_for_user` 是**唯一**的取 skill 入口：所有子文件（含 M6-3 的 import/refresh）
//!     都要走它，不要各写一份带不同过滤条件的版本；
//!   - DTO 在这里统一投影（`skill` 行 → 响应），**不要**把行结构直接 `Serialize`
//!     （列名与响应字段名不一致，且 `content` 在列表响应里应省略）。
//! - **不做什么**：不在这里做保留路径 / 二进制判定（`mc-skill` 的纯函数）、不做 bundle 哈希
//!   （`routes/daemon/skills.rs`，M6-4）。
//!
//! **状态：M6-2 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 260 行以内。
