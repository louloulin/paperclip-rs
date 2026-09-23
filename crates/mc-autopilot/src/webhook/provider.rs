//! provider 适配与事件过滤。
//!
//! - **写者**：M5-5。
//! - **上游**：provider 适配 145 + 事件过滤 129（`validateWebhookEventFilters`17 /
//!   `webhookEventAllowedByTriggerScope`45 / `webhookActionCandidates`45）。
//! - **闭集**：`autopilot_trigger.provider ∈ {generic, github}`（`093` 起的 CHECK）；
//!   事件过滤的候选动作按 provider 分派。
//! - **开放集**：`webhook_delivery.event` 是自由文本 ⇒ 不要为它建封闭枚举。
//! - **与 M5-3 的边界**：`isAllowedWebhookProvider`9 归 M5-3（trigger 写面），本文件只消费该判定。
