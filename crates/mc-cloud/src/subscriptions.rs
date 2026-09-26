//! subscriptions 面（7 条 workspace 级出站代理）的出站路径与注入体形状位 —— **写者 M9-2**。
//!
//! # 这一片（M9-2）要填什么
//!
//! 上游 `internal/handler/cloud_billing.go` 的 `L54–L355`（7 条 subscription handler +
//! `requireCloudSubscriptionWorkspace` + `proxyCloudSubscription`）。同样**零本地表**
//! （`docs/62` §9.4：`cloud_subscription*` 在上游也是 0 张）。
//!
//! 出站路径（逐字取自 `cloud_billing.go:174/190/243/254/284/325/349`；本文件只登记事实）：
//!
//! | 本地路由 | 方法 | 出站路径 | 授权 |
//! | --- | :-: | --- | --- |
//! | `/api/cloud-subscriptions/summary` | GET | `/api/v1/subscriptions/{workspaceId}/summary` | **member 可读** |
//! | `/api/cloud-subscriptions/prices` | GET | `/api/v1/subscriptions/{workspaceId}/prices` | **member 可读** |
//! | `/api/cloud-subscriptions/checkout-sessions` | POST | `/api/v1/subscriptions/checkout-sessions` | owner\|admin |
//! | `/api/cloud-subscriptions/seats/reconcile` | POST | `/api/v1/subscriptions/{workspaceId}/seats/reconcile` | owner\|admin |
//! | `/api/cloud-subscriptions/seats/purchase-preview` | POST | `…/{workspaceId}/seats/purchase-preview` | owner\|admin |
//! | `/api/cloud-subscriptions/seats/purchases` | POST | `…/{workspaceId}/seats/purchases` | owner\|admin |
//! | `/api/cloud-subscriptions/portal-sessions` | POST | `/api/v1/subscriptions/{workspaceId}/portal-sessions` | owner\|admin |
//!
//! ⚠️ 读面**不** gate rollout flag（上游逐字：「Summary and prices are member-readable」），
//! 写面 gate ⇒ flag 关时 **403 `workspace_subscriptions_disabled`**（`docs/62` §2.5 的 B 行）。
//!
//! # 三条只属于本片的契约（`docs/62` §2.7 / §4.2）
//!
//! 1. **`workspace_id` 由中间件注入**（服务端从 `ctxWorkspaceID` 取），
//!    客户端的走私值**被覆盖** —— 替身断言注入体里那个值就是中间件解析出来的那个；
//! 2. **`Idempotency-Key` 两档上限**：checkout / portal 是 **255**，座位购买是 **200**
//!    （上游 `maxCloudSubscriptionIdempotencyKeyLength` /
//!    `maxCloudSubscriptionSeatPurchaseIdempotencyKeyLength`）；
//! 3. **注入体是显式 allowlist**（上游 `cloudSubscriptionCheckoutUpstreamRequest` 注释逐字：
//!    「additive cloud fields are not forwarded until the main repository explicitly reviews
//!    and adds them here」）⇒ 本地请求 DTO 与上游 DTO **不是**同一个类型（本仓在
//!    `mc-core::cloud` 里两者都有）。
//!
//! # 本文件在 anchor 期是**空桩**
//!
//! anchor 不实现任何路由逻辑（`docs/62` §5）。形状与常量归 M9-2 原地填充。
