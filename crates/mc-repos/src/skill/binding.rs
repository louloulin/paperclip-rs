//! `agent_skill` 的绑定面（**授权面**）。
//!
//! - **写者**：M6-4（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/handler/agent.go` 的 `/api/agents/{id}/skills*` 六个 handler。
//! - **语义**：`agent_skill` 这张表**就是**授权 —— 一个 skill 能不能被某个 agent 看到/用，
//!   唯一的判据是这里有没有一行且 `enabled = TRUE`（`ListAgentSkillsByIDs` 是唯一入口）。
//!   所以本文件的写操作必须与「读面」用同一套过滤条件（`enabled` 不要一边查一边不查）。
//! - **幂等**：绑定 / 解绑 / 启停都是**幂等**动作（重复 add = 200 而不是 500）。
//!   实现上走 `ON CONFLICT (agent_id, skill_id) DO UPDATE SET enabled = ...`。
//! - **bundle 的 `source`**：这里只落/读行；bundle 的 `SkillRef{source}` 投影在 route 层
//!   （`workspace` / `builtin` / `plugin` 三态见 `mc_core::skill::SkillSource`）。
//! - **不做什么**：不建「skill 组 / 目录」（上游这一代没有）。
//!
//! **状态：M6-4 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 200 行以内。
