//! 入站落库（admission）：去重、事件过滤、并发认领、run 触发。
//!
//! - **写者**：M5-5。
//! - **上游**：`persistInboundDelivery`52 + `finalise*`65 + `AdmitAutopilotWebhookDelivery`68 +
//!   `recoverConcurrentWebhookAdmission`26 + `DispatchAutopilotForWebhookDelivery`43 +
//!   `ensureWebhookCreateIssueTask`61 + `repairAutopilotRunTaskLink`56 + `extractDedupeKey`23 +
//!   `normalizeWebhookPayload`61 + `stripBOM`18 + `inferEvent`31。
//! - **去重**：`dedupe_key` 走部分唯一索引；`webhook_delivery.attempt_count` 在去重命中时递增
//!   （**不是**重试次数）。
//! - **worker 侧**：`available_at` / `lease_token` / `lease_expires_at` / `dispatch_attempts`
//!   由 `176` 加，是**租约**形态（与 M5-7 的 `sys_cron_executions` 同一套语义，但两张表各管各的）。
//! - **`autopilot_run.webhook_delivery_id` 故意没有外键** ⇒ 「run 指向的 delivery 已消失」是可达
//!   状态，需要 `repairAutopilotRunTaskLink`(56) 那类修复路径。
