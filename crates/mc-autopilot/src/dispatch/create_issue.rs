//! `create_issue` 执行模式：建 issue + 派任务。
//!
//! - **写者**：M5-4。
//! - **上游**：`dispatchCreateIssue`199 + `notifyAutopilotSubscribersOnCreate`91（通知本体在
//!   `../notification.rs`，本文件只调用）+ 模板与工具 200（`issue_title_template` 渲染）。
//! - **数据来源**：`autopilot.execution_mode = 'create_issue'`、`issue_title_template`、
//!   `project_id`（`058` DROP、`097` 加回，FK `ON DELETE SET NULL`）。
//! - **幂等**：与 M5-5 的 `ensureWebhookCreateIssueTask`61 / `repairAutopilotRunTaskLink`56 共用
//!   「run ↔ task ↔ issue 三者的回填」逻辑，别各写一份。
