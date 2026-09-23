//! skill 与支持文件的**写**查询（建 / 改 / 删）。
//!
//! - **写者**：M6-2（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill.go` 的 `createSkill` / `updateSkill` / `deleteSkill` 段。
//! - **两条硬语义**：
//!   1. `UNIQUE(workspace_id, name)` 撞了 ⇒ `RepoError::Conflict`，route 层映射 **409**
//!      （不是 400，也不是 500 —— 上游也是冲突语义）；
//!   2. 改内容与改文件是**两个动作**：`skill.content` 是正文列，`skill_file` 是支持文件；
//!      一次 PUT 里两件都变时要在一个事务里（否则会留下「正文新、文件旧」的中间态）。
//! - **本仓约定**：写用 `&mut PgConnection`（与既有仓储一致，便于并入调用方的事务）；
//!   删除是**硬删** + 依赖 `ON DELETE CASCADE`（迁移 `008` 已声明），不要手写级联。
//! - **不做什么**：不做权限判定（工作区成员/角色在 route 层）；不做审计日志（本仓没有这张表）。
//!
//! **状态：M6-2 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 280 行以内。
