//! 协作者与订阅者（`autopilot_collaborator` / `autopilot_subscriber`）。
//!
//! - **写者**：M5-2。
//! - **上游**：`AddAutopilotCollaborator`59 / `writeAutopilotCollaborators`17 /
//!   `RemoveAutopilotCollaborator`35 / `parseAutopilotSubscribers`33 /
//!   `lockAndValidateAutopilotSubscribers`39 / `validateAutopilotAssigneeForSave`76 /
//!   `isValidAutopilotAssigneeType`19（`handler/autopilot.go`）。
//! - **必须同事务加锁**：`lockAndValidateAutopilotSubscribers`(39) 上游用 `FOR SHARE` / `FOR UPDATE`
//!   语义 ⇒ 本地落地时以**真库并发测试**为准（不能只写 SELECT）。
//! - **assignee 二态**：`agent` | `squad`（`096` 起 `assignee_type` + `assignee_id`）；
//!   `squad` 在**运行期**解析为 `squad.leader_id`（Squad-as-Leader，`096` / MUL-2429）——
//!   不是把 squad id 直接当 agent id 用。
//! - **`user_type` 只有 `'member'`**（`120` / `128` 的 CHECK）⇒ 用
//!   `mc_core::autopilot::AutopilotUserType`（单变体），不要用 `AutopilotActorType`。
//! - **无外键**：这两张表的相关列在库里没有 FK ⇒ 主体存在性由本文件校验（app 层完整性）。
