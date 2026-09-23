//! 通知面：订阅者收件人解析 + 派发/终态通知的投递。
//!
//! - **写者**：M5-1（§3.2 矩阵把本文件判给 M5-1；M5-4 只**调用**）。
//! - **上游**：`service/autopilot_notification_recipient.go`91（收件人解析）+
//!   `notifyAutopilotSubscribersOnCreate`91 + `publishRunDone`13（`service/autopilot.go`）。
//! - **投递通道**：走 `mc-realtime`（本 crate 的依赖），不要直接摸 `mc-ws` 的连接表。
//! - **边界**：`autopilot_subscriber`（`120`）与 `autopilot_collaborator`（`128`）的
//!   `user_type` CHECK 都**只允许 `'member'`**（上游注释 "Members-only for now"）⇒ 收件人集合里
//!   不会出现 agent/daemon 主体，别按 `AutopilotActorType` 的三态去写分发分支。
