//! autopilot 写面（create / update / delete + 规则版本）。
//!
//! - **写者**：M5-2。
//! - **上游**：`CreateAutopilot`159 / `UpdateAutopilot`260 / `DeleteAutopilot`64 /
//!   `autopilotRuleSubstantiveChange`13 / `recordAutopilotRuleVersion`4 / `parseAutopilotProjectID`23
//!   （`handler/autopilot.go`）+ `db/queries/autopilot.sql`810 / 58 查询。
//! - **`UpdateAutopilot` 是三态补丁大户**：缺失 / `null` / 有值必须区分
//!   （`mc-http` 侧用 `Option<Option<T>>`，见 `routes/issues/mod.rs` 的 `#![allow(clippy::option_option)]`）。
//! - **规则版本 append-only**（`186_autopilot_rule_version`，MUL-4302）：只有
//!   `autopilotRuleSubstantiveChange` 判为「实质变更」才 append 一行；不是每次 update 都写。
//! - **列口径提醒**：`autopilot` 表**没有** `priority` / `concurrency_policy`
//!   （分别被 `058` 与 `043` 删掉，见 `mc_core::autopilot` 的「旧 stub 错在哪」表）⇒
//!   「跳过派发」的真值只能从别处推（M5-4 的 `shouldSkipDispatch`），不要按旧桩字段写。
