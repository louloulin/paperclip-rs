//! subscriptions 面（7 条 workspace 级出站代理）的**出站契约** —— **写者 M9-2**（`LUM-1817`）。
//!
//! 上游 `internal/handler/cloud_billing.go` 的 `L54–L355`（7 条 subscription handler +
//! `requireCloudSubscriptionWorkspace` + `proxyCloudSubscription`）。同样**零本地表**
//! （`docs/62` §9.4：`cloud_subscription*` 在上游也是 0 张）。
//!
//! # 七条的出站契约（逐字取自 `cloud_billing.go:174/190/243/254/284/325/349`）
//!
//! | 本地路由 | 方法 | 出站路径 | 身份 | 体 / 头 | 授权 |
//! | --- | :-: | --- | :-: | --- | --- |
//! | `/api/cloud-subscriptions/summary` | GET | `/api/v1/subscriptions/{workspaceId}/summary` | ○ | — | **member 可读** |
//! | `/api/cloud-subscriptions/prices` | GET | `/api/v1/subscriptions/{workspaceId}/prices` | ○ | — | **member 可读** |
//! | `/api/cloud-subscriptions/checkout-sessions` | POST | `/api/v1/subscriptions/checkout-sessions` | ○ | **注入体** + 头 | owner\|admin |
//! | `/api/cloud-subscriptions/seats/reconcile` | POST | `…/{workspaceId}/seats/reconcile` | ○ | — | owner\|admin |
//! | `/api/cloud-subscriptions/seats/purchase-preview` | POST | `…/{workspaceId}/seats/purchase-preview` | ○ | 转发体 | owner\|admin |
//! | `/api/cloud-subscriptions/seats/purchases` | POST | `…/{workspaceId}/seats/purchases` | ○ | 转发体 + 头 | owner\|admin |
//! | `/api/cloud-subscriptions/portal-sessions` | POST | `…/{workspaceId}/portal-sessions` | ○ | — | owner\|admin |
//!
//! 七条**全部**注入 `X-User-ID`（云侧仍是最终授权方：每次 mutation 前重新校验 membership）。
//! 计量标签 [`OP`] 与 billing 面**同一个桶**（上游 `inferOp` 对 `/api/v1/billing/*` 与
//! `/api/v1/subscriptions/*` 都推 `billing`）。
//!
//! # 三条只属于本片的契约（`docs/62` §2.7 / §4.2）
//!
//! 1. 🔴 **`workspace_id` 由服务端解析并注入请求体**（上游逐字：「A caller cannot smuggle a
//!    different workspace or Stripe payer identity through JSON」）。本文件把这一点做成**类型**：
//!    三个路径构造器收 [`Id`] 而不是 `&str` —— `Id` 的 `Display` 是规范化 UUID，
//!    **结构上**不可能拼出 `../`（对比：billing 的 `{sessionId}` 是客户端字符串，必须走
//!    allowlist）。注入体则走 [`CloudSubscriptionCheckoutUpstreamRequest`] 的**显式 allowlist**
//!    （本地请求 DTO 与上游 DTO **不是**同一个类型 ⇒ 客户端在 JSON 里传的 `workspace_id` /
//!    `customer_email` 在反序列化那一步就被丢掉）。
//! 2. **`Idempotency-Key` 两档上限**（[`MAX_IDEMPOTENCY_KEY_LENGTH`] = 255 /
//!    [`MAX_SEAT_PURCHASE_IDEMPOTENCY_KEY_LENGTH`] = 200），且**转发口只有两个**：
//!    [`forwarded_idempotency_key`]（上游 `cloudSubscriptionIdempotencyHeaders`：**只**看请求头）
//!    与座位购买那条显式转发的**已解析键**。
//! 3. **座位购买的三个乐观并发字段逐字透传**（`expected_current_seats` /
//!    `expected_purchase_version` / `accepted_proration_amount`）：它们**不是**校验项，
//!    是"客户端看到的现值"⇒ 本文件只负责把它们序列化进转发体。
//!
//! # 为什么本文件**没有**响应类型
//!
//! 同 [`crate::billing`]：上游 `writeCloudRuntimeResponse` 把云侧状态码与体**原样**写回
//! 客户端（`docs/62` §2.6），建模了就是第二个真相源。
//!
//! # 错误映射（本文件只造请求，状态码由 handler 写）
//!
//! 见 `docs/62` §2.6 的四行；handler 侧实现与 [`crate::billing`] 的那份**同形但独立**
//! （本仓约定：各切片各自持有本地副本，不去改别人写集里的文件）。
//!
//! 形态纪律（`docs/62` §1.4 实测 `dual-form required: 0`）：7 条**只按上游字面量注册**
//! 那一形态；本文件不涉及注册点。

use mc_core::cloud::{
    CloudSubscriptionCheckoutUpstreamRequest, CloudSubscriptionSeatPurchasePreviewRequest,
    CloudSubscriptionSeatPurchaseRequest, IDEMPOTENCY_KEY_HEADER, SUBSCRIPTIONS_UPSTREAM_PREFIX,
};
use mc_core::Id;

use crate::transport::Request;

/// 出站计量标签（上游 `inferOp` 对 `/api/v1/subscriptions/*` 推出来的桶名）。
pub const OP: &str = "billing";

// ---------------------------------------------------------------------------
// 云侧路径
// ---------------------------------------------------------------------------

/// 云侧路径的**后缀**（前缀一律取 [`SUBSCRIPTIONS_UPSTREAM_PREFIX`]）。
///
/// 与 [`crate::billing`] 同样的理由：前缀是 `mc-core` 里 pin 住的唯一常量，
/// 这里再写全串就是第二个真相源。
const SUMMARY_SUFFIX: &str = "/summary";
const PRICES_SUFFIX: &str = "/prices";
const CHECKOUT_SESSIONS_SUFFIX: &str = "/checkout-sessions";
const SEATS_RECONCILE_SUFFIX: &str = "/seats/reconcile";
const PURCHASE_PREVIEW_SUFFIX: &str = "/seats/purchase-preview";
const PURCHASES_SUFFIX: &str = "/seats/purchases";
const PORTAL_SESSIONS_SUFFIX: &str = "/portal-sessions";

/// workspace **作用域**路径的唯一拼接点：`/api/v1/subscriptions/{uuid}{suffix}`。
///
/// 🔴 实参是 [`Id`] 而不是 `&str` —— 这是「客户端不能走私 workspace」这条契约的**类型**形态：
/// `Id` 的 `Display` 只会产出规范化 UUID（36 字符、hyphen 分组、小写 hex），
/// `../` / `?` / `%2f` 在类型层面就进不来（上游用 `util.ParseUUID` + 字符串拼）
/// ⇒ 这里**不需要** allowlist（与 billing 的 `{sessionId}` 相反）。
#[must_use]
fn workspace_path(workspace_id: Id, suffix: &str) -> String {
    format!("{SUBSCRIPTIONS_UPSTREAM_PREFIX}/{workspace_id}{suffix}")
}

/// 不含 workspace 段的那一条（checkout 的云侧路径由 workspace 无关的 endpoint 承担：
/// workspace 走**注入体**，见 [`checkout_body`]）。
#[must_use]
fn upstream_path(suffix: &str) -> String {
    format!("{SUBSCRIPTIONS_UPSTREAM_PREFIX}{suffix}")
}

/// `GET /api/v1/subscriptions/{workspace_id}/summary`。
#[must_use]
pub fn summary_path(workspace_id: Id) -> String {
    workspace_path(workspace_id, SUMMARY_SUFFIX)
}

/// `GET /api/v1/subscriptions/{workspace_id}/prices`。
#[must_use]
pub fn prices_path(workspace_id: Id) -> String {
    workspace_path(workspace_id, PRICES_SUFFIX)
}

/// `POST /api/v1/subscriptions/{workspace_id}/seats/reconcile`。
#[must_use]
pub fn seats_reconcile_path(workspace_id: Id) -> String {
    workspace_path(workspace_id, SEATS_RECONCILE_SUFFIX)
}

/// `POST /api/v1/subscriptions/{workspace_id}/seats/purchase-preview`。
#[must_use]
pub fn seat_purchase_preview_path(workspace_id: Id) -> String {
    workspace_path(workspace_id, PURCHASE_PREVIEW_SUFFIX)
}

/// `POST /api/v1/subscriptions/{workspace_id}/seats/purchases`。
#[must_use]
pub fn seat_purchases_path(workspace_id: Id) -> String {
    workspace_path(workspace_id, PURCHASES_SUFFIX)
}

/// `POST /api/v1/subscriptions/{workspace_id}/portal-sessions`。
#[must_use]
pub fn portal_sessions_path(workspace_id: Id) -> String {
    workspace_path(workspace_id, PORTAL_SESSIONS_SUFFIX)
}

/// `POST /api/v1/subscriptions/checkout-sessions`（**无** workspace 段）。
#[must_use]
pub fn checkout_sessions_path() -> String {
    upstream_path(CHECKOUT_SESSIONS_SUFFIX)
}

// ---------------------------------------------------------------------------
// 注入体 / 转发体（三条纯函数；形状由 `mc-core` 的 DTO 固定）
// ---------------------------------------------------------------------------

/// checkout 的**注入体**（上游 `cloudSubscriptionCheckoutUpstreamRequest` 的 `json.Marshal`）。
///
/// 四个字段里有两个**由服务端填**：`workspace_id`（中间件解析出来的那个）与
/// `customer_email`（账号邮箱，**不是**请求体字段）。客户端的同名 JSON 字段在**反序列化**
/// 那一步就被丢掉 ⇒ 覆盖是**结构上**发生的，不是"记得覆盖"。
#[must_use]
pub fn checkout_body(
    workspace_id: Id,
    interval: &str,
    idempotency_key: Option<&str>,
    customer_email: &str,
) -> Vec<u8> {
    let body = CloudSubscriptionCheckoutUpstreamRequest {
        workspace_id: workspace_id.as_string(),
        interval: interval.to_string(),
        idempotency_key: idempotency_key.map(str::to_string),
        customer_email: Some(customer_email.to_string()),
    };
    serde_json::to_vec(&body).expect("DTO 的序列化不会失败")
}

/// 座位**购买预览**的转发体（上游把解析出来的结构体**重新 marshal**）。
///
/// ⇒ 客户端在 JSON 里多传的字段（`workspace_id` / `target_seats` / …）**不转发**：
/// 上游 DTO 只有 `additional_seats` 一个字段（实测断言逐字 `{"additional_seats":10001}`）。
#[must_use]
pub fn seat_purchase_preview_body(additional_seats: i32) -> Vec<u8> {
    let body = CloudSubscriptionSeatPurchasePreviewRequest { additional_seats };
    serde_json::to_vec(&body).expect("DTO 的序列化不会失败")
}

/// 座位**购买**的转发体（已过校验、币种已小写、幂等键已解析）。
///
/// 🔴 三个乐观并发字段（[`CloudSubscriptionSeatPurchaseRequest::expected_current_seats`] /
/// `expected_purchase_version` / `accepted_proration_amount`）**逐字**进体：它们是
/// "客户端看到的现值"，本地**不**解释、**不**重算（云侧才是权威）。
#[must_use]
pub fn seat_purchase_body(purchase: &CloudSubscriptionSeatPurchaseRequest) -> Vec<u8> {
    serde_json::to_vec(purchase).expect("DTO 的序列化不会失败")
}

// ---------------------------------------------------------------------------
// 幂等键（上游的两个 helper，逐字）
// ---------------------------------------------------------------------------

/// 上游 `strings.TrimSpace` 的那一步：空白 / 空串 ⇒ `None`（"没给"）。
#[must_use]
pub fn trim_idempotency_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 上游 `cloudSubscriptionIdempotencyHeaders`：转发的头**只**取请求头的值（trim 后）。
///
/// ⚠️ 这是本片最容易写错的一处：checkout 的**键**可以来自请求体（`idempotency_key`），
/// 但**转发的头**只看请求头 —— 客户端只给了体键时，上游**不**发这个头。座位购买相反
/// （它显式转发已解析的键）。两条行为都在 handler 侧有逐条用例。
#[must_use]
pub fn forwarded_idempotency_key(header_value: Option<&str>) -> Option<String> {
    header_value.and_then(trim_idempotency_key)
}

/// 幂等键的解析结果（上游 `requireCloudSubscriptionIdempotencyKey` 的两条 400）。
///
/// 状态码与文案由 handler 决定（本文件不做 HTTP）；这里只把**判定**提成纯函数，
/// 好让"体键优先 / 头键兜底 / 两档上限"这三件事各自被钉住。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdempotencyKeyError {
    /// 体键与头键都（trim 后）为空。
    Missing,
    /// 超过 `limit` 字节（Go 的 `len(key)` 是**字节**数）。
    TooLong { limit: usize },
}

/// 上游 `requireCloudSubscriptionIdempotencyKey`：**体键优先，头键兜底**，再判上限。
///
/// # Errors
///
/// 两条都为空 ⇒ [`IdempotencyKeyError::Missing`]；超过 `limit` 字节 ⇒
/// [`IdempotencyKeyError::TooLong`]。判定顺序与上游逐字一致（先判空、再判长）。
pub fn resolve_idempotency_key(
    body_key: Option<&str>,
    header_key: Option<&str>,
    limit: usize,
) -> Result<String, IdempotencyKeyError> {
    let key = body_key
        .and_then(trim_idempotency_key)
        .or_else(|| header_key.and_then(trim_idempotency_key))
        .ok_or(IdempotencyKeyError::Missing)?;
    if key.len() > limit {
        return Err(IdempotencyKeyError::TooLong { limit });
    }
    Ok(key)
}

// ---------------------------------------------------------------------------
// 请求构造函数（7 条，一人一个）
// ---------------------------------------------------------------------------

/// 一个出站请求的公共骨架：方法 + 路径 + **身份** + 计量标签（+ 可选 `X-Request-ID`）。
fn proxy_request(
    method: &reqwest::Method,
    path: String,
    user_id: Id,
    request_id: Option<&str>,
) -> Request {
    Request {
        method: method.clone(),
        path,
        op: Some(OP.to_string()),
        user_id: Some(user_id),
        request_id: request_id.map(str::to_string),
        ..Request::default()
    }
}

/// 转发 `Idempotency-Key`（`None` ⇒ 一个头都不加）。
fn with_forwarded_key(request: Request, key: Option<&str>) -> Request {
    match key {
        Some(key) => request.with_header(IDEMPOTENCY_KEY_HEADER, key),
        None => request,
    }
}

/// `GET /api/v1/subscriptions/{workspace_id}/summary`（上游 `GetCloudWorkspaceSubscriptionSummary`）。
#[must_use]
pub fn summary_request(workspace_id: Id, user_id: Id, request_id: Option<&str>) -> Request {
    proxy_request(
        &reqwest::Method::GET,
        summary_path(workspace_id),
        user_id,
        request_id,
    )
}

/// `GET /api/v1/subscriptions/{workspace_id}/prices`（上游 `GetCloudWorkspaceSubscriptionPrices`）。
#[must_use]
pub fn prices_request(workspace_id: Id, user_id: Id, request_id: Option<&str>) -> Request {
    proxy_request(
        &reqwest::Method::GET,
        prices_path(workspace_id),
        user_id,
        request_id,
    )
}

/// `POST /api/v1/subscriptions/checkout-sessions`（上游 `CreateCloudWorkspaceSubscriptionCheckout`）。
///
/// `forwarded_key` = [`forwarded_idempotency_key`] 的结果（**只**看请求头）。
#[must_use]
pub fn checkout_session_request(
    user_id: Id,
    request_id: Option<&str>,
    body: Vec<u8>,
    forwarded_key: Option<&str>,
) -> Request {
    // workspace 只出现在**体**里 ⇒ 这里用不带 workspace 段的路径（上游逐字）。
    with_forwarded_key(
        proxy_request(
            &reqwest::Method::POST,
            checkout_sessions_path(),
            user_id,
            request_id,
        )
        .with_body(body),
        forwarded_key,
    )
}

/// `POST /api/v1/subscriptions/{workspace_id}/seats/reconcile`（上游 `ReconcileCloudWorkspaceSubscriptionSeats`）。
///
/// ⚠️ 上游这一条**既**不要求幂等键、**也**不转发它（`cloud_billing.go:254` 传的实参是 `nil`，
/// **不是** `cloudSubscriptionIdempotencyHeaders(r)`）⇒ 本函数**没有**幂等键实参。
#[must_use]
pub fn seats_reconcile_request(workspace_id: Id, user_id: Id, request_id: Option<&str>) -> Request {
    proxy_request(
        &reqwest::Method::POST,
        seats_reconcile_path(workspace_id),
        user_id,
        request_id,
    )
}

/// `POST …/seats/purchase-preview`（上游 `PreviewCloudWorkspaceSubscriptionSeatPurchase`）。
///
/// 上游这条**不**转发幂等键（`nil`）。
#[must_use]
pub fn seat_purchase_preview_request(
    workspace_id: Id,
    user_id: Id,
    request_id: Option<&str>,
    body: Vec<u8>,
) -> Request {
    proxy_request(
        &reqwest::Method::POST,
        seat_purchase_preview_path(workspace_id),
        user_id,
        request_id,
    )
    .with_body(body)
}

/// `POST …/seats/purchases`（上游 `PurchaseCloudWorkspaceSubscriptionSeats`）。
///
/// 与 checkout 相反：这条**显式转发已解析的键**（体键也能走到头上）。
#[must_use]
pub fn seat_purchase_request(
    workspace_id: Id,
    user_id: Id,
    request_id: Option<&str>,
    body: Vec<u8>,
    idempotency_key: &str,
) -> Request {
    proxy_request(
        &reqwest::Method::POST,
        seat_purchases_path(workspace_id),
        user_id,
        request_id,
    )
    .with_body(body)
    .with_header(IDEMPOTENCY_KEY_HEADER, idempotency_key)
}

/// `POST …/portal-sessions`（上游 `CreateCloudWorkspaceSubscriptionPortal`）。
#[must_use]
pub fn portal_session_request(
    workspace_id: Id,
    user_id: Id,
    request_id: Option<&str>,
    forwarded_key: Option<&str>,
) -> Request {
    with_forwarded_key(
        proxy_request(
            &reqwest::Method::POST,
            portal_sessions_path(workspace_id),
            user_id,
            request_id,
        ),
        forwarded_key,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> Id {
        Id::parse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").expect("uuid")
    }

    fn user() -> Id {
        Id::parse("11111111-2222-3333-4444-555555555555").expect("uuid")
    }

    /// 七条路径**逐条**产出上游那一条（前缀 = `mc-core` 的唯一常量），且两两不同。
    #[test]
    fn upstream_prefix_is_the_only_source_of_the_seven_paths() {
        let cases: Vec<String> = vec![
            summary_request(ws(), user(), None).path,
            prices_request(ws(), user(), None).path,
            checkout_session_request(user(), None, vec![], None).path,
            seats_reconcile_request(ws(), user(), None).path,
            seat_purchase_preview_request(ws(), user(), None, vec![]).path,
            seat_purchase_request(ws(), user(), None, vec![], "k").path,
            portal_session_request(ws(), user(), None, None).path,
        ];
        let expected = [
            "/api/v1/subscriptions/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/summary",
            "/api/v1/subscriptions/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/prices",
            "/api/v1/subscriptions/checkout-sessions",
            "/api/v1/subscriptions/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/seats/reconcile",
            "/api/v1/subscriptions/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/seats/purchase-preview",
            "/api/v1/subscriptions/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/seats/purchases",
            "/api/v1/subscriptions/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/portal-sessions",
        ];
        for (actual, want) in cases.iter().zip(expected) {
            assert_eq!(actual, want);
            assert!(actual.starts_with(SUBSCRIPTIONS_UPSTREAM_PREFIX));
        }
        let mut unique = cases.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 7, "七条路径两两不同（防复制粘贴漏改）");
    }

    /// 🔴 workspace 段是**类型**保证的：`Id` 只会产出规范化 UUID ⇒ 结构上没有注入面。
    #[test]
    fn the_workspace_segment_cannot_be_injected_through_the_type() {
        let hostile = Id::parse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").expect("uuid");
        let path = summary_path(hostile);
        assert!(!path.contains(".."));
        assert!(!path.contains('?'));
        assert!(!path.contains('#'));
        assert!(!path.contains('%'));
        assert_eq!(
            path.matches('/').count(),
            5,
            "段数固定：/api/v1/subscriptions/<id>/summary"
        );
        // 同一个 `Id` 在任意构造器里都产出同一段。
        for path in [
            prices_path(hostile),
            seats_reconcile_path(hostile),
            seat_purchase_preview_path(hostile),
            seat_purchases_path(hostile),
            portal_sessions_path(hostile),
        ] {
            assert!(path.contains(&hostile.to_string()), "{path}");
        }
    }

    /// 七条**全部**盖章身份与计量标签；只有 checkout / 座位购买转发 `Idempotency-Key`。
    #[test]
    fn every_request_stamps_identity_and_only_two_forward_the_key() {
        let requests = [
            summary_request(ws(), user(), Some("rid-1")),
            prices_request(ws(), user(), Some("rid-1")),
            checkout_session_request(user(), Some("rid-1"), b"{}".to_vec(), Some("k1")),
            seats_reconcile_request(ws(), user(), Some("rid-1")),
            seat_purchase_preview_request(ws(), user(), Some("rid-1"), b"{}".to_vec()),
            seat_purchase_request(ws(), user(), Some("rid-1"), b"{}".to_vec(), "k2"),
            portal_session_request(ws(), user(), Some("rid-1"), Some("k3")),
        ];
        for request in &requests {
            assert_eq!(request.user_id, Some(user()));
            assert_eq!(request.request_id.as_deref(), Some("rid-1"));
            assert_eq!(request.op.as_deref(), Some(OP));
        }
        let key_of = |request: &Request| {
            request
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(IDEMPOTENCY_KEY_HEADER))
                .map(|(_, value)| value.clone())
        };
        assert_eq!(key_of(&requests[0]), None);
        assert_eq!(key_of(&requests[1]), None);
        assert_eq!(key_of(&requests[2]).as_deref(), Some("k1"));
        // 上游 reconcile / preview 传的是 `nil` ⇒ 一个头都不加（客户端给了也不转发）。
        assert_eq!(key_of(&requests[3]), None);
        assert_eq!(key_of(&requests[4]), None);
        assert_eq!(key_of(&requests[5]).as_deref(), Some("k2"));
        assert_eq!(key_of(&requests[6]).as_deref(), Some("k3"));
        // 没给 request id ⇒ 不盖章。
        assert_eq!(summary_request(ws(), user(), None).request_id, None);
    }

    /// 方法：2 条 GET + 5 条 POST；只有 3 条带体。
    #[test]
    fn methods_and_bodies_match_the_seven_upstream_handlers() {
        assert_eq!(
            summary_request(ws(), user(), None).method,
            reqwest::Method::GET
        );
        assert_eq!(
            prices_request(ws(), user(), None).method,
            reqwest::Method::GET
        );
        for request in [
            checkout_session_request(user(), None, b"{}".to_vec(), None),
            seats_reconcile_request(ws(), user(), None),
            seat_purchase_preview_request(ws(), user(), None, b"{}".to_vec()),
            seat_purchase_request(ws(), user(), None, b"{}".to_vec(), "k"),
            portal_session_request(ws(), user(), None, None),
        ] {
            assert_eq!(request.method, reqwest::Method::POST);
        }
        assert!(seats_reconcile_request(ws(), user(), None).body.is_none());
        assert!(portal_session_request(ws(), user(), None, None)
            .body
            .is_none());
        assert!(
            seat_purchase_request(ws(), user(), None, b"{}".to_vec(), "k")
                .body
                .is_some()
        );
    }

    /// 🔴 注入体的**四个字段逐字**：workspace / interval / 幂等键 / 账号邮箱。
    #[test]
    fn the_checkout_body_carries_the_injected_workspace_and_the_payer_email() {
        let bytes = checkout_body(ws(), "year", Some("checkout-1"), "payer@example.test");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("json"),
            serde_json::json!({
                "workspace_id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                "interval": "year",
                "idempotency_key": "checkout-1",
                "customer_email": "payer@example.test",
            })
        );
        // 没有幂等键 ⇒ 该字段**不出现**（`skip_serializing_if`，与 Go 的 `omitempty` 同款）。
        let bytes = checkout_body(ws(), "month", None, "payer@example.test");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("json"),
            serde_json::json!({
                "workspace_id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                "interval": "month",
                "customer_email": "payer@example.test",
            })
        );
    }

    /// 预览体**只有** `additional_seats`（上游实测逐字 `{"additional_seats":10001}`）。
    #[test]
    fn the_preview_body_is_exactly_the_additive_seat_count() {
        assert_eq!(
            String::from_utf8(seat_purchase_preview_body(10_001)).expect("utf8"),
            r#"{"additional_seats":10001}"#
        );
    }

    /// 🔴 三个乐观并发字段**逐字**进购买体（本片 `DoD` 第 4 条）。
    #[test]
    fn the_purchase_body_passes_the_three_concurrency_fields_verbatim() {
        let purchase = CloudSubscriptionSeatPurchaseRequest {
            additional_seats: 2,
            expected_current_seats: 5,
            expected_purchase_version: 41,
            accepted_proration_amount: 425,
            currency: "usd".into(),
            idempotency_key: Some("seat-1".into()),
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&seat_purchase_body(&purchase))
                .expect("json"),
            serde_json::json!({
                "additional_seats": 2,
                "expected_current_seats": 5,
                "expected_purchase_version": 41,
                "accepted_proration_amount": 425,
                "currency": "usd",
                "idempotency_key": "seat-1",
            })
        );
    }

    /// 幂等键：体键优先、头键兜底、两档上限（字节数、不是字符数）。
    #[test]
    fn idempotency_keys_prefer_the_body_and_respect_both_limits() {
        assert_eq!(
            resolve_idempotency_key(Some("body"), Some("header"), 255),
            Ok("body".to_string())
        );
        assert_eq!(
            resolve_idempotency_key(None, Some("header"), 255),
            Ok("header".to_string())
        );
        // 体键是空白 ⇒ 落到头键（上游 `strings.TrimSpace` 的判空）。
        assert_eq!(
            resolve_idempotency_key(Some("   "), Some(" header "), 255),
            Ok("header".to_string())
        );
        assert_eq!(
            resolve_idempotency_key(None, None, 255),
            Err(IdempotencyKeyError::Missing)
        );
        assert_eq!(
            resolve_idempotency_key(Some("  "), Some("\t\n"), 255),
            Err(IdempotencyKeyError::Missing)
        );
        let at_limit = "a".repeat(255);
        assert_eq!(
            resolve_idempotency_key(None, Some(&at_limit), 255),
            Ok(at_limit.clone())
        );
        assert_eq!(
            resolve_idempotency_key(None, Some(&"a".repeat(256)), 255),
            Err(IdempotencyKeyError::TooLong { limit: 255 })
        );
        // 第二档：同一个 201 字节的键在 255 下合法、在 200 下超限（上游刻意更短）。
        let seat = "a".repeat(201);
        assert!(resolve_idempotency_key(None, Some(&seat), 255).is_ok());
        assert_eq!(
            resolve_idempotency_key(None, Some(&seat), 200),
            Err(IdempotencyKeyError::TooLong { limit: 200 })
        );
        // 字节数（不是字符数）：一个 3 字节字符重复 86 次 = 258 字节。
        let wide = "中".repeat(86);
        assert_eq!(wide.chars().count(), 86);
        assert_eq!(wide.len(), 258);
        assert_eq!(
            resolve_idempotency_key(None, Some(&wide), 255),
            Err(IdempotencyKeyError::TooLong { limit: 255 })
        );
    }

    /// 转发的头**只**看请求头那一路（上游 `cloudSubscriptionIdempotencyHeaders`）。
    #[test]
    fn the_forwarded_header_only_looks_at_the_request_header() {
        assert_eq!(forwarded_idempotency_key(Some("  k1  ")), Some("k1".into()));
        assert_eq!(forwarded_idempotency_key(Some("   ")), None);
        assert_eq!(forwarded_idempotency_key(Some("")), None);
        assert_eq!(forwarded_idempotency_key(None), None);
        // 体键**不会**从这里冒出来（这是 handler 侧那条用例的纯函数形态）。
        assert_eq!(
            resolve_idempotency_key(Some("from-body"), None, 255),
            Ok("from-body".into())
        );
        assert_eq!(forwarded_idempotency_key(None), None);
    }

    /// `Debug` 不暴露头**值**（`Idempotency-Key` 是凭据面，判据 ①）。
    #[test]
    fn request_debug_never_prints_the_idempotency_key() {
        let request = seat_purchase_request(
            ws(),
            user(),
            Some("rid-1"),
            b"{}".to_vec(),
            "secret-idempotency-key",
        );
        let rendered = format!("{request:?}");
        assert!(!rendered.contains("secret-idempotency-key"), "{rendered}");
        assert!(rendered.contains("header_names"), "{rendered}");
    }
}
