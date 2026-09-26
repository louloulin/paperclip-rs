//! billing 面（8 条 owner-credit 出站代理）的出站路径与响应类型位 —— **写者 M9-1**。
//!
//! # 这一片（M9-1）要填什么
//!
//! 上游 `internal/handler/cloud_billing.go` 的 `L356–L503`（8 条 billing handler）。
//! 全部是 `proxyCloudRuntime` 的**纯透传**：本地不落任何表（`docs/62` §9.4：
//! 本地与上游都是 **0 张** `cloud_billing_*` / `checkout_session` / `topup` 表）。
//!
//! 出站路径（逐字取自 `cloud_billing.go:357/367/378/386/400/411/439/477`，本文件只登记
//! 事实、**不**导出常量 —— 常量形状归 M9-1 自定，避免两处漂移）：
//!
//! | 本地路由 | 出站 | 备注 |
//! | --- | --- | --- |
//! | `GET /api/cloud-billing/balance` | `GET /api/v1/billing/balance` | |
//! | `GET /api/cloud-billing/transactions` | `GET /api/v1/billing/transactions` | 带 query |
//! | `GET /api/cloud-billing/batches` | `GET /api/v1/billing/batches` | 带 query |
//! | `GET /api/cloud-billing/topups` | `GET /api/v1/billing/topups` | 带 query |
//! | `GET /api/cloud-billing/price-tiers` | `GET /api/v1/billing/price-tiers` | |
//! | `POST /api/cloud-billing/checkout-sessions` | `POST /api/v1/billing/checkout-sessions` | 体 |
//! | `GET /api/cloud-billing/checkout-sessions/{sessionId}` | `…/checkout-sessions/{id}` | **路径参数透传** |
//! | `POST /api/cloud-billing/portal-sessions` | `POST /api/v1/billing/portal-sessions` | 体 |
//!
//! # 与本 crate 的接口（已冻结）
//!
//! ```ignore
//! // 唯一的出站调用形态（`Request` 的 builder 在 transport.rs）
//! let response = client.send(
//!     Request::get("/api/v1/billing/balance")
//!         .with_user_id(user_id)          // 「身份是账号级的」⇒ 必须注入 X-User-ID
//!         .with_request_id(request_id)
//!         .with_op("billing"),
//! ).await?;
//! ```
//!
//! 错误映射按 `docs/62` §2.6：`Disabled` ⇒ 403 `cloud_runtime_not_configured`；
//! `InvalidBaseUrl` ⇒ 500 `cloud_runtime_misconfigured`；`Timeout` ⇒ 504；
//! `Transport` / `ResponseTooLarge` ⇒ 502。**云侧的 4xx/5xx 不是错误**（原样透传）。
//!
//! # 本文件在 anchor 期是**空桩**
//!
//! anchor **不实现任何路由逻辑**（`docs/62` §5 的纪律）：本文件此刻只有这份说明，
//! 让 M9-1 不必回来改任何 frozen 文件即可原地填充。
//! 形态纪律（`docs/62` §1.4 实测 `dual-form required: 0`）：8 条**只按上游字面量注册**
//! 那一形态，路径参数写 `:sessionId`（matchit 0.7 把 `{…}` 当字面量 ⇒ 编译通过且恒 404）。
