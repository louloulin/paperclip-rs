//! M9 云侧面的**本地 DTO、路径与口径常量**（`docs/62-M9-PLAN.md` §2.1 的「领域类型」层）。
//!
//! # 为什么这里**没有**响应结构体
//!
//! 上游的 billing / subscriptions 是**纯出站代理**：`proxyCloudRuntime` 把云侧响应
//! **原样**写回客户端（`writeCloudRuntimeResponse`：状态码 + 体逐字，`json.Valid` 才补
//! `Content-Type`）。⇒ 本地**没有**响应形状可以建模（建模了就是第二个真相源，
//! 而且会在云侧加字段时静默丢数据）。本模块只有**请求** DTO 与口径常量。
//!
//! # 为什么这里**没有**表
//!
//! `docs/62` §9.4 双侧实测：本地 `migrations/upstream/` 与上游都**没有**
//! `cloud_billing_*` / `cloud_subscription*` / `checkout_session` / `topup` / `stripe_*`
//! ⇒ 计费状态在 `multica-cloud` 自己的库里。本波**零新迁移**。
//!
//! # 口径常量（逐字取自上游 `cloud_billing.go` / `cloud_runtime.go`）
//!
//! | 常量 | 值 | 上游 |
//! | --- | --- | --- |
//! | [`MAX_CLOUD_RUNTIME_REQUEST_BODY_SIZE`] | `1 << 20` | `cloud_runtime.go:19` |
//! | [`MAX_STRIPE_WEBHOOK_BODY_SIZE`] | `1 << 20` | `cloud_billing.go:41` |
//! | [`MAX_IDEMPOTENCY_KEY_LENGTH`] | `255` | `cloud_billing.go:49` |
//! | [`MAX_SEAT_PURCHASE_IDEMPOTENCY_KEY_LENGTH`] | `200` | `cloud_billing.go:50` |
//!
//! 常量放在 `mc-core`（而不是 `mc-http` 或 `mc-cloud`）的理由：
//! **`mc-repos` 的 integration 测试与 `mc-http` 的 handler 都要用同一批数字**，
//! 而两者唯一的共同依赖就是领域层（`mc-cloud` 不依赖 `mc-repos`）。

use serde::{Deserialize, Serialize};

/// 本地 billing 面的路径前缀（8 条）。
pub const CLOUD_BILLING_PREFIX: &str = "/api/cloud-billing";
/// 本地 subscriptions 面的路径前缀（7 条）。
pub const CLOUD_SUBSCRIPTIONS_PREFIX: &str = "/api/cloud-subscriptions";
/// 云侧 billing 前缀（上游 `cloud_billing.go:357` 起）。
pub const BILLING_UPSTREAM_PREFIX: &str = "/api/v1/billing";
/// 云侧 subscriptions 前缀（上游 `cloud_billing.go:174` 起）。
pub const SUBSCRIPTIONS_UPSTREAM_PREFIX: &str = "/api/v1/subscriptions";
/// 云侧 stripe webhook 端点（上游 `cloud_billing.go:590`）。
pub const STRIPE_WEBHOOK_UPSTREAM_PATH: &str = "/api/v1/webhooks/stripe";

/// 代理请求体上限（上游 `maxCloudRuntimeRequestBodySize`，`cloud_runtime.go:19`）。
pub const MAX_CLOUD_RUNTIME_REQUEST_BODY_SIZE: usize = 1 << 20;
/// stripe 转发体上限（上游 `maxStripeWebhookBodySize`，`cloud_billing.go:41`）。
pub const MAX_STRIPE_WEBHOOK_BODY_SIZE: usize = 1 << 20;

/// 承载 Stripe 签名的头名（**逐字**，含大小写：上游 `stripeSignatureHeader`）。
pub const STRIPE_SIGNATURE_HEADER: &str = "Stripe-Signature";
/// 承载幂等键的头名（上游 `idempotencyKeyHeader`）。
pub const IDEMPOTENCY_KEY_HEADER: &str = "Idempotency-Key";
/// 幂等键上限（订阅 checkout / portal）。
pub const MAX_IDEMPOTENCY_KEY_LENGTH: usize = 255;
/// 幂等键上限（座位购买 —— **另一档**，上游刻意更短）。
pub const MAX_SEAT_PURCHASE_IDEMPOTENCY_KEY_LENGTH: usize = 200;

/// 订阅计费周期（上游只接受这两个值）。
pub const BILLING_INTERVALS: [&str; 2] = ["month", "year"];

/// 本地 checkout 请求体（上游 `cloudSubscriptionCheckoutRequest`）。
///
/// ⚠️ 与 [`CloudSubscriptionCheckoutUpstreamRequest`] **不是**同一个类型：
/// 上游把「转发哪些字段」做成**显式 allowlist**（注释逐字：「additive cloud fields are not
/// forwarded until the main repository explicitly reviews and adds them here」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudSubscriptionCheckoutRequest {
    /// `month` / `year`（[`BILLING_INTERVALS`]）。
    pub interval: String,
    /// 请求体里的幂等键（也可以走 [`IDEMPOTENCY_KEY_HEADER`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// 转发给云侧的 checkout 请求体（上游 `cloudSubscriptionCheckoutUpstreamRequest`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudSubscriptionCheckoutUpstreamRequest {
    /// **由中间件解析出来的**`workspace_id`（客户端走私值会被覆盖）。
    pub workspace_id: String,
    /// `month` / `year`。
    pub interval: String,
    /// 幂等键（已校验长度）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// 付款人的**账号邮箱**（上游从 `user.email` 读，不是请求体字段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_email: Option<String>,
}

/// 座位购买预览请求体（上游 `cloudSubscriptionSeatPurchasePreviewRequest`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudSubscriptionSeatPurchasePreviewRequest {
    /// **只**接受增量座位数（云侧读权威现值）。
    pub additional_seats: i32,
}

/// 座位购买请求体（上游 `cloudSubscriptionSeatPurchaseRequest`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudSubscriptionSeatPurchaseRequest {
    /// 增量座位数。
    pub additional_seats: i32,
    /// 乐观并发：客户端看到的**当前**座位数。
    pub expected_current_seats: i32,
    /// 乐观并发：客户端看到的**购买版本号**。
    pub expected_purchase_version: i64,
    /// 客户端接受的按比例补差金额（最小货币单位）。
    pub accepted_proration_amount: i64,
    /// 币种。
    pub currency: String,
    /// 幂等键（上限 [`MAX_SEAT_PURCHASE_IDEMPOTENCY_KEY_LENGTH`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// 代理的三个开关（上游 `cloudRuntimeProxyOptions`）—— 逐条路由不同，值得是一个**类型**
/// 而不是三个 `bool` 实参。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CloudRuntimeProxyOptions {
    /// 注入 `X-User-ID`（账号级面；stripe 转发是 `false`）。
    pub with_user_id: bool,
    /// 透传查询串。
    pub with_query: bool,
    /// 转发请求体（1 MiB 上限 + 必须是合法 JSON）。
    pub with_body: bool,
}

impl CloudRuntimeProxyOptions {
    /// 只有身份（`GET /api/v1/`（服务信息）、节点详情等）。
    #[must_use]
    pub const fn user_only() -> Self {
        Self {
            with_user_id: true,
            with_query: false,
            with_body: false,
        }
    }

    /// 身份 + 查询（列表面）。
    #[must_use]
    pub const fn user_and_query() -> Self {
        Self {
            with_user_id: true,
            with_query: true,
            with_body: false,
        }
    }

    /// 身份 + 体（写入面）。
    #[must_use]
    pub const fn user_and_body() -> Self {
        Self {
            with_user_id: true,
            with_query: false,
            with_body: true,
        }
    }

    /// 三者全不要（`/healthz` / `/readyz` —— 云侧的**服务**探针）。
    #[must_use]
    pub const fn none() -> Self {
        Self {
            with_user_id: false,
            with_query: false,
            with_body: false,
        }
    }
}

/// 计费周期是否合法（上游逐字 `in.Interval != "month" && in.Interval != "year"`）。
#[must_use]
pub fn is_valid_billing_interval(interval: &str) -> bool {
    BILLING_INTERVALS.contains(&interval)
}

/// 座位购买的乐观并发三件套（上游 `expected_current_seats` /
/// `expected_purchase_version` / `accepted_proration_amount`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeatPurchaseConcurrency {
    /// 客户端看到的当前座位数。
    pub expected_current_seats: i32,
    /// 客户端看到的购买版本号。
    pub expected_purchase_version: i64,
    /// 接受的按比例补差金额。
    pub accepted_proration_amount: i64,
}

impl SeatPurchaseConcurrency {
    /// 从请求体取。
    #[must_use]
    pub const fn from_request(request: &CloudSubscriptionSeatPurchaseRequest) -> Self {
        Self {
            expected_current_seats: request.expected_current_seats,
            expected_purchase_version: request.expected_purchase_version,
            accepted_proration_amount: request.accepted_proration_amount,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_limits_are_pinned_once() {
        assert_eq!(MAX_CLOUD_RUNTIME_REQUEST_BODY_SIZE, 1_048_576);
        assert_eq!(MAX_STRIPE_WEBHOOK_BODY_SIZE, 1_048_576);
        assert_eq!(MAX_IDEMPOTENCY_KEY_LENGTH, 255);
        assert_eq!(MAX_SEAT_PURCHASE_IDEMPOTENCY_KEY_LENGTH, 200);
        // 两档上限**不是**同一个数（上游刻意更短），别"顺手统一"。
        assert_ne!(
            MAX_IDEMPOTENCY_KEY_LENGTH,
            MAX_SEAT_PURCHASE_IDEMPOTENCY_KEY_LENGTH
        );
        assert_eq!(STRIPE_SIGNATURE_HEADER, "Stripe-Signature");
        assert_eq!(IDEMPOTENCY_KEY_HEADER, "Idempotency-Key");
    }

    #[test]
    fn upstream_paths_are_absolute_and_versioned() {
        for path in [
            CLOUD_BILLING_PREFIX,
            CLOUD_SUBSCRIPTIONS_PREFIX,
            BILLING_UPSTREAM_PREFIX,
            SUBSCRIPTIONS_UPSTREAM_PREFIX,
            STRIPE_WEBHOOK_UPSTREAM_PATH,
        ] {
            assert!(path.starts_with("/api/"), "{path}");
            assert!(!path.ends_with('/'), "{path}");
        }
        assert_eq!(STRIPE_WEBHOOK_UPSTREAM_PATH, "/api/v1/webhooks/stripe");
        assert_eq!(BILLING_UPSTREAM_PREFIX, "/api/v1/billing");
        assert_eq!(SUBSCRIPTIONS_UPSTREAM_PREFIX, "/api/v1/subscriptions");
    }

    #[test]
    fn interval_validation_matches_upstream() {
        assert!(is_valid_billing_interval("month"));
        assert!(is_valid_billing_interval("year"));
        assert!(!is_valid_billing_interval("week"));
        assert!(!is_valid_billing_interval("Month"));
        assert!(!is_valid_billing_interval(""));
    }

    #[test]
    fn proxy_options_presets_cover_the_four_shapes() {
        assert_eq!(
            CloudRuntimeProxyOptions::none(),
            CloudRuntimeProxyOptions::default()
        );
        assert!(CloudRuntimeProxyOptions::user_only().with_user_id);
        assert!(!CloudRuntimeProxyOptions::none().with_user_id);
        let list = CloudRuntimeProxyOptions::user_and_query();
        assert!(list.with_user_id && list.with_query && !list.with_body);
        let write = CloudRuntimeProxyOptions::user_and_body();
        assert!(write.with_user_id && write.with_body && !write.with_query);
    }

    #[test]
    fn checkout_dtos_round_trip_with_the_allowlist_shape() {
        let local = CloudSubscriptionCheckoutRequest {
            interval: "month".into(),
            idempotency_key: None,
        };
        let json = serde_json::to_value(&local).expect("serialize");
        assert_eq!(json, serde_json::json!({"interval": "month"}));
        // 上游注入体**多**两个字段（`workspace_id` / `customer_email`）。
        let upstream = CloudSubscriptionCheckoutUpstreamRequest {
            workspace_id: "ws".into(),
            interval: "year".into(),
            idempotency_key: Some("k".into()),
            customer_email: Some("payer@example.test".into()),
        };
        assert_eq!(
            serde_json::to_value(&upstream).expect("serialize"),
            serde_json::json!({
                "workspace_id": "ws",
                "interval": "year",
                "idempotency_key": "k",
                "customer_email": "payer@example.test",
            })
        );
    }

    #[test]
    fn seat_purchase_concurrency_is_projected_verbatim() {
        let request = CloudSubscriptionSeatPurchaseRequest {
            additional_seats: 2,
            expected_current_seats: 5,
            expected_purchase_version: 41,
            accepted_proration_amount: 1200,
            currency: "usd".into(),
            idempotency_key: Some("k".into()),
        };
        let concurrency = SeatPurchaseConcurrency::from_request(&request);
        assert_eq!(concurrency.expected_current_seats, 5);
        assert_eq!(concurrency.expected_purchase_version, 41);
        assert_eq!(concurrency.accepted_proration_amount, 1200);
    }
}
