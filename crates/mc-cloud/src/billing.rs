//! billing 面（8 条 owner-credit 出站代理）的**出站契约** —— **写者 M9-1**（`LUM-1816`）。
//!
//! 上游 `internal/handler/cloud_billing.go` 的 `L356–L503`（8 条 billing handler）。
//! 全部是 `proxyCloudRuntime` 的**纯透传**：本地不落任何表（`docs/62` §9.4：
//! 本地与上游都是 **0 张** `cloud_billing_*` / `checkout_session` / `topup` 表）。
//!
//! # 八条的出站契约（逐字取自 `cloud_billing.go:357/367/378/386/400/411/439/477`）
//!
//! | 本地路由 | 出站 | 身份 | query | body |
//! | --- | --- | :-: | :-: | :-: |
//! | `GET /api/cloud-billing/balance` | `GET /api/v1/billing/balance` | ○ | — | — |
//! | `GET /api/cloud-billing/transactions` | `GET /api/v1/billing/transactions` | ○ | 透传 | — |
//! | `GET /api/cloud-billing/batches` | `GET /api/v1/billing/batches` | ○ | 透传 | — |
//! | `GET /api/cloud-billing/topups` | `GET /api/v1/billing/topups` | ○ | 透传 | — |
//! | `GET /api/cloud-billing/price-tiers` | `GET /api/v1/billing/price-tiers` | ○ | — | — |
//! | `POST /api/cloud-billing/checkout-sessions` | `POST /api/v1/billing/checkout-sessions` | ○ | — | 转发 |
//! | `GET /api/cloud-billing/checkout-sessions/{sessionId}` | `…/checkout-sessions/{id}` | ○ | — | — |
//! | `POST /api/cloud-billing/portal-sessions` | `POST /api/v1/billing/portal-sessions` | ○ | — | **不转发** |
//!
//! 八条**全部**注入 `X-User-ID`（「身份是账号级的」）—— 本地不发送任何其他身份材料，
//! 云侧仍是最终授权方（每次 mutation 前重新校验 membership）。
//!
//! # 本片实作了什么（M9-1）
//!
//! 1. **8 个请求构造函数**（`*_request`）—— 形状由**签名**固定：只有带 query / 带 body 的
//!    路由才有那个实参，写不出"给 `balance` 传个体"这种错；
//! 2. **[`parse_query`]** —— 上游 `r.URL.Query()` 的等价物（`url.Values` 的解析口径）；
//! 3. **[`is_valid_stripe_session_id`]** —— 上游 `isValidStripeSessionID` 的**allowlist**
//!    （`[A-Za-z0-9_]`）。它是把路径参数**直接拼进出站 URL** 之前的唯一护栏：`../` /
//!    `%2f` / `?` 都能重定向这次出站请求。
//!
//! # 为什么本文件**没有**响应类型
//!
//! 上游 `writeCloudRuntimeResponse` 把云侧状态码与体**原样**写回客户端 ⇒ 本地没有响应形状
//! 可建模（建模了就是第二个真相源，且云侧加字段时会静默丢数据）。
//! `crates/mc-core/src/cloud.rs` 的文件头写着同一件事。
//!
//! # 错误映射按 `docs/62` §2.6（由 handler 写状态码，本文件只造请求）
//!
//! `Disabled` ⇒ 403 `cloud_runtime_not_configured`；`InvalidBaseUrl` ⇒ 500
//! `cloud_runtime_misconfigured`；`Timeout` ⇒ 504；`Transport` / `ResponseTooLarge` ⇒ 502。
//! **云侧的 4xx/5xx 不是错误**（原样透传）。
//!
//! 形态纪律（`docs/62` §1.4 实测 `dual-form required: 0`）：8 条**只按上游字面量注册**
//! 那一形态，路径参数写 `:sessionId`（matchit 0.7 把 `{…}` 当字面量 ⇒ 编译通过且恒 404）。

use mc_core::cloud::BILLING_UPSTREAM_PREFIX;
use mc_core::Id;

use crate::transport::Request;

/// 出站计量标签（上游 `inferOp` 对 `/api/v1/billing/*` 推出来的桶名）。
///
/// 显式给出而不是靠路径推导：M9-1 / M9-11 共用同一份 [`crate::transport::Client`]，
/// 标签要在**调用点**看得见（`docs/62` §2.6 的观测面）。
pub const OP: &str = "billing";

// ---------------------------------------------------------------------------
// 云侧路径
// ---------------------------------------------------------------------------

/// 云侧路径的**后缀**（前缀一律取 [`BILLING_UPSTREAM_PREFIX`]）。
///
/// 为什么存后缀而不是全路径：`BILLING_UPSTREAM_PREFIX` 是 `mc-core` 里 pin 住的**唯一**
/// 前缀常量（`crates/mc-core/src/cloud.rs`）—— 这里再写一遍全串就是第二个真相源。
/// `upstream_prefix_is_the_only_source_of_the_billing_paths` 逐条钉住这层关系。
const BALANCE_SUFFIX: &str = "/balance";
const TRANSACTIONS_SUFFIX: &str = "/transactions";
const BATCHES_SUFFIX: &str = "/batches";
const TOPUPS_SUFFIX: &str = "/topups";
const PRICE_TIERS_SUFFIX: &str = "/price-tiers";
const CHECKOUT_SESSIONS_SUFFIX: &str = "/checkout-sessions";
const PORTAL_SESSIONS_SUFFIX: &str = "/portal-sessions";

/// 拼一条出站路径（**唯一**的拼接点）。
#[must_use]
fn upstream_path(suffix: &str) -> String {
    format!("{BILLING_UPSTREAM_PREFIX}{suffix}")
}

/// `GET /api/v1/billing/checkout-sessions/{session_id}` 的出站路径。
///
/// ⚠️ 调用方**必须**先过 [`is_valid_stripe_session_id`]：本函数不做任何校验，它只是拼接
/// （上游 `GetCloudBillingCheckoutSession` 的顺序也是先校验再拼）。
#[must_use]
pub fn checkout_session_path(session_id: &str) -> String {
    format!("{}/{session_id}", upstream_path(CHECKOUT_SESSIONS_SUFFIX))
}

// ---------------------------------------------------------------------------
// 请求构造函数（8 条，一人一个）
// ---------------------------------------------------------------------------

/// 出一个站请求的公共骨架：方法 + 路径 + **身份** + 计量标签（+ 可选 `X-Request-ID`）。
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

/// `GET /api/v1/billing/balance`（上游 `GetCloudBillingBalance`）。
#[must_use]
pub fn balance_request(user_id: Id, request_id: Option<&str>) -> Request {
    proxy_request(
        &reqwest::Method::GET,
        upstream_path(BALANCE_SUFFIX),
        user_id,
        request_id,
    )
}

/// `GET /api/v1/billing/transactions`（上游 `ListCloudBillingTransactions`，`withQuery`）。
#[must_use]
pub fn transactions_request(
    user_id: Id,
    request_id: Option<&str>,
    query: Vec<(String, String)>,
) -> Request {
    proxy_request(
        &reqwest::Method::GET,
        upstream_path(TRANSACTIONS_SUFFIX),
        user_id,
        request_id,
    )
    .with_query(query)
}

/// `GET /api/v1/billing/batches`（上游 `ListCloudBillingBatches`，`withQuery`）。
#[must_use]
pub fn batches_request(
    user_id: Id,
    request_id: Option<&str>,
    query: Vec<(String, String)>,
) -> Request {
    proxy_request(
        &reqwest::Method::GET,
        upstream_path(BATCHES_SUFFIX),
        user_id,
        request_id,
    )
    .with_query(query)
}

/// `GET /api/v1/billing/topups`（上游 `ListCloudBillingTopups`，`withQuery`）。
#[must_use]
pub fn topups_request(
    user_id: Id,
    request_id: Option<&str>,
    query: Vec<(String, String)>,
) -> Request {
    proxy_request(
        &reqwest::Method::GET,
        upstream_path(TOPUPS_SUFFIX),
        user_id,
        request_id,
    )
    .with_query(query)
}

/// `GET /api/v1/billing/price-tiers`（上游 `ListCloudBillingPriceTiers`）。
///
/// 上传逐字注明：价格分层今天对每个属主都一样，但**仍然**要盖章 `X-User-ID`
/// —— 云侧要能审计"谁在列 tier"，且定价将来按客户区分时这条契约不变。
#[must_use]
pub fn price_tiers_request(user_id: Id, request_id: Option<&str>) -> Request {
    proxy_request(
        &reqwest::Method::GET,
        upstream_path(PRICE_TIERS_SUFFIX),
        user_id,
        request_id,
    )
}

/// `POST /api/v1/billing/checkout-sessions`（上游 `CreateCloudBillingCheckoutSession`）。
///
/// 体**逐字**转发（handler 已在读体时做过空体 / JSON 语法 / 1 MiB 三道判定）。
#[must_use]
pub fn checkout_session_create_request(
    user_id: Id,
    request_id: Option<&str>,
    body: Vec<u8>,
) -> Request {
    proxy_request(
        &reqwest::Method::POST,
        upstream_path(CHECKOUT_SESSIONS_SUFFIX),
        user_id,
        request_id,
    )
    .with_body(body)
}

/// `GET /api/v1/billing/checkout-sessions/{session_id}`（上游 `GetCloudBillingCheckoutSession`）。
///
/// 唯一的**动态路径**端点：`session_id` 由 allowlist 校验后拼进 URL。
#[must_use]
pub fn checkout_session_request(
    session_id: &str,
    user_id: Id,
    request_id: Option<&str>,
) -> Request {
    proxy_request(
        &reqwest::Method::GET,
        checkout_session_path(session_id),
        user_id,
        request_id,
    )
}

/// `POST /api/v1/billing/portal-sessions`（上游 `CreateCloudBillingPortalSession`）。
///
/// ⚠️ **不转发体**：上游逐字写明 `withBody` **没有**打开（`cloud_runtime` 的 helper 会拒绝
/// 空体，所以 portal-sessions 干脆不读体、也不往上传体）。客户端发来的体被**忽略**。
#[must_use]
pub fn portal_session_request(user_id: Id, request_id: Option<&str>) -> Request {
    proxy_request(
        &reqwest::Method::POST,
        upstream_path(PORTAL_SESSIONS_SUFFIX),
        user_id,
        request_id,
    )
}

// ---------------------------------------------------------------------------
// 两条纯函数（出站前的判定）
// ---------------------------------------------------------------------------

/// 把本地的**原始**查询串解析成出站查询对（上游 `cloudRuntimeProxyOptions` 的 `withQuery`
/// 分支：`query = r.URL.Query()`）。
///
/// 逐字对齐 Go `net/url` 的解析口径：按 `&` 切、键与值都做百分号解码、`+` 表空格、
/// 空段跳过、**多值保序保量**（`a=1&a=2` ⇒ 两对）。
///
/// 与上游的**两处**已知差异（登记 `docs/32` §46）：
/// ① Go 的 `ParseQuery` 会把 `;` 当分隔符的地方报错并**丢掉**那一段，本函数按 `&` 切
///    （`form_urlencoded` 的口径）；② 上游 `url.Values.Encode()` 会把键**排序**，
///    本函数保持**到达顺序**（`transport` 的 `append_pair` 保序）—— 对单值查询串不可观测。
#[must_use]
pub fn parse_query(raw: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(raw.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

/// 上游 `isValidStripeSessionID`：非空且**只**由 `[A-Za-z0-9_]` 组成。
///
/// 为什么是 allowlist 而不是 denylist（上游注释逐字）：这个值会被**直接拼进**出站 URL 的
/// 路径段，`/`、`?`、`#`、`%`、`.` 中的任何一个都能把请求**重定向**到另一条云侧路径或
/// 让查询串/片段生效。allowlist 让"Stripe 将来放宽 ID 语法"这件事只能通过**显式改规则**
/// 发生，而不会自己溜过去。
///
/// 上游接受比 `cs_<base62>` 更宽的这一集合（覆盖所有 Stripe ID 变体，且不硬编码前缀）。
#[must_use]
pub fn is_valid_stripe_session_id(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> Id {
        Id::parse("11111111-2222-3333-4444-555555555555").expect("uuid")
    }

    /// 八个构造函数**逐条**产出上游那条出站路径（前缀 = `mc-core` 的唯一常量）。
    #[test]
    fn upstream_prefix_is_the_only_source_of_the_billing_paths() {
        let cases: Vec<(String, String)> = vec![
            (
                balance_request(user(), None).path,
                "/api/v1/billing/balance".into(),
            ),
            (
                transactions_request(user(), None, vec![]).path,
                "/api/v1/billing/transactions".into(),
            ),
            (
                batches_request(user(), None, vec![]).path,
                "/api/v1/billing/batches".into(),
            ),
            (
                topups_request(user(), None, vec![]).path,
                "/api/v1/billing/topups".into(),
            ),
            (
                price_tiers_request(user(), None).path,
                "/api/v1/billing/price-tiers".into(),
            ),
            (
                checkout_session_create_request(user(), None, vec![]).path,
                "/api/v1/billing/checkout-sessions".into(),
            ),
            (
                checkout_session_request("cs_test_abc", user(), None).path,
                "/api/v1/billing/checkout-sessions/cs_test_abc".into(),
            ),
            (
                portal_session_request(user(), None).path,
                "/api/v1/billing/portal-sessions".into(),
            ),
        ];
        for (actual, expected) in &cases {
            assert_eq!(actual, expected);
            let suffix = actual
                .strip_prefix(BILLING_UPSTREAM_PREFIX)
                .expect("前缀必须是 mc-core 的那一个常量");
            assert!(suffix.starts_with('/'), "{actual}");
            assert!(!suffix.ends_with('/'), "{actual}");
        }
        // 八个**不同**的出站路径（防复制粘贴漏改）。
        let mut paths: Vec<&String> = cases.iter().map(|(actual, _)| actual).collect();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), 8);
    }

    /// 三个开关的形状由**签名**固定；`X-User-ID` 与计量标签逐条盖章。
    #[test]
    fn every_request_stamps_identity_and_the_billing_op() {
        for request in [
            balance_request(user(), Some("rid-1")),
            transactions_request(user(), Some("rid-1"), vec![]),
            checkout_session_create_request(user(), Some("rid-1"), b"{}".to_vec()),
            portal_session_request(user(), Some("rid-1")),
        ] {
            assert_eq!(request.user_id, Some(user()));
            assert_eq!(request.request_id.as_deref(), Some("rid-1"));
            assert_eq!(request.op.as_deref(), Some(OP));
            assert_eq!(request.headers, Vec::new(), "本面不转发任何调用方头");
        }
        // 没给 request id ⇒ 不盖章（上游 `cloudRuntimeRequestID` 的空值分支）。
        assert_eq!(balance_request(user(), None).request_id, None);
    }

    /// 方法：5 条 GET（其中 1 条带路径参数）+ 2 条 POST；只有 3 条带 query、1 条带体。
    #[test]
    fn methods_and_optional_parts_match_the_eight_upstream_handlers() {
        assert_eq!(balance_request(user(), None).method, reqwest::Method::GET);
        assert_eq!(
            checkout_session_create_request(user(), None, b"{}".to_vec()).method,
            reqwest::Method::POST
        );
        assert_eq!(
            portal_session_request(user(), None).method,
            reqwest::Method::POST
        );

        let list = transactions_request(user(), None, parse_query("page=2&page_size=20&page=3"));
        assert_eq!(
            list.query,
            vec![
                ("page".to_string(), "2".to_string()),
                ("page_size".to_string(), "20".to_string()),
                ("page".to_string(), "3".to_string()),
            ],
            "多值保序保量（上游 url.Values 的 slice 语义）"
        );
        // 不带 query 的路由**结构上**没有 query 可以传（签名即判据）。
        assert!(balance_request(user(), None).query.is_empty());
        assert!(price_tiers_request(user(), None).query.is_empty());
        // 只有一条带体。
        assert_eq!(
            checkout_session_create_request(user(), None, b"{\"tier\":1}".to_vec())
                .body
                .as_deref(),
            Some(&b"{\"tier\":1}"[..])
        );
        assert!(portal_session_request(user(), None).body.is_none());
    }

    /// 体是**原样字节**（不 trim、不重编码）—— 与 stripe 转发同一条纪律。
    #[test]
    fn the_forwarded_body_is_byte_for_byte() {
        let raw = b"  {\"tier_id\":\"t1\"}\n".to_vec();
        let request = checkout_session_create_request(user(), None, raw.clone());
        assert_eq!(request.body, Some(raw));
    }

    /// `Debug` 不暴露查询串的**值**（判据 ①，`docs/62` §2.4）。
    #[test]
    fn request_debug_never_prints_query_values_or_the_base_url() {
        let request = transactions_request(
            user(),
            Some("rid-1"),
            parse_query("cursor=secret-cursor-value"),
        );
        let rendered = format!("{request:?}");
        assert!(!rendered.contains("secret-cursor-value"), "{rendered}");
        assert!(!rendered.contains("https://"), "{rendered}");
        assert!(rendered.contains("query_keys"), "{rendered}");
    }

    /// 上游 `r.URL.Query()` 的解析口径（含 `+` ⇒ 空格、空段、只有键没有值）。
    #[test]
    fn query_parsing_matches_go_url_values() {
        assert_eq!(
            parse_query("page=2&page_size=20"),
            vec![
                ("page".to_string(), "2".to_string()),
                ("page_size".to_string(), "20".to_string()),
            ]
        );
        assert_eq!(
            parse_query("q=a+b%20c"),
            vec![("q".to_string(), "a b c".to_string())]
        );
        assert_eq!(
            parse_query("k"),
            vec![("k".to_string(), String::new())],
            "只有键没有值 ⇒ 值是空串（Go 的同一口径）"
        );
        assert!(parse_query("").is_empty());
        // 百分号解码（`%2F` 解成 `/` —— 它在**值**里是安全的：`transport` 会重新编码它）。
        assert_eq!(
            parse_query("path=%2Fapi%2Fv1"),
            vec![("path".to_string(), "/api/v1".to_string())]
        );
    }

    /// 上游 `isValidStripeSessionID` 的正反例（allowlist）。
    #[test]
    fn stripe_session_id_allowlist_matches_upstream() {
        for ok in ["cs_test_abc", "cs_1", "ABC123", "_", "a", "cs_test_abc_123"] {
            assert!(is_valid_stripe_session_id(ok), "{ok}");
        }
        for bad in [
            "",
            "cs-test",
            "cs.test",
            "cs/test",
            "../balance",
            "cs_1/../foo",
            "cs_1?x=1",
            "cs_1#frag",
            "%2f",
            "cs 1",
            "cs_测试",
        ] {
            assert!(!is_valid_stripe_session_id(bad), "{bad:?}");
        }
    }

    /// 路径参数要拼进**出站**路径（含合法字符集里的长 id）。
    #[test]
    fn checkout_session_path_splices_the_validated_id() {
        assert_eq!(
            checkout_session_path("cs_test_abc"),
            "/api/v1/billing/checkout-sessions/cs_test_abc"
        );
        let long = format!("cs_{}", "A1b2C3d4".repeat(8));
        assert!(is_valid_stripe_session_id(&long));
        assert!(checkout_session_path(&long).ends_with(&format!("/{long}")));
    }
}
