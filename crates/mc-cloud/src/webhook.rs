//! stripe webhook 转发面的路径与原始体纪律位 —— **写者 M9-6**。
//!
//! # 这一片（M9-6）要填什么
//!
//! 上游 `internal/handler/cloud_billing.go` 的 `L38–L52`（常量）+ `L504–L604`
//! （`HandleCloudBillingStripeWebhook`）。出站路径 **`/api/v1/webhooks/stripe`**
//! （`cloud_billing.go:590`），方法 `POST`。
//!
//! # 本地**只有**三段语义（顺序固定，`docs/62` §9.5）
//!
//! | 顺序 | 判据 | 结果 |
//! | :-: | --- | --- |
//! | 1 | `MULTICA_CLOUD_URL` 未配置 | **403** `cloud_runtime_not_configured`（**先于**限流与签名检查） |
//! | 2 | per-IP 限流超限 | **429** |
//! | 3 | 缺 `Stripe-Signature` 头 | **401** |
//! | — | 1 MiB 体上限 | **413** |
//!
//! 三段之后是**原始体逐字**出站：不 `trim`、不 `json` 校验、不重编码；且
//! **`X-User-ID` 不注入**（`Request::user_id` 留 `None` —— 这是本 crate 支持
//! 「无身份转发」的用例，见 `transport.rs` 的同名用例）。
//!
//! # 两条**有意等价**（不是缺口，`docs/62` §9.5）
//!
//! 1. **不做本地验签**：签名校验在**云侧**（本地不读 `STRIPE_WEBHOOK_SECRET`、
//!    不自建 HMAC）；
//! 2. **不做本地事件去重**：幂等由云侧事件 id 负责；本波的"幂等"验收只覆盖
//!    「同一请求重投 ⇒ 转发两次、本地不落任何状态」。
//!
//! # 限流器复用（**禁止**新写）
//!
//! 上游复用 `h.WebhookIPRateLimiter`（`handler.go:492-494` 三条装配之一）。本地对应物是
//! `crates/mc-autopilot/src/webhook/ratelimit.rs` 的 `SlidingWindowLimiter`（三条闸）。
//! ⚠️ 上游字段名与本地三条闸**不是一一对应** ⇒ M9-6 起手必须逐字复核是哪一条并写进
//! 自己的 `DoD`（`docs/62` §9.5 末段）。
//!
//! # 本文件在 anchor 期是**空桩**
//!
//! anchor 不实现任何路由逻辑（`docs/62` §5）。常量与 handler 归 M9-6 原地填充。
//! ⑨ 的 3 条 `unmounted → pass` 判据（401/403/429）是它的门禁锚点（`docs/62` §6.2）。
