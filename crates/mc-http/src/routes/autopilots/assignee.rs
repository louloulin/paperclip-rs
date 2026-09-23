//! M5-0 anchor：autopilot **assignee 解析与校验**共享模块（**非路由**，不含 `router()`）。
//!
//! - **写者**：M5-2（`docs/44` §3.2），但 M5-3 的 trigger 写面会**读**它（`trigger.rs` 要按同一
//!   套规则校验 assignee 快照）。
//! - **上游**：`validateAutopilotAssigneeForSave`76 + `isValidAutopilotAssigneeType`19
//!   （`handler/autopilot.go`，合计 135 行 —— §6.3 明确把它从 `crud.rs` 拆出来防门 ⑩）。
//! - **二态**：`assignee_type ∈ {agent, squad}`（`042` 建列、`096` 定为二态）。
//! - **`squad` 的解析规则（Squad-as-Leader，`096` / MUL-2429）**：squad 不直接当 agent 用 ——
//!   运行期要解析到 `squad.leader_id`；`096` 同时给 `autopilot_run` 加了 `squad_id` 快照列
//!   （跑的时候的队长是谁，以 run 行为准，不要事后重算）。
//! - **`assignee_id` 的语义随 `assignee_type` 变**（`042` 原建的是 `REFERENCES agent(id)`，
//!   已由 `096` 放开为多态）⇒ 任何「JOIN agent」都要先判 `assignee_type`。
