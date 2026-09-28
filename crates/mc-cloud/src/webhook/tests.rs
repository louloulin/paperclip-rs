//! M9-6 的出站契约判据（`crates/mc-cloud/src/webhook.rs` 的测试面）。

use super::*;

/// 三个常量逐字取自 `mc-core`（**不是**本文件重写）：上游 `cloud_billing.go:41/48/590`。
#[test]
fn constants_come_from_mc_core_verbatim() {
    assert_eq!(STRIPE_WEBHOOK_UPSTREAM_PATH, "/api/v1/webhooks/stripe");
    assert_eq!(MAX_STRIPE_WEBHOOK_BODY_SIZE, 1_048_576);
    assert_eq!(STRIPE_SIGNATURE_HEADER, "Stripe-Signature");
}

/// 计量标签逐字取上游 `Op: "billing"`（`cloud_billing.go:596`）—— **不是** `infer_op`
/// 从路径推出来的 `fleet`（路径里没有 `/billing`）。
#[test]
fn op_is_billing_verbatim_not_inferred() {
    let request = stripe_webhook_request(b"{}", &["sig"], &[], None);
    assert_eq!(request.op.as_deref(), Some(OP));
    assert_eq!(OP, "billing");
    // 反证：若不显式给 op，推导桶会是 `fleet` —— 所以显式给是**必需**的。
    assert_eq!(
        crate::transport::infer_op(None, &request.method, &request.path),
        "fleet"
    );
}

/// 出站请求的形状：路径 / 方法 / **不注入 `X-User-ID`**。
#[test]
fn request_shape_matches_upstream() {
    let request = stripe_webhook_request(b"{\"id\":\"evt\"}", &["t=1,v1=deadbeef"], &[], None);
    assert_eq!(request.method, reqwest::Method::POST);
    assert_eq!(request.path, "/api/v1/webhooks/stripe");
    assert_eq!(request.user_id, None, "stripe 转发没有人类身份");
    assert_eq!(request.request_id, None);
    assert_eq!(
        request.headers,
        vec![(
            "Stripe-Signature".to_string(),
            "t=1,v1=deadbeef".to_string()
        )]
    );
}

/// **原始体逐字**：不是合法 JSON、带首尾空白、带 NUL 字节的体都必须**一个字节不差**地出去。
#[test]
fn raw_body_is_forwarded_byte_for_byte() {
    // 故意挑一个「trim 会改它、`json()` 会拒它」的体。
    let raw = b"  {\n \"id\" : \"evt\" ,\n\t\"data\":{\"n\":1}\n}  \n\x00";
    let request = stripe_webhook_request(raw, &["sig"], &[], None);
    assert_eq!(request.body.as_deref(), Some(raw.as_slice()));
    assert!(
        serde_json::from_slice::<serde_json::Value>(request.body.as_ref().unwrap()).is_err(),
        "这个体刻意不是合法 JSON —— 逐字转发不许碰它"
    );
}

/// 空体也要发出去（上游 `TestStripeWebhookForwardsEmptyBody`：Stripe 的 tester 会发空 ping，
/// 本地**不**预判空体）。
#[test]
fn empty_body_is_not_dropped() {
    let request = stripe_webhook_request(b"", &["sig"], &[], None);
    assert_eq!(request.body.as_deref(), Some(b"".as_slice()));
}

/// 同名**多值**头一个不少地转过去（上游 `headers[k] = sigs` 是 map 赋值，不是 `Get`）。
#[test]
fn multi_valued_headers_are_all_forwarded() {
    let request = stripe_webhook_request(
        b"{}",
        &["t=1,v1=a", "t=1,v1=b"],
        &["application/json", "application/json; charset=utf-8"],
        None,
    );
    assert_eq!(
        request.headers,
        vec![
            ("Stripe-Signature".to_string(), "t=1,v1=a".to_string()),
            ("Stripe-Signature".to_string(), "t=1,v1=b".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "Content-Type".to_string(),
                "application/json; charset=utf-8".to_string()
            ),
        ]
    );
}

/// `Content-Type` 没有就不带（**不**自己编一个）。
#[test]
fn content_type_is_absent_when_the_caller_sent_none() {
    let request = stripe_webhook_request(b"{}", &["sig"], &[], None);
    assert!(!request
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type")));
}

/// `X-Request-ID` 只在调用方给了的时候才盖章（`transport` 会把它设成**不可覆盖**的戳）。
#[test]
fn request_id_is_stamped_only_when_given() {
    let request = stripe_webhook_request(b"{}", &["sig"], &[], Some("req-1"));
    assert_eq!(request.request_id.as_deref(), Some("req-1"));
}

/// `Debug` 不得回显签名与体（`transport` 的脱敏判据，`docs/62` §2.4）。
#[test]
fn debug_does_not_leak_signature_or_body() {
    let request = stripe_webhook_request(b"{\"secret\":1}", &["t=1,v1=deadbeef"], &[], None);
    let rendered = format!("{request:?}");
    assert!(!rendered.contains("deadbeef"), "{rendered}");
    assert!(!rendered.contains("secret"), "{rendered}");
}

/// `has_stripe_signature`：存在性判定的**全部**形态。
///
/// - 头不存在（`values` 为空）⇒ 缺失；
/// - 显式 `""` / 纯空白 ⇒ **也算缺失**（跟上游**注释**的意图，见文件头的偏离登记）；
/// - 多值头里**任一**非空白即算存在（值由调用方逐值转发，本函数不归并）。
#[test]
fn signature_presence_folds_absent_and_empty() {
    assert!(!has_stripe_signature(&[]), "头不存在");
    assert!(!has_stripe_signature(&[""]), "显式空串");
    assert!(!has_stripe_signature(&["   \t"]), "纯空白");
    assert!(!has_stripe_signature(&["", ""]), "全空多值");
    assert!(has_stripe_signature(&["t=1,v1=deadbeef"]));
    assert!(
        has_stripe_signature(&["", "t=1,v1=deadbeef"]),
        "多值里有一个非空"
    );
    assert!(has_stripe_signature(&["t=1,v1=a", "t=1,v1=b"]));
}

/// 拼进 URL 的路径必须是**字面量**（这里不接受任何调用方提供的路径段）。
#[test]
fn upstream_path_has_no_callable_segments() {
    let request = stripe_webhook_request(b"{}", &["sig"], &[], None);
    assert!(request.path.starts_with('/'));
    assert!(!request.path.contains('{'));
    assert!(!request.path.contains(".."));
}
