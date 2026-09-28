//! stripe webhook 转发面的**出站契约** —— **写者 M9-6**（`LUM-1821` / `docs/62` §4.1 第 7 行）。
//!
//! 上游 `internal/handler/cloud_billing.go` 的 `L38–L48`（两个常量）+ `L520–L604`
//! （`HandleCloudBillingStripeWebhook`）。出站端点 **`/api/v1/webhooks/stripe`**
//! （`cloud_billing.go:590`），方法 `POST`。
//!
//! # 本地**只有**四段语义（`docs/62` §9.5；顺序 = 上游 `cloud_billing.go:520–L604` 的书写顺序）
//!
//! | 顺序 | 判据 | 结果 | 归属 |
//! | :-: | --- | --- | :-: |
//! | 1 | `MULTICA_CLOUD_URL` 未配置 | **403** `cloud_runtime_not_configured` | `mc-http`（`state.cloud`） |
//! | 2 | per-IP 限流超限 | **429** `rate limit exceeded` | `mc-http`（限流器是 HTTP 面的事） |
//! | 3 | 缺 `Stripe-Signature` | **401** | 本文件 [`has_stripe_signature`] |
//! | 4 | 体 > 1 MiB | **413** | `mc-http`（读体是 HTTP 面的事） |
//!
//! ⚠️ **403 在最前**（上游 `cloud_billing.go:521–524` 的第一个 `if`）。`docs/62` §9.5 把
//! 「per-IP 限流 / 签名 / 体上限」列成 ①②③，但那是**清单**不是顺序：403 由
//! `h.CloudRuntime == nil || !Enabled()` 产出，**先于**后面两段。顺序是判据的一部分
//! （`docs/62` §6.5 的 M9-6 行）。
//!
//! 四段之后是**原始体逐字**出站：不 `trim`、不 `json` 校验、不重编码（上游注释逐字：
//! 「the upstream signature check is computed over exactly what we received, so any
//! transformation here would silently break verification」）；且 **`X-User-ID` 不注入**
//! （上游逐字注释：「Intentionally no `UserID` — webhook is unauthenticated by design」）。
//!
//! # 两条**有意等价**（不是缺口，`docs/62` §9.5）
//!
//! 1. **不做本地验签**：签名校验在**云侧**（本地不读 `STRIPE_WEBHOOK_SECRET`、不自建 HMAC）
//!    —— 上游注释逐字：「We forward whatever the client sent verbatim; the cloud side is
//!    the one that knows the shared secret and rejects on mismatch」；
//! 2. **不做本地事件去重**：幂等由云侧事件 id 负责（上游 `TestStripeWebhookForwardsEmptyBody`
//!    与「同一请求重投 ⇒ 转发两次」共同钉住这一点）。
//!
//! # 一处**有意偏离上游字面量**（`DoD` 的判据，登记在 `docs/32` §9.13）
//!
//! 上游的代码是 `if len(r.Header.Values(stripeSignatureHeader)) == 0`，而它**上面那段注释**
//! 写的是「we use `Header.Values` to detect presence rather than `Get`, so a header
//! explicitly set to `""` still **counts as missing**」。两者在 Go 里**不一致**
//! （`Header.Set(k, "")` 之后 `Values(k)` 返回 `[""]`，长度 1 ⇒ 字面量代码判为**存在**）。
//! 本仓按 [`docs/62`] §6.5 的 M9-6 行（「`Header.Values` 语义（显式 `""` 也算缺失）」）
//! 跟**注释**、不跟字面量 ⇒ [`has_stripe_signature`] 把「无值」与「全空白值」折叠成 401。
//! 这与上游**云侧**的行为一致（缺签名 ⇒ 云侧也回 401），所以不会改变 Stripe 的投递视图。
//!
//! # 为什么这里**没有**响应类型
//!
//! 与 [`crate::billing`] 同款理由：云侧状态码与体**原样**写回客户端 ⇒ 建模就是第二个真相源。
//!
//! 形态纪律（`docs/62` §1.4 实测 `declared 34 / dual-form required: 3`）：本波**只有**
//! `/api/notification-preferences` 那 3 条需要补尾斜杠形态；本路由**只按上游字面量**
//! 注册 `/api/webhooks/stripe`（**单形态**）。

use crate::transport::Request;

/// 云侧端点（上游 `cloud_billing.go:590`）= `STRIPE_WEBHOOK_UPSTREAM_PATH`。
///
/// ⚠️ 数值本身**只**在 [`mc_core::cloud`] 里 pin 一次；这里 re-export 而不是重写，
/// 免得出现第二个真相源（与 [`crate::billing`] 对 `BILLING_UPSTREAM_PREFIX` 的处理同款）。
pub use mc_core::cloud::{
    MAX_STRIPE_WEBHOOK_BODY_SIZE, STRIPE_SIGNATURE_HEADER, STRIPE_WEBHOOK_UPSTREAM_PATH,
};

/// 出站计量标签（上游 `cloud_billing.go:596` 逐字：`Op: "billing"`）。
///
/// 显式给出而不是靠 [`crate::transport::infer_op`] 推导：路径 `/api/v1/webhooks/stripe`
/// 里没有 `/billing`，推导会落 `fleet` 桶 —— 那**不是**上游的口径。
pub const OP: &str = "billing";

/// `Stripe-Signature` **存在性**判定。
///
/// 实参是**全部**同名头的值（上游 `r.Header.Values(stripeSignatureHeader)` 的等价物）
/// ⇒ 两条判据：
///
/// 1. **多值保留**：上游把 `headers[stripeSignatureHeader] = sigs`（**全部**值）转给云侧，
///    所以这里只判存在性、**值由调用方逐值转发**，不做归并；
/// 2. **显式 `""`（以及纯空白）算缺失** ⇒ 401。见文件头「一处有意偏离上游字面量」。
#[must_use]
pub fn has_stripe_signature(values: &[&str]) -> bool {
    !values.is_empty() && values.iter().any(|value| !value.trim().is_empty())
}

/// 出站请求构造函数：`POST /api/v1/webhooks/stripe`，**原始体逐字**转发。
///
/// - `body`：**原样字节**，不做 `trim` / 不做 JSON 解析 / 不重编码；
/// - `signature`：`Stripe-Signature` 的**全部**值（上游 `headers[stripeSignatureHeader] = sigs`）；
/// - `content_type`：`Content-Type` 的**全部**值（上游注释逐字：「plus Stripe's original
///   Content-Type … putting Content-Type here is enough」）—— 上游那一条是**防御性**的
///   （`cloudruntime` 的默认 `Content-Type` 本来就会覆盖），本地照做；
/// - **恒不设 `user_id`** ⇒ `X-User-ID` **不注入**。
#[must_use]
pub fn stripe_webhook_request(
    body: &[u8],
    signatures: &[&str],
    content_types: &[&str],
    request_id: Option<&str>,
) -> Request {
    let mut request = Request::post(STRIPE_WEBHOOK_UPSTREAM_PATH)
        .with_op(OP)
        .with_body(body.to_vec());
    // 上游是 `http.Header` 的 map 赋值：同名多值**全部**转过去，一个不少。
    for signature in signatures {
        request = request.with_header(STRIPE_SIGNATURE_HEADER, *signature);
    }
    for content_type in content_types {
        request = request.with_header("Content-Type", *content_type);
    }
    if let Some(request_id) = request_id {
        request = request.with_request_id(request_id);
    }
    request
}

#[cfg(test)]
mod tests;
