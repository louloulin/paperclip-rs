//! webhook 入口面（**唯一无认证入口**，R5）。
//!
//! - **写者**：M5-5（`webhook/**` 整组）。
//! - **上游**：`handler/autopilot_webhook.go` 全 1,010（`HandleAutopilotWebhook`298 /
//!   `persistInboundDelivery`52 / `finalise*`65 / 签名 36 / 事件过滤 129 / 限流 40 / provider 适配 145）
//!   以及 `service/autopilot.go` 的 `AdmitAutopilotWebhookDelivery`68 /
//!   `recoverConcurrentWebhookAdmission`26 / `DispatchAutopilotForWebhookDelivery`43 /
//!   `ensureWebhookCreateIssueTask`61 / `repairAutopilotRunTaskLink`56。
//! - **路由**：`POST /api/webhooks/autopilots/{token}`（本波唯一无认证路由，`docs/44` §1.1 #21）。
//! - **`webhook_delivery.status` 语义**（按 `093_webhook_deliveries.up.sql` 的注释）：
//!   `{queued,dispatched,rejected,ignored,failed}`；被 admission 跳过的 run 仍算 `dispatched`，
//!   「跳过」记在 `autopilot_run.status` 上。
//! - **两个计数器不要混**：`attempt_count` 是**入站去重命中计数**，`dispatch_attempts` 才是 worker
//!   的派发尝试次数（`176_webhook_delivery_worker`）。
pub mod admission;
pub mod provider;
pub mod ratelimit;
pub mod signature;
